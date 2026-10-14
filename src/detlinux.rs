//! Deterministic Linux boot + divergence-measurement harness.
//!
//! This is a NEW code path, separate from the non-deterministic `boot-linux`
//! (`linux.rs`). It boots a real x86-64 kernel under hydo's instruction clock
//! (PMU fast-path for speed, single-step for exact landing) with tunable
//! determinism knobs, and measures how deep into boot two independent boots stay
//! bit-identical.
//!
//! The engine is the same one the paravirtual guests use — a guest-only retired-
//! instruction PMU counter free-runs the guest, stopping `SS_MARGIN` before each
//! checkpoint, then single-steps to land on the exact instruction. Because the
//! landing is exact, two boots can be compared at identical instruction budgets.
//!
//! Determinism knobs (see `DetConfig`), applied in increasing difficulty:
//!   1. mask RDRAND / RDSEED CPUID bits (entropy)
//!   2. drop the KVM paravirt-clock CPUID leaf (so the guest uses native TSC, not
//!      the host-time-driven kvm-clock shared page)
//!   3. reset the guest TSC to 0 at boot (controlled offset; the rate is still
//!      host wall-clock — KVM does not let userspace trap every RDTSC)
//!
//! What is NOT tamed here (measured and documented, not faked): the guest TSC
//! *rate* (RDTSC is wall-clock between two guest-mode intervals) and the in-kernel
//! LAPIC timer (fires on host time). The harness's job is to *find and prove*
//! exactly where these bite.

use std::io;
use std::os::unix::io::AsRawFd;

use kvm_bindings::{
    kvm_guest_debug, kvm_msr_entry, kvm_userspace_memory_region, Msrs, KVM_MAX_CPUID_ENTRIES,
};
use kvm_ioctls::{Kvm, VcpuExit, VcpuFd, VmFd};

use crate::boot::{self, LinuxBootOpts};
use crate::memory::GuestMemory;
use crate::pmu::{self, PerfCounter, HW_INSTRUCTIONS, TYPE_HARDWARE};
use crate::uart::Uart;

const KVM_GUESTDBG_ENABLE: u32 = 1;
const KVM_GUESTDBG_SINGLESTEP: u32 = 2;

/// Single-step this many instructions before each target to absorb PMU skid.
///
/// This must exceed the worst-case overflow skid so the free-run phase always
/// stops BEFORE the budget and the exact landing is done by single-step. Linux
/// decompression runs long `rep` string instructions; KVM's PMU overflow is only
/// delivered at a `rep` iteration boundary, and the skid there can be far larger
/// than for scalar code — so we use a generous margin. If the free-run overshoots
/// the budget the landing count jitters run-to-run (observed ~±4), which would
/// masquerade as a divergence, so this margin is a correctness knob, not just
/// performance. Single-stepping a few thousand extra instructions per checkpoint
/// is cheap.
const SS_MARGIN: u64 = 65536;
/// A sample period so large the counter never overflows during a landing burst.
const NO_OVERFLOW: u64 = u64::MAX / 2;

// Scratch pages KVM keeps internally for the Intel identity map + TSS.
const KVM_TSS_ADDRESS: u64 = 0xfffb_d000;
const KVM_IDENTITY_MAP_ADDRESS: u64 = 0xfffb_c000;

// KVM_TRANSLATE = _IOWR(KVMIO=0xAE, 0x85, struct kvm_translation[24 bytes]).
const KVM_TRANSLATE: libc::c_ulong = 0xc018_ae85;

#[repr(C)]
#[derive(Default)]
struct KvmTranslation {
    linear_address: u64,
    physical_address: u64,
    valid: u8,
    writeable: u8,
    usable: u8,
    _pad: [u8; 5],
}

/// The determinism knobs, chosen so we can knock down nondeterminism sources one
/// at a time and re-measure. Higher levels include all lower-level fixes.
#[derive(Debug, Clone, Copy)]
pub struct DetConfig {
    /// Mask CPUID.01H:ECX.RDRAND[30] and CPUID.07H:EBX.RDSEED[18].
    pub mask_entropy: bool,
    /// Clear the kvm-clock feature bits in CPUID leaf 0x40000001 so the guest
    /// falls back to native TSC instead of the host-time-driven kvm-clock page.
    pub drop_kvmclock: bool,
    /// Write IA32_TSC = 0 just before entry (controlled, reproducible offset).
    pub reset_tsc: bool,
}

impl DetConfig {
    /// Named levels for the "knock down one source at a time" progression.
    pub fn level(n: u32) -> Self {
        DetConfig {
            mask_entropy: n >= 1,
            drop_kvmclock: n >= 2,
            reset_tsc: n >= 3,
        }
    }
}

/// Why a bounded step stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStop {
    /// Reached the instruction budget.
    Budget,
    /// Guest executed HLT (idle / end of reachable deterministic boot).
    Hlt,
    /// Guest triple-faulted.
    Shutdown,
}

/// A deterministic-mode Linux VM. In-kernel irqchip (so the LAPIC/PIC/IOAPIC exist
/// and the kernel does not crash on APIC access) but NO PIT (no host-wall-clock
/// legacy timer ticks), host CPUID with the entropy/clock bits optionally masked,
/// and no wall-clock wakeups or stdin (deterministic by construction).
pub struct DetLinuxVm {
    #[allow(dead_code)]
    vm: VmFd,
    vcpu: VcpuFd,
    mem: GuestMemory,
    uart: Uart,
    /// Retired guest instructions — the instruction clock.
    pub insn_count: u64,
    /// Per-VM port-0x61 refresh state (must NOT be a process global: two boots run
    /// in the same process and would otherwise share it and diverge spuriously).
    p61: u8,
    #[allow(dead_code)]
    kvm: Kvm,
}

