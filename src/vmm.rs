//! The virtual machine monitor: owns the KVM VM, its single vCPU, guest RAM, and
//! the emulated devices, and drives the vCPU run loop by dispatching VM exits.

use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use kvm_bindings::{kvm_interrupt, kvm_userspace_memory_region};
use kvm_ioctls::{Kvm, VcpuExit, VcpuFd, VmFd};

/// Vector used for the deterministic timer interrupt.
const TIMER_VECTOR: u8 = 0x20;

/// KVM_INTERRUPT = _IOW(KVMIO=0xAE, 0x86, struct kvm_interrupt{u32}).
/// kvm-ioctls 0.17 does not wrap this, so we issue it directly.
const KVM_INTERRUPT: libc::c_ulong = 0x4004_ae86;

const KVM_GUESTDBG_ENABLE: u32 = 1;
const KVM_GUESTDBG_SINGLESTEP: u32 = 2;

/// Fast-path: single-step this many instructions before each interrupt target.
/// Must exceed the (non-deterministic) PMU overflow skid so free-run always stops
/// before the target and the single-step phase lands on the exact instruction.
const SS_MARGIN: u64 = 256;

use crate::memory::GuestMemory;
use crate::pv::Pv;
use crate::serial::{Serial, COM1_BASE};
use crate::state::{DeviceState, Snapshot};

/// Why the run loop stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitReason {
    /// Guest executed HLT.
    Halted,
    /// Guest requested shutdown via the paravirtual exit port (M1+).
    Shutdown(u32),
    /// KVM reported a triple fault / shutdown.
    TripleFault,
}

/// Outcome of a bounded step: either the guest terminated, or we hit the exit
/// budget and paused at a clean re-entry boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    Exited(ExitReason),
    Paused,
}

/// The CPU mode the guest is entered in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMode {
    /// 16-bit real mode, flat segments.
    Real,
    /// 64-bit long mode with identity-mapped paging and an IDT.
    Long,
}

/// Everything needed to boot a guest, bundled so run/verify share one shape.
#[derive(Debug, Clone, Copy)]
pub struct BootConfig {
    pub load_addr: u64,
    pub mem_size: usize,
    pub seed: u64,
    pub mode: GuestMode,
    /// Inject a timer interrupt every this many clock ticks. 0 disables.
    pub timer_period: u64,
    /// Use the instruction clock (single-step, count retired instructions) instead
    /// of the exit-count clock. Interrupts then fire at exact instruction offsets.
    pub insn_clock: bool,
    /// With `insn_clock`, use the PMU fast-path: free-run at native speed, using a
    /// perf counter to stop just before each interrupt target, then single-step the
    /// last stretch to land exactly. Needs perf_event_paranoid <= 1.
    pub fast: bool,
}

/// Deterministic timer: injects `TIMER_VECTOR` on a schedule measured in virtual
/// clock ticks (VM exits), never wall-clock, so the interrupt pattern replays.
#[derive(Debug, Clone, Copy)]
pub struct Timer {
    pub period: u64,
    pub next_tick: u64,
    /// A vector scheduled but not yet accepted by the guest (waiting for it to be
    /// interruptible). Part of snapshot state so a branch resumes it correctly.
    pub pending: Option<u8>,
    pub count: u64,
}

impl Timer {
    fn new(period: u64) -> Self {
        Timer {
            period,
            next_tick: period,
            pending: None,
            count: 0,
        }
    }
}

// Guest-physical layout for long mode. All of these live inside guest RAM and
// are set up by the VMM before the guest runs.
const PML4_ADDR: u64 = 0x1000;
const PDPT_ADDR: u64 = 0x2000;
const PD_ADDR: u64 = 0x3000;
const GDT_ADDR: u64 = 0x4000;
pub const IDT_ADDR: u64 = 0x5000;
const LONG_STACK_TOP: u64 = 0x20_0000; // 2 MiB