impl DetLinuxVm {
    /// Build and enter a deterministic-mode Linux VM ready to single-step / fast-run.
    pub fn boot(opts: &LinuxBootOpts, det: DetConfig) -> io::Result<Self> {
        let kvm = Kvm::new()?;
        let vm = kvm.create_vm()?;

        vm.set_identity_map_address(KVM_IDENTITY_MAP_ADDRESS)?;
        vm.set_tss_address(KVM_TSS_ADDRESS as usize)?;

        // In-kernel PIC/IOAPIC/LAPIC, but deliberately NO PIT: the PIT would inject
        // timer interrupts on the host wall clock, an immediate nondeterminism
        // source. The kernel still finds the LAPIC/IOAPIC via the MP table.
        vm.create_irq_chip()?;

        let mem = GuestMemory::new(opts.mem_size)?;
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

        // Host CPUID with the requested bits masked.
        let mut cpuid = kvm.get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)?;
        for e in cpuid.as_mut_slice() {
            match e.function {
                0x1 if det.mask_entropy => {
                    e.ecx &= !(1 << 30); // RDRAND
                }
                0x7 if det.mask_entropy && e.index == 0 => {
                    e.ebx &= !(1 << 18); // RDSEED
                }
                0x4000_0001 if det.drop_kvmclock => {
                    // Clear KVM_FEATURE_CLOCKSOURCE(0), CLOCKSOURCE2(3),
                    // CLOCKSOURCE_STABLE_BIT(24) so the guest will not enable
                    // kvm-clock (whose shared page KVM stamps with host time).
                    e.eax &= !((1 << 0) | (1 << 3) | (1 << 24));
                }
                _ => {}
            }
        }
        vcpu.set_cpuid2(&cpuid)?;

        let mut me = DetLinuxVm {
            vm,
            vcpu,
            mem,
            uart: Uart::new(),
            insn_count: 0,
            p61: 0,
            kvm,
        };

        let info = boot::load_linux(&mut me.mem, opts)?;
        me.setup_long_mode(info.entry, info.boot_params)?;

        if det.reset_tsc {
            // Controlled TSC: same starting offset each boot. (The advance *rate*
            // is still host wall-clock; KVM offers no per-RDTSC trap to userspace.)
            let entries = [kvm_msr_entry {
                index: 0x10, // IA32_TSC
                data: 0,
                ..Default::default()
            }];
            if let Ok(msrs) = Msrs::from_entries(&entries) {
                let _ = me.vcpu.set_msrs(&msrs);
            }
        }

        Ok(me)
    }

    fn setup_long_mode(&mut self, entry: u64, boot_params: u64) -> io::Result<()> {
        const PML4_ADDR: u64 = 0x1000;
        const PDPT_ADDR: u64 = 0x2000;
        const PD_ADDR: u64 = 0x3000;
        const GDT_ADDR: u64 = 0x4000;
        const PTE_PRESENT: u64 = 1 << 0;
        const PTE_RW: u64 = 1 << 1;
        const PTE_PS: u64 = 1 << 7;
        const CR0_PE: u64 = 1 << 0;
        const CR0_PG: u64 = 1 << 31;
        const CR4_PAE: u64 = 1 << 5;
        const EFER_LME: u64 = 1 << 8;
        const EFER_LMA: u64 = 1 << 10;

        self.write_u64(PML4_ADDR, PDPT_ADDR | PTE_PRESENT | PTE_RW);
        self.write_u64(PDPT_ADDR, PD_ADDR | PTE_PRESENT | PTE_RW);
        for i in 0..512u64 {
            self.write_u64(PD_ADDR + i * 8, (i * 0x20_0000) | PTE_PRESENT | PTE_RW | PTE_PS);
        }
        self.write_u64(GDT_ADDR, 0);
        self.write_u64(GDT_ADDR + 8, 0x00AF_9A00_0000_FFFF);
        self.write_u64(GDT_ADDR + 16, 0x00CF_9200_0000_FFFF);

        let mut sregs = self.vcpu.get_sregs()?;
        sregs.cs = seg(0x08, 0xb, true);
        let d = seg(0x10, 0x3, false);
        sregs.ds = d;
        sregs.es = d;
        sregs.fs = d;
        sregs.gs = d;
        sregs.ss = d;
        sregs.gdt.base = GDT_ADDR;
        sregs.gdt.limit = 3 * 8 - 1;
        sregs.cr3 = PML4_ADDR;
        sregs.cr4 |= CR4_PAE;
        sregs.cr0 |= CR0_PE | CR0_PG;
        sregs.efer |= EFER_LME | EFER_LMA;
        self.vcpu.set_sregs(&sregs)?;

        let mut regs = self.vcpu.get_regs()?;
        regs.rip = entry;
        regs.rsi = boot_params;
        regs.rsp = 0x8000;
        regs.rflags = 0x2;
        self.vcpu.set_regs(&regs)?;
        Ok(())
    }

    fn write_u64(&mut self, gpa: u64, val: u64) {
        self.mem.write(gpa, &val.to_le_bytes());
    }

    fn set_singlestep(&self, on: bool) -> io::Result<()> {
        let mut dbg = kvm_guest_debug::default();
        if on {
            dbg.control = KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP;
        }
        self.vcpu.set_guest_debug(&dbg)?;
        Ok(())
    }

    /// One single-stepped guest instruction (or a device exit). Increments the
    /// instruction clock on each DEBUG (retired instruction). Returns a terminal
    /// stop if the guest halted / faulted.
    fn ss_dispatch(&mut self) -> io::Result<Option<StepStop>> {
        match self.vcpu.run()? {
            VcpuExit::Debug(_) => {
                self.insn_count += 1;
                Ok(None)
            }
            VcpuExit::IrqWindowOpen => Ok(None),
            VcpuExit::IoOut(port, data) => {
                if Uart::handles(port) {
                    self.uart.io_out(port, data);
                }
                Ok(None)
            }
            VcpuExit::IoIn(port, data) => {
                if Uart::handles(port) {
                    self.uart.io_in(port, data);
                } else {
                    port_in_default(&mut self.p61, port, data);
                }
                Ok(None)
            }
            VcpuExit::MmioRead(_addr, data) => {
                for b in data.iter_mut() {
                    *b = 0;
                }
                Ok(None)
            }
            VcpuExit::MmioWrite(_, _) => Ok(None),
            VcpuExit::Hlt => Ok(Some(StepStop::Hlt)),
            VcpuExit::Shutdown => Ok(Some(StepStop::Shutdown)),
            other => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("unexpected VM exit: {other:?}"),
            )),
        }
    }

    /// Fast-path dispatch: like `ss_dispatch` but DEBUG exits are NOT counted here
    /// (the PMU counts them); only IO/MMIO exits (which the PMU misses) tick.
    fn fast_dispatch(&mut self) -> io::Result<Fast> {
        match self.vcpu.run() {
            Ok(VcpuExit::Debug(_)) | Ok(VcpuExit::IrqWindowOpen) => Ok(Fast::Continue),
            Ok(VcpuExit::IoOut(port, data)) => {
                if Uart::handles(port) {
                    self.uart.io_out(port, data);
                }
                self.insn_count += 1;
                Ok(Fast::Continue)
            }
            Ok(VcpuExit::IoIn(port, data)) => {
                if Uart::handles(port) {
                    self.uart.io_in(port, data);
                } else {
                    port_in_default(&mut self.p61, port, data);
                }
                self.insn_count += 1;
                Ok(Fast::Continue)
            }
            Ok(VcpuExit::MmioRead(_addr, data)) => {
                for b in data.iter_mut() {
                    *b = 0;
                }
                self.insn_count += 1;
                Ok(Fast::Continue)
            }
            Ok(VcpuExit::MmioWrite(_, _)) => {
                self.insn_count += 1;
                Ok(Fast::Continue)
            }
            Ok(VcpuExit::Hlt) => Ok(Fast::Exited(StepStop::Hlt)),
            Ok(VcpuExit::Shutdown) => Ok(Fast::Exited(StepStop::Shutdown)),
            Err(e) if e.errno() == libc::EINTR => Ok(Fast::Overflow),
            Ok(other) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("unexpected VM exit: {other:?}"),
            )),
            Err(e) => Err(e.into()),
        }
    }

    /// Enable/disable hardware single-step (public for the lockstep bisection).
    pub fn singlestep(&self, on: bool) -> io::Result<()> {
        self.set_singlestep(on)
    }

    /// Advance exactly one retired instruction. Assumes single-step is already
    /// enabled by the caller (kept on across a bisection to avoid per-instruction
    /// KVM_SET_GUEST_DEBUG churn). Returns a terminal stop if the guest halted.
    pub fn step_one(&mut self) -> io::Result<Option<StepStop>> {
        let start = self.insn_count;
        loop {
            if let Some(s) = self.ss_dispatch()? {
                return Ok(Some(s));
            }
            if self.insn_count > start {
                return Ok(None);
            }
        }
    }

    /// Complete any in-flight userspace-IO instruction so we pause on a clean
    /// instruction boundary (mirrors the engine's drain).
    fn drain_pending_io(&mut self) -> io::Result<()> {
        self.vcpu.set_kvm_immediate_exit(1);
        let outcome = match self.vcpu.run() {
            Ok(VcpuExit::Intr) => Ok(()),
            Err(e) if e.errno() == libc::EINTR => Ok(()),
            Ok(_other) => Ok(()),
            Err(e) => Err(e.into()),
        };
        self.vcpu.set_kvm_immediate_exit(0);
        outcome
    }

    /// Free-run (PMU) to `budget`, landing on the exact instruction via single-step
    /// for the final SS_MARGIN. Uses the already-open counter; returns Some(stop)
    /// if the guest terminated before the budget.
    fn advance_fast(
        &mut self,
        ctr: &PerfCounter,
        last_pmu: &mut u64,
        cur_period: &mut u64,
        budget: u64,
    ) -> io::Result<Option<StepStop>> {
        let mut ss_on = false;
        let result = loop {
            // read_scheduled (not read): if the PMU event was EVER descheduled off
            // the fixed counter this run, the count is untrustworthy — abort so the
            // caller retries from a fresh VM. Using plain read() here silently
            // lands at a wrong instruction count and manufactures false divergences.
            let c = ctr.read_scheduled().map_err(|_| pmu_sched_err())?;
            self.insn_count += c - *last_pmu;
            *last_pmu = c;

            if self.insn_count >= budget {
                if ss_on {
                    self.set_singlestep(false)?;
                }
                self.drain_pending_io()?;
                break None;
            }

            let remaining = budget - self.insn_count;
            if remaining <= SS_MARGIN {
                if !ss_on {
                    self.set_singlestep(true)?;
                    ss_on = true;
                }
                if *cur_period != NO_OVERFLOW {
                    ctr.set_period(NO_OVERFLOW)?;
                    *cur_period = NO_OVERFLOW;
                }
                if let Fast::Exited(s) = self.fast_dispatch()? {
                    // Fold the final PMU delta and stop.
                    let c2 = ctr.read_scheduled().map_err(|_| pmu_sched_err())?;
                    self.insn_count += c2 - *last_pmu;
                    *last_pmu = c2;
                    if ss_on {
                        self.set_singlestep(false)?;
                    }
                    break Some(s);
                }
            } else {
                if ss_on {
                    self.set_singlestep(false)?;
                    ss_on = false;
                }
                let want = (remaining - SS_MARGIN).min(NO_OVERFLOW);
                if *cur_period != want {
                    ctr.set_period(want)?;
                    *cur_period = want;
                }
                loop {
                    match self.fast_dispatch()? {
                        Fast::Continue => {}
                        Fast::Overflow => break,
                        Fast::Exited(s) => {
                            // ss_on is false in the free-run branch, so no
                            // single-step teardown is needed here.
                            let c2 = ctr.read_scheduled().map_err(|_| pmu_sched_err())?;
                            self.insn_count += c2 - *last_pmu;
                            *last_pmu = c2;
                            return Ok(Some(s));
                        }
                    }
                }
            }
        };
        Ok(result)
    }

    /// Fast-forward to an exact instruction budget (one open counter for the whole
    /// call). Used to re-position a fresh VM at the last-matching checkpoint before
    /// the fine single-step bisection.
    pub fn fast_forward(&mut self, budget: u64) -> io::Result<StepStop> {
        pmu::install_overflow_handler();
        let ctr = PerfCounter::open_guest_overflow(TYPE_HARDWARE, HW_INSTRUCTIONS, NO_OVERFLOW)?;
        ctr.reset();
        ctr.enable();
        let mut last_pmu = 0u64;
        let mut cur_period = NO_OVERFLOW;
        let stop = self.advance_fast(&ctr, &mut last_pmu, &mut cur_period, budget)?;
        ctr.disable();
        Ok(stop.unwrap_or(StepStop::Budget))
    }

    // --- state capture ---

    /// Cheap fingerprint: architectural registers + serial output (no RAM scan).
    /// Catches register-visible divergence (RDTSC/RDRAND/RDMSR results) at once.
    pub fn reg_fingerprint(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(b"detlinux-regs-v1");
        if let Ok(r) = self.vcpu.get_regs() {
            for v in [
                r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rsp, r.rbp, r.r8, r.r9, r.r10, r.r11,
                r.r12, r.r13, r.r14, r.r15, r.rip, r.rflags,
            ] {
                h.update(&v.to_le_bytes());
            }
        }
        if let Ok(s) = self.vcpu.get_sregs() {
            for v in [s.cr0, s.cr2, s.cr3, s.cr4, s.efer] {
                h.update(&v.to_le_bytes());
            }
            h.update(&s.cs.base.to_le_bytes());
            h.update(&s.cs.selector.to_le_bytes());
        }
        h.update(&(self.uart.out.len() as u64).to_le_bytes());
        h.update(&self.uart.out);
        *h.finalize().as_bytes()
    }

    /// Full fingerprint: registers + serial + BLAKE3 of ALL guest RAM.
    pub fn full_fingerprint(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(b"detlinux-full-v1");
        h.update(&self.reg_fingerprint());
        h.update(&(self.mem.size() as u64).to_le_bytes());
        h.update(self.mem.as_slice());
        *h.finalize().as_bytes()
    }

    pub fn regs(&self) -> io::Result<kvm_bindings::kvm_regs> {
        Ok(self.vcpu.get_regs()?)
    }

    /// (apic_base, interrupt.injected, exception.injected/pending) — to show an
    /// asynchronous interrupt or fault is in flight at a divergence.
    pub fn irq_state(&self) -> (u64, u8, u8, u8) {
        let apic = self.vcpu.get_sregs().map(|s| s.apic_base).unwrap_or(0);
        let e = self.vcpu.get_vcpu_events().unwrap_or_default();
        (apic, e.interrupt.injected, e.exception.injected, e.exception.nr)
    }

    pub fn serial(&self) -> &[u8] {
        &self.uart.out
    }

    /// Translate a guest-virtual address to guest-physical via KVM_TRANSLATE (walks
    /// the guest's current CR3), returning None if unmapped.
    pub fn translate(&self, gva: u64) -> Option<u64> {
        let mut t = KvmTranslation {
            linear_address: gva,
            ..Default::default()
        };
        // SAFETY: fd is a valid vCPU fd; t is a live kvm_translation.
        let ret = unsafe {
            libc::ioctl(
                self.vcpu.as_raw_fd(),
                KVM_TRANSLATE,
                &mut t as *mut KvmTranslation,
            )
        };
        if ret < 0 || t.valid == 0 {
            None
        } else {
            Some(t.physical_address)
        }
    }

    /// Read up to `len` bytes of guest code around the current RIP (translating the
    /// virtual RIP through the guest page tables). Returns (phys_addr, bytes).
    pub fn read_code_at_rip(&self, rip: u64, before: u64, len: usize) -> Option<(u64, Vec<u8>)> {
        let start_gva = rip.saturating_sub(before);
        let phys = self.translate(start_gva)?;
        let start = phys as usize;
        let end = (start + len).min(self.mem.size());
        if start >= self.mem.size() {
            return None;
        }
        Some((start_gva, self.mem.as_slice()[start..end].to_vec()))
    }
}