// Paging / control-register bits.
const PTE_PRESENT: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
const PTE_PS: u64 = 1 << 7; // 2 MiB large page in a PD entry
const CR0_PE: u64 = 1 << 0;
const CR0_PG: u64 = 1 << 31;
const CR4_PAE: u64 = 1 << 5;
const EFER_LME: u64 = 1 << 8;
const EFER_LMA: u64 = 1 << 10;

pub struct Vmm {
    // Held to keep the VM alive for the lifetime of its vCPU; not read directly.
    #[allow(dead_code)]
    pub vm: VmFd,
    pub vcpu: VcpuFd,
    pub mem: GuestMemory,
    pub serial: Serial,
    pub pv: Pv,
    pub timer: Timer,
    /// Retired guest instructions (single-step DEBUG exits). The instruction clock.
    pub insn_count: u64,
    /// When true, `step`/`run` single-step and schedule interrupts by instruction.
    pub insn_clock: bool,
    /// When true (with insn_clock), use the PMU fast-path instead of pure single-step.
    pub fast: bool,
    /// Diagnostic: the active-clock value at each interrupt injection. Not part of
    /// determinism state; used to show interrupts land at exact clock offsets.
    pub injection_log: Vec<u64>,
    _kvm: Kvm,
}

impl Vmm {
    /// Create a VM, seeding the deterministic PRNG with `seed`.
    pub fn with_seed(mem_size: usize, seed: u64) -> io::Result<Self> {
        let kvm = Kvm::new()?;
        let vm = kvm.create_vm()?;

        let mem = GuestMemory::new(mem_size)?;
        let region = kvm_userspace_memory_region {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: mem.size() as u64,
            userspace_addr: mem.host_addr(),
        };
        // SAFETY: region describes our live, stable mapping owned by `mem`.
        unsafe { vm.set_user_memory_region(region)? };

        let vcpu = vm.create_vcpu(0)?;

        Ok(Vmm {
            vm,
            vcpu,
            mem,
            serial: Serial::new(),
            pv: Pv::new(seed),
            timer: Timer::new(0),
            insn_count: 0,
            insn_clock: false,
            fast: false,
            injection_log: Vec::new(),
            _kvm: kvm,
        })
    }

    /// Create a VM, load a flat guest blob, and enter it in the requested mode.
    pub fn boot(guest: &Path, cfg: BootConfig) -> io::Result<Self> {
        let mut vm = Vmm::with_seed(cfg.mem_size, cfg.seed)?;
        vm.timer = Timer::new(cfg.timer_period);
        vm.insn_clock = cfg.insn_clock;
        vm.fast = cfg.fast;
        vm.load_guest(guest, cfg.load_addr)?;
        match cfg.mode {
            GuestMode::Real => vm.setup_real_mode(cfg.load_addr)?,
            GuestMode::Long => vm.setup_long_mode(cfg.load_addr)?,
        }
        Ok(vm)
    }

    /// Load a flat guest blob at `load_addr` in guest RAM.
    pub fn load_guest(&mut self, path: &Path, load_addr: u64) -> io::Result<()> {
        let blob = std::fs::read(path)?;
        self.mem.write(load_addr, &blob);
        Ok(())
    }

    /// Configure the vCPU for 16-bit real-mode execution entering at `entry`.
    pub fn setup_real_mode(&mut self, entry: u64) -> io::Result<()> {
        let mut sregs = self.vcpu.get_sregs()?;
        // Flat real-mode segments: base 0, so linear == offset.
        for seg in [
            &mut sregs.cs,
            &mut sregs.ds,
            &mut sregs.es,
            &mut sregs.fs,
            &mut sregs.gs,
            &mut sregs.ss,
        ] {
            seg.base = 0;
            seg.selector = 0;
            seg.limit = 0xffff;
        }
        self.vcpu.set_sregs(&sregs)?;

        let mut regs = self.vcpu.get_regs()?;
        regs.rip = entry;
        regs.rsp = 0x7000;
        regs.rflags = 0x2; // bit 1 is reserved-and-always-set
        regs.rax = 0;
        regs.rbx = 0;
        self.vcpu.set_regs(&regs)?;
        Ok(())
    }

    /// Configure the vCPU for 64-bit long mode with identity-mapped paging.
    ///
    /// We install a 4-level page table that maps the low 1 GiB with 2 MiB pages,
    /// a 3-entry GDT, and point the IDT register at a (guest-initialized) IDT.
    /// The guest starts already in long mode at `entry`.
    pub fn setup_long_mode(&mut self, entry: u64) -> io::Result<()> {
        // Page tables.
        self.write_u64(PML4_ADDR, PDPT_ADDR | PTE_PRESENT | PTE_RW);
        self.write_u64(PDPT_ADDR, PD_ADDR | PTE_PRESENT | PTE_RW);
        for i in 0..512u64 {
            let entry_val = (i * 0x20_0000) | PTE_PRESENT | PTE_RW | PTE_PS;
            self.write_u64(PD_ADDR + i * 8, entry_val);
        }

        // GDT: null, 64-bit code, data.
        self.write_u64(GDT_ADDR, 0);
        self.write_u64(GDT_ADDR + 8, 0x00AF_9A00_0000_FFFF); // code64
        self.write_u64(GDT_ADDR + 16, 0x00CF_9200_0000_FFFF); // data

        let mut sregs = self.vcpu.get_sregs()?;

        let code = long_segment(0x08, 0xb, true);
        let data = long_segment(0x10, 0x3, false);
        sregs.cs = code;
        sregs.ds = data;
        sregs.es = data;
        sregs.fs = data;
        sregs.gs = data;
        sregs.ss = data;

        sregs.gdt.base = GDT_ADDR;
        sregs.gdt.limit = 3 * 8 - 1;
        sregs.idt.base = IDT_ADDR;
        sregs.idt.limit = 256 * 16 - 1;

        sregs.cr3 = PML4_ADDR;
        sregs.cr4 |= CR4_PAE;
        sregs.cr0 |= CR0_PE | CR0_PG;
        sregs.efer |= EFER_LME | EFER_LMA;
        self.vcpu.set_sregs(&sregs)?;

        let mut regs = self.vcpu.get_regs()?;
        regs.rip = entry;
        regs.rsp = LONG_STACK_TOP;
        regs.rflags = 0x2;
        self.vcpu.set_regs(&regs)?;
        Ok(())
    }

    fn write_u64(&mut self, gpa: u64, val: u64) {
        self.mem.write(gpa, &val.to_le_bytes());
    }

    /// Enable or disable hardware single-stepping of the guest.
    pub fn set_singlestep(&self, on: bool) -> io::Result<()> {
        let mut dbg = kvm_bindings::kvm_guest_debug::default();
        if on {
            dbg.control = KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP;
        }
        self.vcpu.set_guest_debug(&dbg)?;
        Ok(())
    }

    /// Diagnostic: single-step up to `max_steps` and return (exit-label, rip) for
    /// each step, so we can observe how KVM interleaves DEBUG and IO exits.
    pub fn trace_singlestep(&mut self, max_steps: usize) -> io::Result<Vec<(String, u64)>> {
        self.set_singlestep(true)?;
        let mut trace = Vec::new();
        for _ in 0..max_steps {
            let label = match self.vcpu.run()? {
                VcpuExit::Debug(_) => "DEBUG".to_string(),
                VcpuExit::IoOut(port, data) => {
                    if is_com1(port) {
                        self.serial.io_out(port, data);
                    } else if Pv::handles(port) {
                        self.pv.io_out(port, data);
                    }
                    format!("IO_OUT:{port:#x}")
                }
                VcpuExit::IoIn(port, data) => {
                    if is_com1(port) {
                        self.serial.io_in(port, data);
                    } else if Pv::handles(port) {
                        self.pv.io_in(port, data);
                    } else {
                        for b in data.iter_mut() {
                            *b = 0;
                        }
                    }
                    format!("IO_IN:{port:#x}")
                }
                VcpuExit::Hlt => {
                    trace.push(("HLT".to_string(), self.vcpu.get_regs()?.rip));
                    break;
                }
                other => format!("{other:?}"),
            };
            let rip = self.vcpu.get_regs()?.rip;
            let done = self.pv.exit.is_some();
            trace.push((label, rip));
            if done {
                break;
            }
        }
        self.set_singlestep(false)?;
        Ok(trace)
    }