/// Fast-path exit classification.
enum Fast {
    Continue,
    Overflow,
    Exited(StepStop),
}

/// Deterministic reads for ports we do not model. 0x61 toggles its refresh bit as
/// a function of read count; `p61` is per-VM (reset to 0 at boot) so two identical
/// boots read the identical sequence.
fn port_in_default(p61: &mut u8, port: u16, data: &mut [u8]) {
    let val = match port {
        0x61 => {
            *p61 = p61.wrapping_add(0x10);
            *p61 & 0x30
        }
        _ => 0xff,
    };
    if let Some(b) = data.first_mut() {
        *b = val;
    }
    for b in data.iter_mut().skip(1) {
        *b = 0;
    }
}

fn seg(selector: u16, type_: u8, code: bool) -> kvm_bindings::kvm_segment {
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

const PMU_FAIL_MSG: &str = "pmu counter failed to schedule";
fn pmu_sched_err() -> io::Error {
    io::Error::new(io::ErrorKind::Other, PMU_FAIL_MSG)
}

// ------------------------------------------------------------------------------
// Divergence-measurement driver
// ------------------------------------------------------------------------------

/// Diagnostic: single-step ONE boot from entry, printing (step, rip, phys) every
/// `print_every` steps, until `max_steps` or termination. Reveals how the
/// instruction-boundary clock (KVM single-step coalesces each `rep` into one step)
/// traverses the boot, and how many boundaries reach each phase.
pub fn boot_step_debug(
    opts: &LinuxBootOpts,
    det: DetConfig,
    max_steps: u64,
    print_every: u64,
) -> io::Result<()> {
    use std::io::Write as _;
    let mut vm = DetLinuxVm::boot(opts, det)?;
    vm.singlestep(true)?;
    let t0 = std::time::Instant::now();
    for step in 0..max_steps {
        if step % print_every == 0 {
            let rip = vm.regs()?.rip;
            let phys = vm.translate(rip);
            eprintln!(
                "  step {step:>9}  insn={:>10}  rip={rip:#018x}  phys={:?}  serial={}B  {:.2}s",
                vm.insn_count,
                phys.map(|p| format!("{p:#x}")),
                vm.serial().len(),
                t0.elapsed().as_secs_f64(),
            );
            let _ = std::io::stderr().flush();
        }
        if let Some(stop) = vm.step_one()? {
            eprintln!("  step {step}: terminal {stop:?}  insn={}", vm.insn_count);
            break;
        }
    }
    vm.singlestep(false)?;
    eprintln!(
        "[hydo] final: {} steps region, serial {}B",
        max_steps,
        vm.serial().len()
    );
    let s = vm.serial();
    eprintln!(
        "[hydo] serial tail:\n----\n{}\n----",
        String::from_utf8_lossy(&s[s.len().saturating_sub(300)..]).trim_end()
    );
    Ok(())
}

/// Diagnostic: fast-forward `reps` fresh boots to exactly `budget` retired
/// instructions and report the landed instruction count + register fingerprint of
/// each. If landing is reproducible (the whole point), every row is identical.
pub fn boot_probe(opts: &LinuxBootOpts, det: DetConfig, budget: u64, reps: u32) -> io::Result<()> {
    eprintln!("[hydo] boot-probe: landing {reps} fresh boots at budget {budget} insns");
    for i in 0..reps {
        let (fp, landed, rip) = with_retry(|| {
            let mut vm = DetLinuxVm::boot(opts, det)?;
            let _ = vm.fast_forward(budget)?;
            Ok((vm.reg_fingerprint(), vm.insn_count, vm.regs()?.rip))
        })?;
        eprintln!(
            "  rep {i}: landed_insn={landed:>12}  rip={rip:#018x}  reg_fp={}",
            &hex(&fp)[..24]
        );
    }
    Ok(())
}

/// Retry a closure that may hit the intermittent PMU-scheduling failure.
fn with_retry<T>(mut f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    const MAX: usize = 12;
    for attempt in 1..=MAX {
        match f() {
            Err(e) if e.to_string().contains(PMU_FAIL_MSG) => {
                eprintln!("[hydo] PMU did not schedule (attempt {attempt}); retrying");
            }
            other => return other,
        }
    }
    Err(pmu_sched_err())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Run the full divergence measurement: boot twice, find the first differing
/// checkpoint, then single-step-bisect to the exact instruction, and diagnose.
pub fn boot_verify(
    opts: &LinuxBootOpts,
    det: DetConfig,
    interval: u64,
    max_insns: u64,
    with_ram: bool,
) -> io::Result<()> {
    eprintln!(
        "[hydo] boot-verify: mem={}MiB  interval={}M insns  max={}M insns  ram_hash={}",
        opts.mem_size / (1024 * 1024),
        interval / 1_000_000,
        max_insns / 1_000_000,
        with_ram
    );
    eprintln!(
        "[hydo] determinism: mask_entropy={} drop_kvmclock={} reset_tsc={}",
        det.mask_entropy, det.drop_kvmclock, det.reset_tsc
    );
    eprintln!("[hydo] cmdline: {}", opts.cmdline);

    // --- context boot: one clean run to `max_insns` (or the guest's own terminal)
    //     so we can show how far the boot progresses and its console tail ---
    let t0 = std::time::Instant::now();
    let (serial, ctx_insn, ctx_stop) = with_retry(|| {
        let mut vm = DetLinuxVm::boot(opts, det)?;
        let stop = vm.fast_forward(max_insns)?;
        Ok((vm.serial().to_vec(), vm.insn_count, stop))
    })?;
    eprintln!(
        "[hydo] context boot: {} retired insns, terminal={:?}, {} serial bytes, {:.1}s",
        ctx_insn,
        ctx_stop,
        serial.len(),
        t0.elapsed().as_secs_f64()
    );
    eprintln!(
        "[hydo] serial tail:\n----\n{}\n----",
        String::from_utf8_lossy(&serial[serial.len().saturating_sub(360)..]).trim_end()
    );

    // A coarse probe: land a FRESH boot (fresh PMU counter) at `budget` and return
    // its fingerprint + landed count + whether it terminated. Fresh counter per
    // probe makes this immune to the mid-run PMU descheduling that silently
    // corrupts a single long-lived counter (which produced false early divergences).
    let probe = |budget: u64| -> io::Result<([u8; 32], u64, bool)> {
        with_retry(|| {
            let mut vm = DetLinuxVm::boot(opts, det)?;
            let stop = vm.fast_forward(budget)?;
            let fp = if with_ram {
                vm.full_fingerprint()
            } else {
                vm.reg_fingerprint()
            };
            Ok((fp, vm.insn_count, stop != StepStop::Budget))
        })
    };

    // --- probe-based coarse search: walk checkpoints, comparing two fresh boots
    //     landed at the identical budget, until they first differ ---
    let cap = if ctx_stop != StepStop::Budget {
        ctx_insn // don't probe past where the guest terminates
    } else {
        max_insns
    };
    let mut coarse_lo = 0u64;
    let mut coarse_hi: Option<u64> = None;
    let mut b = interval;
    loop {
        let budget = b.min(cap);
        let (fa, ia, _ta) = probe(budget)?;
        let (fb, ib, _tb) = probe(budget)?;
        let same = fa == fb && ia == ib;
        eprintln!(
            "[hydo] checkpoint @{budget:>12}: {} (A insn={ia} B insn={ib})",
            if same { "identical" } else { "DIFFER" }
        );
        if same {
            coarse_lo = budget;
            if budget >= cap {
                break; // reached the guest's terminal / the max budget with no divergence
            }
        } else {
            coarse_hi = Some(budget);
            break;
        }
        b += interval;
    }

    let coarse_hi = match coarse_hi {
        None => {
            println!("\n================ DIVERGENCE REPORT ================");
            println!(
                "determinism knobs: mask_entropy={} drop_kvmclock={} reset_tsc={}",
                det.mask_entropy, det.drop_kvmclock, det.reset_tsc
            );
            println!("RESULT: NO DIVERGENCE observed.");
            println!(
                "Two boots stayed bit-identical through {coarse_lo} retired instructions{}.",
                if ctx_stop != StepStop::Budget {
                    format!(" (the guest then reached terminal {ctx_stop:?})")
                } else {
                    " (the run budget ceiling)".to_string()
                }
            );
            println!("===================================================");
            return Ok(());
        }
        Some(h) => h,
    };
    eprintln!(
        "[hydo] first divergence in interval ({coarse_lo}, {coarse_hi}] insns; bisecting..."
    );

    // --- exact bisection: fresh VMs fast-forwarded to the last matching point,
    //     then single-stepped in lockstep until register state differs ---
    let exact = with_retry(|| bisect_exact(opts, det, coarse_lo, coarse_hi));

    println!("\n================ DIVERGENCE REPORT ================");
    println!("determinism knobs: mask_entropy={} drop_kvmclock={} reset_tsc={}",
        det.mask_entropy, det.drop_kvmclock, det.reset_tsc);
    println!("coarse interval : ({coarse_lo}, {coarse_hi}] retired instructions");
    match exact {
        Ok(Some(ex)) => ex.print(),
        Ok(None) => {
            println!("REPRODUCIBLE THROUGH : {coarse_lo} retired instructions (registers + control flow)");
            println!("FIRST DIVERGENCE     : RAM-only, within ({coarse_lo}, {coarse_hi}]");
            println!("  The register/control-flow state is identical at both interval ends, so the");
            println!("  earliest difference is in guest RAM only — e.g. the kvm-clock/pvclock page KVM");
            println!("  stamps with host wall-clock time, or a time/entropy value stored to memory but");
            println!("  not yet branched on. (Run with --ram to hash RAM at each coarse checkpoint.)");
        }
        Err(e) => println!("bisection failed: {e}"),
    }
    println!("===================================================");
    Ok(())
}

/// The exact-instruction divergence and its diagnosis.
pub struct ExactDivergence {
    pub insn: u64,
    pub culprit_rip: u64,
    pub control_flow_diverged: bool,
    pub reg_diffs: Vec<(&'static str, u64, u64)>,
    pub code_phys: Option<u64>,
    pub code_bytes: Vec<u8>,
    pub diagnosis: String,
    /// (apic_base, irq.injected, exc.injected, exc.nr) for boot A and boot B at hi.
    pub irq_a: (u64, u8, u8, u8),
    pub irq_b: (u64, u8, u8, u8),
    pub rip_a: u64,
    pub rip_b: u64,
}

impl ExactDivergence {
    fn print(&self) {
        println!("REPRODUCIBLE THROUGH : {} retired instructions", self.insn.saturating_sub(1));
        println!("FIRST DIVERGENCE AT  : instruction #{}", self.insn);
        println!("culprit RIP          : {:#018x}", self.culprit_rip);
        if let Some(p) = self.code_phys {
            println!("  (guest-phys {:#x})   bytes: {}", p, hex(&self.code_bytes));
        } else {
            println!("  (could not translate RIP to physical — high/unmapped)");
        }
        if self.control_flow_diverged {
            println!("kind                 : CONTROL-FLOW divergence (RIP differs after the step)");
            println!("  boot A rip           : {:#018x}", self.rip_a);
            println!("  boot B rip           : {:#018x}", self.rip_b);
            println!("                       => asynchronous event: an interrupt/fault landed on");
            println!("                       one boot but not the other at this instruction.");
        } else {
            println!("kind                 : DATA divergence (same RIP, differing register values)");
        }
        println!(
            "irq/apic  boot A     : apic_base={:#x} irq.injected={} exc.injected={} exc.nr={}",
            self.irq_a.0, self.irq_a.1, self.irq_a.2, self.irq_a.3
        );
        println!(
            "irq/apic  boot B     : apic_base={:#x} irq.injected={} exc.injected={} exc.nr={}",
            self.irq_b.0, self.irq_b.1, self.irq_b.2, self.irq_b.3
        );
        if !self.reg_diffs.is_empty() {
            println!("differing registers  :");
            for (name, a, b) in &self.reg_diffs {
                println!("    {name:<6} A={a:#018x}  B={b:#018x}");
            }
        }
        println!("DIAGNOSIS            : {}", self.diagnosis);
    }
}

/// Boot a fresh VM and fast-forward it to exactly `budget` retired instructions;
/// return its register fingerprint (and the actual landed instruction count).
fn probe_at(opts: &LinuxBootOpts, det: DetConfig, budget: u64) -> io::Result<([u8; 32], u64, bool)> {
    let mut vm = DetLinuxVm::boot(opts, det)?;
    let stop = vm.fast_forward(budget)?;
    Ok((vm.reg_fingerprint(), vm.insn_count, stop != StepStop::Budget))
}

/// Binary-search the exact retired-instruction count of first divergence, using
/// the PMU fast-path to land two independent boots at identical budgets and
/// comparing register fingerprints. Everything stays in ONE clock (retired
/// instructions), so the reported count is exact and consistent — unlike a
/// single-step bisection, which coalesces `rep` string ops into one step.
///
/// `lo` is known to match on both boots, `hi` is known to differ. Returns the
/// exact culprit instruction diagnosis (the instruction at `lo` produced it).
fn bisect_exact(
    opts: &LinuxBootOpts,
    det: DetConfig,
    mut lo: u64,
    mut hi: u64,
) -> io::Result<Option<ExactDivergence>> {
    // Invariant: state at `lo` matches on both boots; state at `hi` differs.
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        // Land two fresh boots at `mid` and compare (fresh VMs make this immune to
        // any live-VM state carry-over; both are deterministic up to divergence).
        let (fp_a, _, _) = probe_at(opts, det, mid)?;
        let (fp_b, _, _) = probe_at(opts, det, mid)?;
        if fp_a == fp_b {
            lo = mid;
        } else {
            hi = mid;
        }
    }

    // `lo` is the last matching instruction; the instruction at `lo` caused the
    // divergence observed at `hi = lo + 1`. Re-derive both states there.
    let mut a = DetLinuxVm::boot(opts, det)?;
    a.fast_forward(lo)?; // both boots identical here — this is boot A at the culprit
    let culprit_rip = a.regs()?.rip;

    let mut a_hi = DetLinuxVm::boot(opts, det)?;
    a_hi.fast_forward(hi)?;
    let mut b_hi = DetLinuxVm::boot(opts, det)?;
    b_hi.fast_forward(hi)?;

    if a_hi.reg_fingerprint() == b_hi.reg_fingerprint() {
        // The difference at `hi` is not register-visible → RAM-only.
        return Ok(None);
    }
    let cf = a_hi.regs()?.rip != b_hi.regs()?.rip;
    Ok(Some(diagnose(&a_hi, &b_hi, culprit_rip, hi, cf)))
}

/// Build the diagnosis for a divergence: which registers differ, the code bytes
/// at the culprit RIP, and a best-effort cause classification. `a`/`b` are the two
/// boots landed at `hi` (the first differing instruction count); `culprit_rip` is
/// the RIP of the instruction at `hi-1` whose execution produced the divergence.
fn diagnose(
    a: &DetLinuxVm,
    b: &DetLinuxVm,
    culprit_rip: u64,
    hi: u64,
    control_flow: bool,
) -> ExactDivergence {
    let ra = a.regs().unwrap_or_default();
    let rb = b.regs().unwrap_or_default();
    let named: [(&str, u64, u64); 18] = [
        ("rax", ra.rax, rb.rax),
        ("rbx", ra.rbx, rb.rbx),
        ("rcx", ra.rcx, rb.rcx),
        ("rdx", ra.rdx, rb.rdx),
        ("rsi", ra.rsi, rb.rsi),
        ("rdi", ra.rdi, rb.rdi),
        ("rbp", ra.rbp, rb.rbp),
        ("rsp", ra.rsp, rb.rsp),
        ("r8", ra.r8, rb.r8),
        ("r9", ra.r9, rb.r9),
        ("r10", ra.r10, rb.r10),
        ("r11", ra.r11, rb.r11),
        ("r12", ra.r12, rb.r12),
        ("r13", ra.r13, rb.r13),
        ("r14", ra.r14, rb.r14),
        ("r15", ra.r15, rb.r15),
        ("rip", ra.rip, rb.rip),
        ("rflags", ra.rflags, rb.rflags),
    ];
    let reg_diffs: Vec<(&'static str, u64, u64)> =
        named.iter().filter(|(_, x, y)| x != y).cloned().collect();

    // Read up to 16 code bytes at the culprit instruction.
    let (code_phys, code_bytes) = match a.read_code_at_rip(culprit_rip, 0, 16) {
        Some((_gva, bytes)) => {
            let phys = a.translate(culprit_rip);
            (phys, bytes)
        }
        None => (None, Vec::new()),
    };

    let irq_a = a.irq_state();
    let irq_b = b.irq_state();
    let diagnosis = classify(
        control_flow,
        &reg_diffs,
        &code_bytes,
        ra.rip,
        rb.rip,
        irq_a.1 != 0 || irq_b.1 != 0,
    );

    ExactDivergence {
        insn: hi,
        culprit_rip,
        control_flow_diverged: control_flow,
        reg_diffs,
        code_phys,
        code_bytes,
        diagnosis,
        irq_a,
        irq_b,
        rip_a: ra.rip,
        rip_b: rb.rip,
    }
}

/// Best-effort cause classification from the culprit opcode, which regs differ,
/// and the post-step control-flow / interrupt state.
fn classify(
    control_flow: bool,
    reg_diffs: &[(&str, u64, u64)],
    code: &[u8],
    rip_a: u64,
    rip_b: u64,
    irq_injected: bool,
) -> String {
    // A true asynchronous interrupt jumps FAR (into a handler) and shows an
    // injected interrupt; a data-driven branch stays within the same function.
    let rips_far = rip_a.abs_diff(rip_b) > 0x400;
    let async_irq = control_flow && (irq_injected || rips_far);
    // Identify the opcode at the culprit (skip common prefixes).
    let mut i = 0;
    while i < code.len()
        && matches!(
            code[i],
            0x66 | 0x67 | 0xf0 | 0xf2 | 0xf3 | 0x2e | 0x36 | 0x3e | 0x26 | 0x64 | 0x65
                | 0x40..=0x4f
        )
    {
        i += 1;
    }
    let op = &code[i..];
    let mnemonic = if op.len() >= 2 && op[0] == 0x0f {
        match op[1] {
            0x31 => Some("RDTSC (0f 31) — reads the timestamp counter"),
            0xa2 => Some("CPUID (0f a2)"),
            0x32 => Some("RDMSR (0f 32) — model-specific register read"),
            0x33 => Some("RDPMC (0f 33) — performance-counter read"),
            0x01 if op.len() >= 3 && op[2] == 0xf9 => Some("RDTSCP (0f 01 f9) — timestamp counter"),
            0xc7 if op.len() >= 3 => {
                let reg = (op[2] >> 3) & 7;
                match reg {
                    6 => Some("RDRAND (0f c7 /6) — hardware entropy"),
                    7 => Some("RDSEED (0f c7 /7) — hardware entropy"),
                    _ => None,
                }
            }
            _ => None,
        }
    } else {
        None
    };

    if async_irq {
        return
            "asynchronous interrupt/fault delivered on one boot but not the other \
             (in-kernel LAPIC timer or an APIC/IPI firing on host wall-clock time). \
             The instruction at the culprit RIP is innocent; the divergence is in \
             *when* the interrupt landed. This is the hard, timer-driven source."
                .to_string();
    }

    if let Some(m) = mnemonic {
        // CPUID itself is deterministic; a differing *result* means the input leaf
        // in EAX/ECX (or an upstream register) already differs — i.e. the real
        // divergence is a TSC-derived value a few instructions earlier.
        if m.starts_with("CPUID") {
            return format!(
                "culprit decodes as {m}, but CPUID is deterministic — a differing result means \
                 its input leaf (EAX/ECX) was computed from an already-divergent value upstream. \
                 In this TSC-calibration region that upstream value is a prior RDTSC (host wall-clock)."
            );
        }
        return format!(
            "the culprit instruction is {m}. Its result differs run-to-run, which \
             is exactly this instruction's nondeterminism source (RDTSC advances on host \
             wall-clock time; KVM exposes no per-RDTSC trap to userspace)."
        );
    }

    // Culprit is a conditional branch (Jcc) taken differently — a value computed
    // upstream differs. With the entropy/kvm-clock knobs on, the remaining such
    // value is almost always TSC-derived: the guest RDTSC advances on host
    // wall-clock, so calibration/delay loops iterate a different number of times.
    let is_jcc = {
        let mut i = 0;
        while i < code.len() && matches!(code[i], 0x66 | 0x67 | 0xf2 | 0xf3 | 0x2e | 0x3e) {
            i += 1;
        }
        let o = &code[i..];
        (!o.is_empty() && (0x70..=0x7f).contains(&o[0]))
            || (o.len() >= 2 && o[0] == 0x0f && (0x80..=0x8f).contains(&o[1]))
    };
    if control_flow && is_jcc {
        return
            "data-driven CONTROL-FLOW split: the culprit is a conditional branch (Jcc) that the \
             two boots take differently, because a value computed upstream differs. With entropy \
             and kvm-clock disabled, the remaining wall-clock leak is the TSC: RDTSC advances on \
             host time, not instruction count (KVM exposes no per-RDTSC trap to userspace), so \
             TSC calibration / delay loops run a different number of iterations each boot. The \
             run-to-run variance in the divergence LOCATION (below) is the fingerprint of this."
                .to_string();
    }
    if control_flow {
        return
            "control-flow split with no interrupt in flight — a value computed upstream differs \
             and steered a branch. Under these knobs the usual remaining cause is TSC-derived \
             timing (RDTSC is host-wall-clock). Inspect the culprit bytes with objdump."
                .to_string();
    }

    // No obvious instruction; infer from the differing registers.
    let only = |names: &[&str]| {
        !reg_diffs.is_empty()
            && reg_diffs
                .iter()
                .all(|(n, _, _)| names.contains(n))
    };
    if only(&["rax", "rdx"]) || only(&["rax"]) || only(&["rdx"]) {
        return "only RAX/RDX differ with identical RIP — classic signature of a \
                timestamp/MSR read (RDTSC/RDMSR) whose value is host-time dependent."
            .to_string();
    }
    format!(
        "differing registers do not match a known instruction signature; inspect the \
         culprit bytes above with objdump (-b binary -m i386:x86-64). Differing regs: {}",
        reg_diffs
            .iter()
            .map(|(n, _, _)| *n)
            .collect::<Vec<_>>()
            .join(",")
    )
}