    /// Diagnostic: free-run the guest with a PMU counter armed to raise a signal
    /// every `period` retired guest instructions. Each time the signal interrupts
    /// KVM_RUN, record the counter value. Returns those stop points — used to
    /// confirm the overflow mechanism works and to measure PMU skid.
    pub fn pmu_overflow_probe(&mut self, period: u64, max_stops: usize) -> io::Result<Vec<u64>> {
        crate::pmu::install_overflow_handler();
        let ctr = crate::pmu::PerfCounter::open_guest_overflow(
            crate::pmu::TYPE_HARDWARE,
            crate::pmu::HW_INSTRUCTIONS,
            period,
        )?;
        ctr.reset();
        ctr.enable();

        let mut stops = Vec::new();
        loop {
            match self.vcpu.run() {
                Ok(VcpuExit::IoOut(port, data)) => {
                    if is_com1(port) {
                        self.serial.io_out(port, data);
                    } else if Pv::handles(port) {
                        self.pv.io_out(port, data);
                    }
                }
                Ok(VcpuExit::IoIn(port, data)) => {
                    if is_com1(port) {
                        self.serial.io_in(port, data);
                    } else if Pv::handles(port) {
                        self.pv.io_in(port, data);
                    } else {
                        for b in data.iter_mut() {
                            *b = 0;
                        }
                    }
                }
                Ok(VcpuExit::Hlt) | Ok(VcpuExit::Shutdown) => break,
                Ok(_) => {}
                Err(e) if e.errno() == libc::EINTR => {
                    stops.push(ctr.read()?);
                    if stops.len() >= max_stops {
                        break;
                    }
                }
                Err(e) => return Err(e.into()),
            }
            if self.pv.exit.is_some() {
                break;
            }
        }
        ctr.disable();
        Ok(stops)
    }

    /// Run until the guest halts or requests shutdown. Returns the exit reason.
    pub fn run(&mut self) -> io::Result<ExitReason> {
        match self.step(u64::MAX)? {
            StepOutcome::Exited(r) => Ok(r),
            StepOutcome::Paused => unreachable!("u64::MAX budget cannot pause"),
        }
    }

    /// Run at most `budget` clock ticks, then pause at a clean instruction
    /// boundary. The active clock (exit-count or instruction-count) is selected by
    /// `insn_clock`, so `budget` is in that clock's units. Pausing at a fixed
    /// budget is a reproducible cut point for snapshot/branch experiments.
    pub fn step(&mut self, budget: u64) -> io::Result<StepOutcome> {
        match (self.insn_clock, self.fast) {
            (true, true) => self.step_insn_fast(budget),
            (true, false) => self.step_insn(budget),
            (false, _) => self.step_exits(budget),
        }
    }

    /// Exit-count clock: run guest freely between exits; budget counts VM exits.
    fn step_exits(&mut self, budget: u64) -> io::Result<StepOutcome> {
        let mut handled: u64 = 0;
        loop {
            if handled >= budget {
                // A port IN/OUT is split across two KVM_RUN calls: the first exits
                // to us for the data, the second completes the instruction (loads
                // the register, advances RIP). If we pause between them, that
                // pending completion lives in kernel state we do not snapshot, so a
                // restored VM would re-execute the instruction and diverge. Drain
                // it here so we always pause at a true instruction boundary.
                self.drain_pending_io()?;
                return Ok(StepOutcome::Paused);
            }
            // Every VM exit is one tick of the virtual clock: the guest's sense
            // of time advances with control transfers, never with wall-clock.
            self.pv.clock.advance(1);

            // Deterministic timer: possibly inject a timer interrupt, based only
            // on the virtual clock and the guest's (deterministic) readiness.
            self.service_timer_at(self.pv.clock.now())?;

            if let Some(outcome) = self.dispatch_exit()? {
                return Ok(outcome);
            }
            handled += 1;
        }
    }

    /// Instruction-count clock: single-step the guest, counting retired
    /// instructions (DEBUG exits), and schedule interrupts on exact instruction
    /// offsets. `budget` counts instructions. Slower (one exit per instruction)
    /// but gives skid-free, instruction-precise interrupt timing.
    fn step_insn(&mut self, budget: u64) -> io::Result<StepOutcome> {
        self.set_singlestep(true)?;
        let result = loop {
            if self.insn_count >= budget {
                // We only reach the budget immediately after a DEBUG exit, i.e. at a
                // retired-instruction boundary with no pending IO — a clean pause.
                break StepOutcome::Paused;
            }
            // Schedule/inject based on the instruction clock, then advance the
            // exit clock (so the guest's PV_TIME still ticks) and step once.
            self.service_timer_at(self.insn_count)?;
            self.pv.clock.advance(1);

            match self.dispatch_exit()? {
                Some(outcome) => break outcome,
                None => {}
            }
        };
        self.set_singlestep(false)?;
        Ok(result)
    }

    /// Run the guest once and handle the resulting exit. Returns `Some(outcome)`
    /// if the guest terminated, `None` to keep stepping. A DEBUG exit (one retired
    /// instruction under single-step) increments the instruction clock.
    fn dispatch_exit(&mut self) -> io::Result<Option<StepOutcome>> {
        match self.vcpu.run()? {
            VcpuExit::Debug(_) => {
                self.insn_count += 1;
                Ok(None)
            }
            VcpuExit::IrqWindowOpen => Ok(None),
            VcpuExit::IoOut(port, data) => {
                if is_com1(port) {
                    self.serial.io_out(port, data);
                } else if Pv::handles(port) {
                    self.pv.io_out(port, data);
                    if let Some(code) = self.pv.exit {
                        return Ok(Some(StepOutcome::Exited(ExitReason::Shutdown(code))));
                    }
                }
                Ok(None)
            }
            VcpuExit::IoIn(port, data) => {
                if is_com1(port) {
                    self.serial.io_in(port, data);
                } else if Pv::handles(port) {
                    self.pv.io_in(port, data);
                } else {
                    for b in data.iter_mut() {
                        *b = 0;
                    }
                }
                Ok(None)
            }
            VcpuExit::MmioRead(_addr, data) => {
                for b in data.iter_mut() {
                    *b = 0;
                }
                Ok(None)
            }
            VcpuExit::MmioWrite(_addr, _data) => Ok(None),
            VcpuExit::Hlt => Ok(Some(StepOutcome::Exited(ExitReason::Halted))),
            VcpuExit::Shutdown => Ok(Some(StepOutcome::Exited(ExitReason::TripleFault))),
            other => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("unexpected VM exit: {other:?}"),
            )),
        }
    }

    /// PMU fast-path instruction clock. Free-run at native speed, using a perf
    /// counter armed to stop `SS_MARGIN` instructions before each interrupt target,
    /// then single-step the remainder to land on the exact instruction. Produces
    /// the identical clock as pure single-step (true retired instructions =
    /// PMU-counted instructions + IO exits the PMU misses) but far faster.
    fn step_insn_fast(&mut self, budget: u64) -> io::Result<StepOutcome> {
        use crate::pmu::{PerfCounter, HW_INSTRUCTIONS, TYPE_HARDWARE};
        crate::pmu::install_overflow_handler();

        // A period so large the counter never overflows during a bounded
        // single-step burst; used when landing on a target, not fast-forwarding.
        const NO_OVERFLOW: u64 = u64::MAX / 2;

        // ONE guest-instruction counter, opened once and kept live. Opening a
        // second (or reopening) races with KVM and occasionally fails to attach;
        // instead we change the overflow point in place with set_period.
        let ctr = PerfCounter::open_guest_overflow(TYPE_HARDWARE, HW_INSTRUCTIONS, NO_OVERFLOW)?;
        ctr.reset();
        ctr.enable();
        let mut last_pmu: u64 = 0;
        let mut cur_period = NO_OVERFLOW;
        let mut ss_on = false;

        let result = loop {
            // Fold the PMU delta (non-IO instructions) into the instruction clock.
            let c = ctr.read()?;
            self.insn_count += c - last_pmu;
            last_pmu = c;

            if self.insn_count >= budget {
                // Complete any in-flight IO instruction so we pause at a clean
                // boundary (a restored VM must not re-execute a split IN/OUT).
                if ss_on {
                    self.set_singlestep(false)?;
                    ss_on = false;
                }
                self.drain_pending_io()?;
                break StepOutcome::Paused;
            }

            // Inject if we've reached an interrupt target.
            self.service_timer_at(self.insn_count)?;

            let target = if self.timer.period > 0 {
                self.timer.next_tick.min(budget)
            } else {
                budget
            };
            let remaining = target.saturating_sub(self.insn_count);

            if self.timer.pending.is_some() || remaining <= SS_MARGIN {
                // Precision phase: single-step to land on the exact instruction.
                if !ss_on {
                    self.set_singlestep(true)?;
                    ss_on = true;
                }
                if cur_period != NO_OVERFLOW {
                    ctr.set_period(NO_OVERFLOW)?;
                    cur_period = NO_OVERFLOW;
                }
                if let FastEvent::Exited(o) = self.fast_dispatch()? {
                    break o;
                }
            } else {
                // Fast phase: free-run, stopping ~SS_MARGIN before the target.
                if ss_on {
                    self.set_singlestep(false)?;
                    ss_on = false;
                }
                // Clamp huge distances (e.g. no timer, or the tail after the last
                // interrupt) to NO_OVERFLOW: free-run to the guest's natural exit
                // rather than asking the kernel for an out-of-range sample period.
                let want = (remaining - SS_MARGIN).min(NO_OVERFLOW);
                if cur_period != want {
                    ctr.set_period(want)?;
                    cur_period = want;
                }
                loop {
                    match self.fast_dispatch()? {
                        FastEvent::Continue => {}
                        FastEvent::Overflow => break,
                        FastEvent::Exited(o) => {
                            // A fully-scheduled read is required for the count to be
                            // trustworthy; otherwise retry from a fresh VM.
                            let c2 = ctr
                                .read_scheduled()
                                .map_err(|_| pmu_scheduling_error())?;
                            self.insn_count += c2 - last_pmu;
                            if ss_on {
                                self.set_singlestep(false)?;
                            }
                            return Ok(o);
                        }
                    }
                }
            }
        };

        // Final sync. Require a fully-scheduled read: if the perf event was
        // descheduled off the PMU at any point this run (intermittent hardware
        // condition), the count is unreliable — surface it so the caller retries.
        let c = ctr.read_scheduled().map_err(|_| pmu_scheduling_error())?;
        self.insn_count += c - last_pmu;
        if ss_on {
            self.set_singlestep(false)?;
        }
        Ok(result)
    }

    /// Run the guest once for the fast-path and classify the exit. Unlike
    /// `dispatch_exit`, DEBUG exits are NOT counted here (the PMU counts them);
    /// only IO instructions, which the PMU misses, add to the instruction clock.
    fn fast_dispatch(&mut self) -> io::Result<FastEvent> {
        match self.vcpu.run() {
            Ok(VcpuExit::Debug(_)) | Ok(VcpuExit::IrqWindowOpen) => Ok(FastEvent::Continue),
            Ok(VcpuExit::IoOut(port, data)) => {
                if is_com1(port) {
                    self.serial.io_out(port, data);
                    self.insn_count += 1;
                } else if Pv::handles(port) {
                    self.pv.io_out(port, data);
                    if self.pv.exit.is_some() {
                        // The terminating OUT does not retire; do not count it.
                        return Ok(FastEvent::Exited(StepOutcome::Exited(ExitReason::Shutdown(
                            self.pv.exit.unwrap(),
                        ))));
                    }
                    self.insn_count += 1;
                } else {
                    self.insn_count += 1;
                }
                Ok(FastEvent::Continue)
            }
            Ok(VcpuExit::IoIn(port, data)) => {
                if is_com1(port) {
                    self.serial.io_in(port, data);
                } else if Pv::handles(port) {
                    self.pv.io_in(port, data);
                } else {
                    for b in data.iter_mut() {
                        *b = 0;
                    }
                }
                self.insn_count += 1;
                Ok(FastEvent::Continue)
            }
            Ok(VcpuExit::MmioRead(_addr, data)) => {
                for b in data.iter_mut() {
                    *b = 0;
                }
                self.insn_count += 1;
                Ok(FastEvent::Continue)
            }
            Ok(VcpuExit::MmioWrite(_addr, _data)) => {
                self.insn_count += 1;
                Ok(FastEvent::Continue)
            }
            Ok(VcpuExit::Hlt) => Ok(FastEvent::Exited(StepOutcome::Exited(ExitReason::Halted))),
            Ok(VcpuExit::Shutdown) => {
                Ok(FastEvent::Exited(StepOutcome::Exited(ExitReason::TripleFault)))
            }
            Err(e) if e.errno() == libc::EINTR => Ok(FastEvent::Overflow),
            Ok(other) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("unexpected VM exit: {other:?}"),
            )),
            Err(e) => Err(e.into()),
        }
    }
}

/// Classification of a fast-path VM exit.
enum FastEvent {
    /// Keep going (normal instruction or IO handled).
    Continue,
    /// The perf counter overflowed — stop free-running.
    Overflow,
    /// The guest terminated.
    Exited(StepOutcome),
}

impl Vmm {
    /// Complete any pending userspace-IO instruction without executing further,
    /// leaving the vCPU at a clean instruction boundary. KVM runs the IO
    /// completion callback before honoring `immediate_exit`, so a single
    /// immediate-exit run finalizes the instruction and returns at once (EINTR).
    fn drain_pending_io(&mut self) -> io::Result<()> {
        self.vcpu.set_kvm_immediate_exit(1);
        // Compute an owned result inside the match so the borrowed VcpuExit is
        // dropped before we reset the flag below.
        let outcome = match self.vcpu.run() {
            Ok(VcpuExit::Intr) => Ok(()),
            Err(e) if e.errno() == libc::EINTR => Ok(()),
            Ok(other) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("unexpected exit during IO drain: {other:?}"),
            )),
            Err(e) => Err(e.into()),
        };
        self.vcpu.set_kvm_immediate_exit(0);
        outcome
    }

    /// Capture the complete architectural + device state.
    pub fn snapshot(&self) -> io::Result<Snapshot> {
        let regs = self.vcpu.get_regs()?;
        let sregs = self.vcpu.get_sregs()?;
        let fpu = self.vcpu.get_fpu()?;
        let events = self.vcpu.get_vcpu_events()?;
        Ok(Snapshot {
            regs,
            sregs,
            fpu,
            events,
            mem: self.mem.as_slice().to_vec(),
            device: DeviceState {
                serial_out: self.serial.out.clone(),
                prng_state: self.pv.prng.state(),
                clock_ticks: self.pv.clock.now(),
                insn_count: self.insn_count,
                checkpoints: self.pv.checkpoints.clone(),
                exit: self.pv.exit,
                timer_period: self.timer.period,
                timer_next: self.timer.next_tick,
                timer_pending: self.timer.pending,
                timer_count: self.timer.count,
            },
        })
    }

    /// Restore a previously captured state into this VM.
    pub fn restore(&mut self, snap: &Snapshot) -> io::Result<()> {
        self.vcpu.set_regs(&snap.regs)?;
        self.vcpu.set_sregs(&snap.sregs)?;
        self.vcpu.set_fpu(&snap.fpu)?;
        self.vcpu.set_vcpu_events(&snap.events)?;
        assert_eq!(snap.mem.len(), self.mem.size(), "snapshot RAM size mismatch");
        self.mem.as_mut_slice().copy_from_slice(&snap.mem);
        self.serial.out = snap.device.serial_out.clone();
        self.pv.prng.set_state(snap.device.prng_state);
        self.pv.clock.set(snap.device.clock_ticks);
        self.insn_count = snap.device.insn_count;
        self.pv.checkpoints = snap.device.checkpoints.clone();
        self.pv.exit = snap.device.exit;
        self.timer.period = snap.device.timer_period;
        self.timer.next_tick = snap.device.timer_next;
        self.timer.pending = snap.device.timer_pending;
        self.timer.count = snap.device.timer_count;
        Ok(())
    }

    /// Schedule and deliver the deterministic timer interrupt. `now` is the
    /// current value of the active clock (exit-count or instruction-count). The
    /// decision to inject depends only on `now` and the guest's readiness (both
    /// deterministic), so the interrupt pattern replays.
    fn service_timer_at(&mut self, now: u64) -> io::Result<()> {
        if self.timer.period == 0 {
            return Ok(());
        }
        if self.timer.pending.is_none() && now >= self.timer.next_tick {
            self.timer.pending = Some(TIMER_VECTOR);
            self.timer.next_tick = self.timer.next_tick.wrapping_add(self.timer.period);
        }
        match self.timer.pending {
            Some(vec) => {
                let ready = self.vcpu.get_kvm_run().ready_for_interrupt_injection != 0;
                if ready {
                    self.inject_interrupt(vec)?;
                    self.timer.pending = None;
                    self.timer.count += 1;
                    self.injection_log.push(now);
                    self.vcpu.get_kvm_run().request_interrupt_window = 0;
                } else {
                    // Ask KVM to exit as soon as the guest can take an interrupt.
                    self.vcpu.get_kvm_run().request_interrupt_window = 1;
                }
            }
            None => {
                self.vcpu.get_kvm_run().request_interrupt_window = 0;
            }
        }
        Ok(())
    }

    /// Inject a hardware interrupt vector via KVM_INTERRUPT (legacy path; valid
    /// because we run without an in-kernel IRQ chip).
    fn inject_interrupt(&self, vector: u8) -> io::Result<()> {
        let irq = kvm_interrupt {
            irq: vector as u32,
        };
        // SAFETY: fd is a valid vCPU fd; irq points to a live kvm_interrupt.
        let ret = unsafe {
            libc::ioctl(
                self.vcpu.as_raw_fd(),
                KVM_INTERRUPT,
                &irq as *const kvm_interrupt,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn is_com1(port: u16) -> bool {
    (COM1_BASE..COM1_BASE + 8).contains(&port)
}

const PMU_FAIL_MSG: &str = "pmu counter failed to schedule";

fn pmu_scheduling_error() -> io::Error {
    io::Error::new(io::ErrorKind::Other, PMU_FAIL_MSG)
}

/// Build a long-mode segment descriptor for the KVM cached-segment fields.
/// `code` selects the 64-bit L bit (set for code, clear for data).
fn long_segment(selector: u16, type_: u8, code: bool) -> kvm_bindings::kvm_segment {
    kvm_bindings::kvm_segment {
        base: 0,
        limit: 0xffff_ffff,
        selector,
        type_,
        present: 1,
        dpl: 0,
        db: if code { 0 } else { 1 },
        s: 1,
        l: if code { 1 } else { 0 },
        g: 1,
        avl: 0,
        unusable: 0,
        padding: 0,
    }
}
