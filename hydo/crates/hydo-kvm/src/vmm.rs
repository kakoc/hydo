//! The machine around one vCPU: guest RAM, the devices (the 8259 PIC, the 16550, hydo's clock
//! and timer, the Mac engine's virtio-mmio disk and network), the exit clock, the interrupt
//! delivery rule, tick jitter and forced preemption points, snapshots and fingerprints, and the
//! run loop. It is generic over the vCPU ([`Cpu`], [`Hypervisor`]): KVM's on Linux, a scripted
//! one in the tests. The structure follows hydo-hvm's `vmm` (the Mac engine), function for
//! function where the two machines agree.
//!
//! # The clock
//!
//! The guest's clock is `Mach::vtime` in nanoseconds; it advances by the step (HYDO_KVM_STEP,
//! default 2500 ns) at every VM exit that reaches the engine and at nothing else. The guest
//! kernel reads it on HYDO_PORT_CLOCK, an exit itself, so a guest that polls the clock sees it
//! move. Exits KVM handles in the kernel (EPT faults, host interrupts, PAUSE loops) are not
//! counted: they do not depend on the guest alone. A kick from the host (the spin watchdog) is
//! not counted either.
//!
//! # The interrupt delivery rule
//!
//! Interrupts are decided only at exits, from the devices' levels at that exit: when the PIC has
//! a vector and the vCPU can take it now (KVM's `ready_for_interrupt_injection`: IF set, no STI
//! or MOV SS shadow, nothing being injected), the engine acknowledges it and injects it, and the
//! guest takes it before its next instruction; when it cannot, the engine asks for an
//! interrupt-window exit, which the CPU takes at the first instruction boundary where the guest
//! can (an architectural VM exit, as exact as any other), and decides again there. Every exit
//! that leaves an instruction to finish (I/O, MMIO, MSR) is finished first ([`Cpu::complete`]),
//! so the decision always sees the state after the instruction; a SNAP and its RESUME see the same
//! state as the run that went on (the removed KVM monitor learnt this: a snapshot taken between
//! the two halves of an I/O instruction re-ran it after the restore).
//!
//! # Idle
//!
//! KVM has no in-kernel interrupt controller here, so a HLT comes to the engine: with an
//! interrupt pending, the loop injects it; else the clock jumps to the next event the guest can
//! wait for (the timer's deadline, a device's completion) as on the Mac; with nothing armed the
//! run stops (`STOP_IDLE`); a HLT with interrupts off is a halted machine (`STOP_HALT`).
//!
//! # Exit-free loops
//!
//! A guest that computes without a VM exit cannot be preempted at a deterministic instruction:
//! interrupts arrive only at exits. The watchdog kicks the vCPU after HYDO_KVM_SPIN_MS of host
//! time without an exit, and the spin observer ([`crate::spin`], [`Vmm::observe`]) single-steps
//! it: a proven exact cycle (a spin on a flag another task must set) is treated as an idle HLT at
//! the cycle's canonical state. Kicks and steps are not exits. A guest still without an exit after
//! HYDO_KVM_SPIN_SECS ends the run as a SPIN (invalid).
//!
//! # The rewind mode (counter loops, Milestone 161)
//!
//! A proven counter loop (`while (flag) n++`: [`crate::spin::counter_loop`]) cannot be resolved
//! where the kick found it, since how far it had counted depends on the kick's host time. Without
//! the rewind mode the run ends at once as a SPIN, and the host runs the schedule again with
//! `spin=rewind` (hydo-control, as for the Mac engine). In the rewind mode ([`Rewind`]) the
//! engine keeps a checkpoint at a loop top: the vCPU (and the `kvm_run` outputs the interrupt
//! rule reads), the machine state, the devices (the copy-on-write disk is a cheap copy), tick
//! jitter, preemption points and the receive queue, and guest RAM as a second mapping kept equal
//! to RAM at the checkpoint (the pages written since are KVM's dirty-page log plus the pages the
//! devices wrote). It is taken at the start of every `run_until`, at the loop top after every
//! stop of the lock-step protocol (the host may have typed input there), after every rewind, and
//! every [`CK_EVERY`] exits. A proven counter loop then rewinds: the written pages back from the
//! copy, the vCPU into a fresh VM, and the run goes on from the checkpoint with the stop hook not
//! called up to the exit count where the loop was found (the host saw those exits already: the
//! guest makes them again, exactly as before), then with a hardware breakpoint at the loop's
//! canonical instruction. At its first arrival there with interrupts enabled the clock jumps to
//! the next event, as at an idle HLT, and the interrupt is delivered. The delivery point is fixed
//! by the guest alone; the host's timing decides only how much is replayed. A run without a
//! counter loop gives the same fingerprint with the mode on or off.
//!
//! # Several vCPUs (docs/design/kvm-vcpus.md)
//!
//! With `VmOpts::vcpus` above 1 the VM has that many vCPUs, run one at a time from this thread
//! in turns ([`crate::smp`]): the running vCPU is `cur`, every counted exit of it advances the
//! one clock and counts against its turn, and the turn ends after `q` exits, at a HLT, at an OUT
//! to HYDO_PORT_RELAX (the SMP guest kernel's `cpu_relax()`), or at a spin the observer proves
//! while another vCPU can run. Each vCPU has a local APIC in x2APIC mode that the engine emulates
//! ([`crate::apic`]: its MSR accesses come here), the 8259 PIC reaches vCPU 0 only (its LINT0 in
//! ExtINT mode), and the interrupt rule above runs for the vCPU about to run. INIT and STARTUP
//! IPIs start the other vCPUs in real mode. With one vCPU none of this runs: the machine, its
//! snapshots and its fingerprints are the ones of before.

use crate::cpu::{Cpu, CpuState, Exit, Hypervisor, Reply};
use crate::fnv::{fnv, fnv_u64, mix64, OFFSET};
use crate::layout::*;
use crate::machine::{Mach, Tjit};
use crate::mem::{Mem, RamBase, RamImage, PAGE};
use crate::msr;
use crate::pic::Pic;
use crate::smp::{Run, Smp};
use crate::uart;
use crate::{fatal, Diag};
use hydo_hvm::machine::{Pct, PctLog, Where};
use hydo_hvm::virtio::{self, Ram, VStats, Virtio};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The run loop's settings.
#[derive(Clone, Debug)]
pub struct VmOpts {
    pub ram_size: u64,
    pub step: u64,
    pub spin_secs: f64,
    /// host seconds without an exit before the spin observer looks (HYDO_KVM_SPIN_MS)
    pub spin_look: f64,
    pub trace: Option<String>,
    pub trace_unknown: bool,
    /// HYDO_KVM_SPIN_STATE: the spin observer's full-state cycles ([`Vmm::observe_state`])
    pub spin_state: bool,
    /// The fingerprint keeps KVM's leftover event fields ([`CpuState::without_stale_events`]):
    /// only for an image built before fingerprint epoch 2 (Milestone 204).
    pub stale_events: bool,
    pub cmdline: String,
    pub virtio: virtio::Config,
    /// the vCPUs (1: the machine of every recorded value; more: [`crate::smp`])
    pub vcpus: usize,
}

/// What the watchdog calls to kick the current vCPU out of `KVM_RUN` (None between vCPUs).
type Kicker = Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>>;

/// The watchdog's shared state.
struct Watch {
    exits: AtomicU64,
    armed: AtomicBool,
    stop: AtomicBool,
    /// host seconds without an exit before a kick (f64 bits): the observer's delay, doubled
    /// after each observation that proved nothing
    wait: AtomicU64,
}

pub struct Vmm {
    pub ram: Mem,
    pub ram_size: u64,
    hv: Box<dyn Hypervisor>,
    /// the VM's vCPUs, vCPU 0 first (one, or [`VmOpts::vcpus`])
    cpus: Vec<Box<dyn Cpu>>,
    /// the vCPU that runs now (always 0 with one vCPU)
    cur: usize,
    /// the vCPUs a fresh VM gets
    ncpu: usize,
    /// several vCPUs: the scheduler, the local APICs and the other vCPUs' MSR models (None: one)
    pub smp: Option<Box<Smp>>,
    pub m: Mach,
    pub step: u64,
    pub console: Vec<u8>,
    echo: Option<Box<dyn Write + Send>>,
    trace: Option<std::io::BufWriter<std::fs::File>>,
    trace_unknown: bool,
    pub tj: Tjit,
    pub pct: Pct,
    /// `HYDO_PREEMPT_RECORD`: the records of the run's forced preemption points (None: off;
    /// in no snapshot and no fingerprint)
    pub pct_log: PctLog,
    pub rx: uart::Rx,
    pub vio: Virtio,
    pub max_exits: u64,
    spin_secs: f64,
    spin_look: f64,
    last_exit: Instant,
    pub kicks: u64,
    /// HYDO_KVM_TRACE_UNKNOWN: where the vCPU was at the kicks of an exit-free stretch
    spin_rips: Vec<u64>,
    watch: Arc<Watch>,
    wd: Option<std::thread::JoinHandle<()>>,
    kicker: Kicker,
    pub diag: Diag,
    /// the base guest RAM is mapped from ([`crate::mem`]; None: anonymous memory since the boot)
    base: Option<Arc<RamBase>>,
    /// the rewind mode ([`Rewind`])
    pub rw: Rewind,
    /// counts the lock-step stops (the host's conversations): the stop hook adds one at each, and
    /// the rewind mode takes a checkpoint after it
    pub stops: u64,
    /// console bytes echoed so far (a replay of the rewind mode does not echo them again)
    echoed: u64,
    /// HYDO_KVM_SPIN_STATE: the observer also proves full-state cycles ([`Vmm::observe_state`])
    spin_state: bool,
    /// [`VmOpts::stale_events`]
    stale_events: bool,
    /// full-state cycles proven and resolved (statistics)
    pub state_cycles: u64,
    /// full-state looks that proved nothing (statistics)
    pub state_gave_up: u64,
    /// why the last full-state look proved nothing (the trace)
    state_why: &'static str,
}

/// A snapshot of the machine (the vCPU, the engine's state, RAM, the devices, the console). RAM
/// is a base and the pages that differ from it ([`RamImage`]), the disk a copy-on-write copy:
/// a snapshot costs what the run changed, not the size of RAM and the disk.
#[derive(Default)]
pub struct XSnap {
    /// vCPU 0
    pub cpu: CpuState,
    /// vCPUs 1 to N-1 and their `kvm_run` flags (several vCPUs)
    pub aps: Vec<(CpuState, [u8; 3])>,
    /// the several-vCPU state ([`crate::smp`]; None with one vCPU)
    pub smp: Option<Box<Smp>>,
    /// what KVM said at the last exit (`ready_for_interrupt_injection`, `if_flag`) and the window
    /// request ([`crate::cpu::Cpu::run_flags`]): not part of the vCPU's state that `KVM_SET_*`
    /// loads, but the interrupt rule reads them at the first loop top after a restore, before
    /// any `KVM_RUN` has set them again. A fresh vCPU has them all 0 ("cannot take an interrupt
    /// now"), so a run restored at a stop where an interrupt was pending asked for the window (an
    /// exit more) where the run that took the snapshot injected at once.
    pub flags: [u8; 3],
    pub m: Mach,
    pub ram: RamImage,
    pub vio: Option<virtio::Save>,
    pub con: Vec<u8>,
}

/// Exits between two checkpoints of the rewind mode when no stop comes sooner.
pub const CK_EVERY: u64 = 1024;

/// A checkpoint of the rewind mode (RAM is the mode's copy, [`Rewind::shadow`]).
struct Ck {
    cpu: CpuState,
    flags: [u8; 3],
    m: Mach,
    vio: Option<virtio::Save>,
    vs: VStats,
    tj: Tjit,
    pct: Pct,
    rx: uart::Rx,
    /// the loop top's `resume` (after a kick: no interrupt decision)
    resume: bool,
}

/// The rewind mode's state (see the module documentation). Nothing of it is guest state: it is
/// in no snapshot and no fingerprint.
#[derive(Default)]
pub struct Rewind {
    /// the mode is on (`spin=rewind`)
    pub on: bool,
    /// the copy of RAM and the dirty-page log are in step with the checkpoint
    valid: bool,
    ck: Option<Box<Ck>>,
    /// guest RAM as at the checkpoint (a private mapping of the same base)
    shadow: Option<Mem>,
    /// pages the devices wrote since the checkpoint
    dev_dirty: Vec<u64>,
    /// a checkpoint is due at the next loop top
    due: bool,
    /// the exit count of the loop top where a counter loop was found: replayed up to it without
    /// the stop hook
    replay_to: Option<u64>,
    /// the loop's canonical instruction, armed at `replay_to`'s loop top
    target: Option<u64>,
    armed: bool,
    pub proven: u64,
    pub resolved: u64,
    pub checkpoints: u64,
    pub replayed_exits: u64,
    ck_exits: u64,
}

fn watchdog(w: Arc<Watch>, kicker: Kicker) {
    let mut last = u64::MAX;
    let mut since = Instant::now();
    while !w.stop.load(Ordering::Relaxed) {
        let wait = f64::from_bits(w.wait.load(Ordering::Relaxed));
        std::thread::sleep(Duration::from_secs_f64((wait / 4.0).clamp(0.001, 0.25)));
        if !w.armed.load(Ordering::Relaxed) {
            last = u64::MAX;
            since = Instant::now();
            continue;
        }
        let x = w.exits.load(Ordering::Relaxed);
        if x != last {
            last = x;
            since = Instant::now();
            continue;
        }
        if since.elapsed().as_secs_f64() > wait {
            if let Some(k) = kicker.lock().ok().as_ref().and_then(|k| k.as_ref()) {
                k();
            }
            since = Instant::now();
        }
    }
}

impl Vmm {
    /// The VM with the kernel, the initramfs and the command line loaded, the devices, and a
    /// vCPU at the kernel's 64-bit entry.
    pub fn new(
        o: &VmOpts,
        kernel: &[u8],
        initrd: &[u8],
        hv: Box<dyn Hypervisor>,
        diag: Diag,
        echo: Option<Box<dyn Write + Send>>,
    ) -> Vmm {
        let vio = match Virtio::setup(&o.virtio) {
            Ok(v) => v,
            Err(e) => fatal(1, &e.replace("hvm:", "hydo-kvm:")),
        };
        let ram = match Mem::anon(o.ram_size as usize) {
            Ok(m) => m,
            Err(e) => fatal(1, &format!("hydo-kvm: guest RAM: {e}")),
        };
        let trace = o
            .trace
            .as_ref()
            .and_then(|p| std::fs::File::create(p).ok().map(std::io::BufWriter::new));
        let watch = Arc::new(Watch {
            exits: AtomicU64::new(0),
            armed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            wait: AtomicU64::new(o.spin_look.to_bits()),
        });
        let kicker: Kicker = Arc::new(std::sync::Mutex::new(None));
        let (w2, k2) = (watch.clone(), kicker.clone());
        let wd = std::thread::Builder::new()
            .name("kvm-watchdog".into())
            .spawn(move || watchdog(w2, k2))
            .ok();
        let ncpu = o.vcpus.clamp(1, crate::smp::MAX_VCPUS);
        let mut vm = Vmm {
            ram,
            ram_size: o.ram_size,
            hv,
            cpus: Vec::new(),
            cur: 0,
            ncpu,
            smp: (ncpu > 1).then(|| Box::new(Smp::new(ncpu))),
            m: Mach::default(),
            step: o.step,
            console: vec![0u8; CONSOLE_MAX],
            echo,
            trace,
            trace_unknown: o.trace_unknown,
            tj: Tjit::default(),
            pct: Pct::default(),
            pct_log: None,
            rx: uart::Rx::default(),
            vio,
            max_exits: 200_000_000,
            spin_secs: o.spin_secs,
            spin_look: o.spin_look,
            last_exit: Instant::now(),
            kicks: 0,
            spin_rips: Vec::new(),
            watch,
            wd,
            kicker,
            diag,
            base: None,
            rw: Rewind::default(),
            stops: 0,
            echoed: 0,
            spin_state: o.spin_state,
            stale_events: o.stale_events,
            state_cycles: 0,
            state_gave_up: 0,
            state_why: "",
        };
        let mut cmdline = cmdline(&o.cmdline, vm.vio.ndev());
        if ncpu > 1 {
            // the engine's x2APIC needs no interrupt remapping; the PIC goes to vCPU 0's LINT0
            // only with the I/O APIC turned off (docs/design/kvm-vcpus.md)
            cmdline.push_str(" x2apic_phys noapic");
        }
        match crate::boot::load_n(vm.ram.bytes_mut(), kernel, initrd, &cmdline, ncpu) {
            Ok(line) => vm.say(&line),
            Err(e) => fatal(1, &e),
        }
        vm.vio.bind(o.step);
        let h = vm.hv.describe();
        vm.say(&h);
        vm.fresh_vm();
        let mut s = vm.cpu().save();
        s.regs = crate::boot::regs();
        s.sregs = crate::boot::sregs();
        if let Some(sm) = &vm.smp {
            // the local APIC on in x2APIC mode: KVM shows the APIC in CPUID from this base
            s.sregs.apic_base = sm.apic[0].base;
        }
        vm.cpu_mut().load(&s);
        for i in 1..vm.cpus.len() {
            let base = vm.smp.as_ref().map_or(0, |sm| sm.apic[i].base);
            let mut a = vm.cpus[i].save();
            a.sregs.apic_base = base;
            vm.cpus[i].load(&a);
        }
        vm
    }

    /// A fresh VM and vCPU around the same RAM (the boot, and every restore).
    pub fn fresh_vm(&mut self) {
        self.cpus.clear();
        let (p, n) = (self.ram.ptr(), self.ram.len());
        // SAFETY: the RAM mapping lives as long as this value, which drops its vCPUs first
        let made = unsafe {
            if self.ncpu > 1 {
                self.hv.fresh_n(p, n, self.ncpu)
            } else {
                self.hv.fresh(p, n).map(|c| vec![c])
            }
        };
        let c = match made {
            Ok(c) => c,
            Err(e) => fatal(1, &e),
        };
        // one thread runs every vCPU, so vCPU 0's kicker reaches whichever runs
        if let Ok(mut k) = self.kicker.lock() {
            *k = Some(c[0].kicker());
        }
        self.cpus = c;
    }

    fn cpu(&self) -> &dyn Cpu {
        &*self.cpus[self.cur]
    }
    fn cpu_mut(&mut self) -> &mut dyn Cpu {
        &mut *self.cpus[self.cur]
    }

    /// The vCPU that runs now (0 with one vCPU).
    pub fn current(&self) -> usize {
        self.cur
    }

    pub fn say(&self, s: &str) {
        crate::say(&self.diag, s);
    }

    pub fn trace_line(&mut self, s: &str) {
        if self.rw.replay_to.is_some() {
            return; // a replay of the rewind mode: traced the first time
        }
        if let Some(t) = &mut self.trace {
            let _ = writeln!(t, "{s}");
        }
    }

    // ---------------- typed input (the lock-step SEND) ----------------
    pub fn rx_room(&self) -> u32 {
        self.rx.room()
    }
    pub fn rx_push(&mut self, c: u8) {
        self.rx.push(c)
    }

    // ---------------- the timer, tick jitter and preemption points ----------------
    /// The clock value at which the timer's interrupt is raised: the deadline, plus the delay.
    pub fn timer_due(&self) -> u64 {
        if !self.tj.on {
            return self.m.timer_cval;
        }
        self.m.timer_cval.wrapping_add(
            self.tj
                .delay(self.m.timer_cval / TJ_PERIOD, self.step)
                .wrapping_mul(self.step),
        )
    }
    fn pct_at_exit(&mut self) {
        if !self.pct.due(self.m.exits) {
            return;
        }
        let timer = match &self.smp {
            None => (self.m.timer_armed != 0, self.timer_due()),
            Some(sm) => {
                let d = sm.apic[self.cur].timer_deadline();
                (d != 0, d)
            }
        };
        let at = if self.pct_log.is_some() {
            self.cpu().where_now()
        } else {
            Where::default()
        };
        let mut vtime = self.m.vtime;
        self.pct
            .at_exit(self.m.exits, timer, &mut vtime, &mut self.pct_log, at);
        self.m.vtime = vtime;
    }
    fn timer_level(&mut self) -> bool {
        if self.m.timer_armed == 0 {
            return false;
        }
        if !self.tj.on {
            return self.m.vtime >= self.m.timer_cval;
        }
        let lv = self.m.vtime >= self.timer_due();
        if lv
            && self.m.vtime >= self.m.timer_cval
            && self.tj.delay(self.m.timer_cval / TJ_PERIOD, self.step) != 0
        {
            let k = self.m.timer_cval / TJ_PERIOD;
            if k != self.tj.last_applied_k {
                self.tj.applied += 1;
                self.tj.last_applied_k = k;
            }
        }
        lv
    }

    /// The devices' lines into the PIC (level-triggered).
    fn update_levels(&mut self) {
        if let Some(sm) = self.smp.as_mut() {
            let now = self.m.vtime;
            for a in sm.apic.iter_mut() {
                a.tick(now);
            }
        }
        let mut l: u16 = 0;
        if self.timer_level() {
            l |= 1 << TIMER_IRQ;
        }
        if uart::level(&self.m.uart, &self.rx) {
            l |= 1 << uart::IRQ;
        }
        let now = self.m.vtime;
        let mut ram = Ram {
            b: self.ram.bytes_mut(),
            base: 0,
            high: ram_high(self.ram_size),
            dirty: self.rw.on.then_some(&mut self.rw.dev_dirty),
        };
        self.vio.poll(&mut ram, now);
        for d in 0..self.vio.ndev().min(VIRTIO_IRQ.len()) {
            if self.vio.level(d) {
                l |= 1 << VIRTIO_IRQ[d];
            }
        }
        self.m.pic.set_levels(l);
    }

    /// The vector the guest would take now, if any (with several vCPUs: the running vCPU).
    pub fn irq_pending(&mut self) -> Option<u8> {
        if self.smp.is_some() {
            let i = self.cur;
            return self.pending_for(i).map(|(v, _)| v);
        }
        self.update_levels();
        self.m.pic.pending()
    }

    /// Several vCPUs: the vector vCPU `i` would take now and whether it is the PIC's. Its
    /// APIC's vector comes first, then the PIC's, for vCPU 0 only and only through an ExtINT
    /// LINT0 (QEMU's user-space APIC orders them the same way).
    fn pending_for(&mut self, i: usize) -> Option<(u8, bool)> {
        self.update_levels();
        let sm = self.smp.as_ref()?;
        if let Some(v) = sm.apic[i].pending() {
            return Some((v, false));
        }
        if i == 0 && sm.apic[0].extint() {
            return self.m.pic.pending().map(|v| (v, true));
        }
        None
    }

    /// The interrupt delivery rule at the top of the run loop (see the module documentation).
    fn deliver(&mut self) {
        if self.smp.is_some() {
            return self.deliver_smp();
        }
        if self.irq_pending().is_none() {
            self.cpu_mut().request_window(false);
            return;
        }
        if !self.cpu().interruptible() {
            self.cpu_mut().request_window(true);
            return;
        }
        let Some((vector, line)) = self.m.pic.ack() else {
            return;
        };
        if line == TIMER_IRQ {
            self.m.timer_armed = 0; // the one-shot timer is disarmed by its delivery
        }
        self.m.n_irq += 1;
        if self.trace.is_some() {
            let (ex, rip) = (self.m.exits, self.cpu().rip());
            self.trace_line(&format!(
                "IRQ {line} vector {vector:#x} at exit {ex} rip {rip:x}"
            ));
        }
        self.cpu_mut().request_window(false);
        self.cpu_mut().inject(vector);
    }

    /// The rule for the running vCPU of a machine with several vCPUs.
    fn deliver_smp(&mut self) {
        let i = self.cur;
        let Some((vector, pic)) = self.pending_for(i) else {
            self.cpu_mut().request_window(false);
            return;
        };
        if !self.cpu().interruptible() {
            self.cpu_mut().request_window(true);
            return;
        }
        if pic {
            let Some((v, line)) = self.m.pic.ack() else {
                return;
            };
            if line == TIMER_IRQ {
                self.m.timer_armed = 0;
            }
            debug_assert_eq!(v, vector);
        } else if let Some(sm) = self.smp.as_mut() {
            sm.apic[i].ack();
        }
        self.m.n_irq += 1;
        if self.trace.is_some() {
            let (ex, rip) = (self.m.exits, self.cpu().rip());
            self.trace_line(&format!(
                "IRQ vector {vector:#x} to vCPU {i} at exit {ex} rip {rip:x}"
            ));
        }
        self.cpu_mut().request_window(false);
        self.cpu_mut().inject(vector);
    }

    /// Several vCPUs: whether vCPU `i` can run now (ready, or halted with interrupts on and an
    /// interrupt deliverable to it).
    fn runnable(&mut self, i: usize) -> bool {
        let run = self.smp.as_ref().map(|sm| sm.run[i]);
        match run {
            Some(Run::Ready) => true,
            Some(Run::Halted) => self.pending_for(i).is_some(),
            _ => false,
        }
    }

    /// Several vCPUs, at the loop top: when the turn ended or the running vCPU cannot run, the
    /// next vCPU that can (round robin from the one after), or, when none can, the clock's jump
    /// to the next event, as at one vCPU's idle HLT. Some(stop) when nothing can run again.
    fn schedule(&mut self) -> Option<i32> {
        loop {
            let (end, n) = {
                let sm = self.smp.as_ref()?;
                (sm.end, sm.n())
            };
            let cur = self.cur;
            if !end && self.runnable(cur) {
                if let Some(sm) = self.smp.as_mut() {
                    if sm.run[cur] == Run::Halted {
                        sm.switch_to(cur); // woken in its own turn: a new turn
                    }
                }
                return None;
            }
            let next = (1..=n).map(|k| (cur + k) % n).find(|&j| self.runnable(j));
            if let Some(j) = next {
                if let Some(sm) = self.smp.as_mut() {
                    sm.switch_to(j);
                }
                self.cur = j;
                return None;
            }
            // nobody can run: the next event any vCPU or device waits for
            let all_stopped = self.smp.as_ref().is_some_and(|sm| {
                sm.run
                    .iter()
                    .all(|r| matches!(r, Run::Stopped | Run::WaitSipi))
            });
            if all_stopped {
                return Some(STOP_HALT);
            }
            let t = self.next_event();
            if t > self.m.vtime {
                self.m.vtime = t;
                self.m.n_hlt_jump += 1;
                continue;
            }
            // nothing armed, or a due event no vCPU can take (masked, or in service)
            return Some(STOP_IDLE);
        }
    }

    /// Several vCPUs: whether a vCPU other than the running one can run now.
    fn others_runnable(&mut self) -> bool {
        let n = self.smp.as_ref().map_or(1, |sm| sm.n());
        (0..n).any(|j| j != self.cur && self.runnable(j))
    }

    /// Several vCPUs: end the running vCPU's turn at the next loop top.
    fn end_turn(&mut self) {
        if let Some(sm) = self.smp.as_mut() {
            sm.end = true;
        }
    }

    /// The next clock value the guest can wait for, 0 when nothing is armed.
    pub fn next_event(&mut self) -> u64 {
        let mut next = if self.m.timer_armed != 0 {
            self.timer_due()
        } else {
            0
        };
        if let Some(sm) = &self.smp {
            for a in &sm.apic {
                let d = a.timer_deadline();
                if d != 0 && (next == 0 || d < next) {
                    next = d;
                }
            }
        }
        let now = self.m.vtime;
        let ram = Ram {
            b: self.ram.bytes_mut(),
            base: 0,
            high: ram_high(self.ram_size),
            dirty: None,
        };
        let vd = self.vio.next_due(&ram, now);
        if vd != 0 && (next == 0 || vd < next) {
            next = vd;
        }
        next
    }

    /// A HLT: Some(stop) when the run ends here.
    fn idle(&mut self) -> Option<i32> {
        self.m.n_hlt += 1;
        if self.smp.is_some() {
            // the turn ends; the scheduler runs another vCPU or moves the clock
            let i = self.cur;
            let off = !self.cpu().if_flag();
            if !off && self.pending_for(i).is_some() {
                return None; // the loop's top delivers it
            }
            if let Some(sm) = self.smp.as_mut() {
                sm.run[i] = if off { Run::Stopped } else { Run::Halted };
                sm.end = true;
            }
            return None;
        }
        if !self.cpu().if_flag() {
            return Some(STOP_HALT); // cli; hlt: nothing can wake it
        }
        if self.irq_pending().is_some() {
            return None; // the loop's top delivers it
        }
        let next = self.next_event();
        if next > self.m.vtime {
            self.m.vtime = next;
            self.m.n_hlt_jump += 1;
            return None;
        }
        if next == 0 {
            return Some(STOP_IDLE);
        }
        None
    }

    // ---------------- exits ----------------
    fn console_put(&mut self, v: u8) {
        if (self.m.console_len as usize) < CONSOLE_MAX {
            self.console[self.m.console_len as usize] = v;
            self.m.console_len += 1;
        }
        if self.m.console_len <= self.echoed {
            return; // a replay of the rewind mode: echoed the first time
        }
        self.echoed = self.m.console_len;
        if let Some(e) = &mut self.echo {
            let _ = e.write_all(&[v]);
            if v == b'\n' {
                let _ = e.flush();
            }
        }
    }

    fn unknown(&mut self, what: String) {
        self.m.n_unknown += 1;
        if self.trace_unknown {
            let rip = self.cpu().rip();
            self.say(&format!(
                "hydo-kvm: {what} at exit {} rip {rip:#x}",
                self.m.exits
            ));
        }
    }

    /// One element of a port read.
    fn port_in(&mut self, port: u16, size: u8, k: u32) -> u64 {
        match port {
            p if Pic::handles(p) => self.m.pic.read(p) as u64,
            p if (uart::BASE..uart::BASE + 8).contains(&p) => {
                uart::read(&self.m.uart, &mut self.rx, p - uart::BASE) as u64
            }
            PORT_CLOCK => {
                if k == 0 {
                    self.m.n_clock += 1;
                }
                let t = self.m.vtime;
                if k.is_multiple_of(2) {
                    t & 0xffff_ffff
                } else {
                    t >> 32
                }
            }
            PORT_WALL => {
                if k.is_multiple_of(2) {
                    WALL_EPOCH & 0xffff_ffff
                } else {
                    WALL_EPOCH >> 32
                }
            }
            PORT_CTL => 1, // the interface version
            _ => {
                self.unknown(format!("IN from port {port:#x}"));
                u64::MAX >> (64 - 8 * size as u32)
            }
        }
    }

    /// One element of a port write.
    fn port_out(&mut self, port: u16, v: u64) {
        match port {
            p if Pic::handles(p) => self.m.pic.write(p, v as u8),
            p if (uart::BASE..uart::BASE + 8).contains(&p) => {
                if let Some(c) = uart::write(&mut self.m.uart, p - uart::BASE, v as u8) {
                    self.console_put(c);
                }
            }
            PORT_TIMER => {
                let d = v as u32 as u64;
                if d == 0 {
                    self.m.timer_armed = 0;
                } else {
                    self.m.timer_armed = 1;
                    self.m.timer_cval = self.m.vtime.wrapping_add(d);
                }
            }
            PORT_CTL => {
                if v as u32 == 1 || v as u32 == 2 {
                    self.m.halted = v as u32;
                }
            }
            // the SMP guest kernel's cpu_relax(): this vCPU waits for another one
            PORT_RELAX if self.smp.is_some() => self.end_turn(),
            _ => self.unknown(format!("OUT {v:#x} to port {port:#x}")),
        }
    }

    fn handle_io(&mut self, port: u16, size: u8, out: bool, count: u32, data: &[u8]) {
        self.m.n_io += 1;
        let sz = size as usize;
        if out {
            for k in 0..count as usize {
                let mut b = [0u8; 8];
                b[..sz].copy_from_slice(&data[k * sz..k * sz + sz]);
                self.port_out(port, u64::from_le_bytes(b));
            }
            return;
        }
        let mut r = Vec::with_capacity(sz * count as usize);
        for k in 0..count {
            let v = self.port_in(port, size, k);
            r.extend_from_slice(&v.to_le_bytes()[..sz]);
        }
        self.cpu_mut().reply(Reply::Io(r));
    }

    fn handle_mmio(&mut self, addr: u64, len: u8, write: bool, data: [u8; 8]) {
        self.m.n_mmio += 1;
        let n = self.vio.ndev() as u64;
        let end = VIRTIO_BASE + n * virtio::MMIO_SIZE;
        let mut v = u64::from_le_bytes(data);
        if (VIRTIO_BASE..end).contains(&addr) {
            // the Mac engine's devices, at their own addresses
            let ipa = virtio::MMIO_BASE + (addr - VIRTIO_BASE);
            let now = self.m.vtime;
            let mut ram = Ram {
                b: self.ram.bytes_mut(),
                base: 0,
                high: ram_high(self.ram_size),
                dirty: self.rw.on.then_some(&mut self.rw.dev_dirty),
            };
            self.vio.mmio(&mut ram, ipa, write, len as u32, &mut v, now);
        } else {
            self.unknown(format!(
                "MMIO {} at {addr:#x}",
                if write { "write" } else { "read" }
            ));
            v = 0;
        }
        if !write {
            self.cpu_mut().reply(Reply::Mmio(v.to_le_bytes()));
        }
    }

    fn handle_msr(&mut self, index: u32, value: Option<u64>) {
        self.m.n_msr += 1;
        let now = self.m.vtime;
        if self.smp.is_some()
            && (index == crate::apic::MSR_APIC_BASE
                || (crate::apic::X2APIC_FIRST..=crate::apic::X2APIC_LAST).contains(&index))
        {
            return self.apic_msr(index, value);
        }
        let i = self.cur;
        let model = match self.smp.as_mut() {
            Some(sm) if i > 0 => &mut sm.msrs[i],
            _ => &mut self.m.msrs,
        };
        let a = match value {
            None => model.read(index, now),
            Some(v) => model.write(index, v),
        };
        if !msr::Msrs::known(index) {
            self.unknown(format!(
                "{} MSR {index:#x} (#GP)",
                if value.is_some() { "WRMSR" } else { "RDMSR" }
            ));
        }
        let r = match a {
            msr::Answer::Value(v) => Reply::Msr {
                value: v,
                fault: false,
            },
            msr::Answer::Fault => Reply::Msr {
                value: 0,
                fault: true,
            },
        };
        self.cpu_mut().reply(r);
    }

    /// An access to the running vCPU's local APIC (several vCPUs).
    fn apic_msr(&mut self, index: u32, value: Option<u64>) {
        let (i, now) = (self.cur, self.m.vtime);
        let Some(sm) = self.smp.as_mut() else {
            return;
        };
        let n = sm.n();
        let reply = match value {
            None => match sm.apic[i].rdmsr(index, now) {
                Some(v) => Reply::Msr {
                    value: v,
                    fault: false,
                },
                None => Reply::Msr {
                    value: 0,
                    fault: true,
                },
            },
            Some(v) => match sm.apic[i].wrmsr(index, v, now) {
                crate::apic::Wrote::Done => Reply::Msr {
                    value: 0,
                    fault: false,
                },
                crate::apic::Wrote::Fault => Reply::Msr {
                    value: 0,
                    fault: true,
                },
                crate::apic::Wrote::Unsupported(msg) => {
                    let rip = self.cpu().rip();
                    fatal(3, &format!("{msg} (vCPU {i}, rip {rip:#x})"))
                }
                crate::apic::Wrote::Send(snd) => {
                    for t in crate::smp::targets(snd.dest, i, n) {
                        self.ipi(i, t, snd.kind);
                    }
                    Reply::Msr {
                        value: 0,
                        fault: false,
                    }
                }
            },
        };
        self.cpu_mut().reply(reply);
    }

    /// An IPI of vCPU `from` arrives at vCPU `t`.
    fn ipi(&mut self, from: usize, t: usize, kind: crate::apic::Kind) {
        use crate::apic::Kind;
        if self.trace.is_some() {
            let ex = self.m.exits;
            self.trace_line(&format!(
                "IPI {kind:?} from vCPU {from} to vCPU {t} at exit {ex}"
            ));
        }
        let Some(sm) = self.smp.as_mut() else {
            return;
        };
        match kind {
            Kind::Fixed(v) => sm.apic[t].post(v),
            Kind::Nmi => fatal(
                3,
                &format!("hydo-kvm: an NMI IPI from vCPU {from} to vCPU {t} is not emulated yet"),
            ),
            Kind::Init => {
                if sm.started[t] {
                    fatal(
                        3,
                        &format!(
                            "hydo-kvm: an INIT to vCPU {t}, which already ran (CPU hot-unplug), is not emulated"
                        ),
                    );
                }
                sm.run[t] = Run::WaitSipi;
                sm.apic[t].init();
            }
            Kind::Startup(v) => {
                if sm.run[t] != Run::WaitSipi {
                    return; // a STARTUP to a running vCPU is ignored (Linux sends two)
                }
                sm.run[t] = Run::Ready;
                let base = sm.apic[t].base;
                // the state after INIT and STARTUP: the reset state with CS = V << 8, IP 0
                let mut a = self.cpus[t].save();
                a.regs = crate::abi::kvm_regs {
                    rflags: 0x2,
                    ..crate::abi::kvm_regs::default()
                };
                a.sregs.cs.selector = (v as u16) << 8;
                a.sregs.cs.base = (v as u64) << 12;
                a.sregs.cs.limit = 0xffff;
                a.sregs.apic_base = base;
                self.cpus[t].load(&a);
            }
        }
    }

    // ---------------- the run loop ----------------
    /// Run until `hook` says stop (`STOP_MARKER`) or the guest stops: the stop reason.
    pub fn run_until(&mut self, hook: &mut dyn FnMut(&mut Vmm) -> bool) -> i32 {
        let mut resume = false;
        if self.rw.on {
            self.rw_enter();
        }
        loop {
            if self.m.halted != 0 {
                return STOP_HALT;
            }
            if self.m.exits >= self.max_exits {
                return STOP_LIMIT;
            }
            // a replay of the rewind mode: the host saw these exits' stops already
            let replaying = self.rw.replay_to.is_some_and(|x| self.m.exits <= x);
            if !replaying {
                let n = self.stops;
                if hook(self) {
                    return STOP_MARKER;
                }
                if self.stops != n {
                    self.rw.due = true; // the host may have typed input
                }
                // (not where a kick left the guest: a checkpoint is at a loop top of an exit)
                if self.rw.on
                    && !resume
                    && (self.rw.due || self.m.exits >= self.rw.ck_exits + CK_EVERY)
                {
                    self.rw_checkpoint(resume);
                }
            }
            if !resume {
                if self.smp.is_some() {
                    if let Some(stop) = self.schedule() {
                        return stop;
                    }
                }
                self.deliver();
            }
            resume = false;
            if let Some(t) = self.rw.target {
                if !self.rw.armed && self.rw.replay_to == Some(self.m.exits) {
                    // the replay is at the loop top where the counter loop was found: stop at
                    // the loop's canonical instruction
                    self.cpu_mut()
                        .step_to(Some(&crate::spin::Step { at: [t, 0], n: 1 }));
                    self.rw.armed = true;
                }
            }
            self.watch.armed.store(true, Ordering::Relaxed);
            let e = self.cpu_mut().run();
            self.watch.armed.store(false, Ordering::Relaxed);
            if self.rw.armed {
                match e {
                    Exit::Debug => {
                        match self.rw_hit() {
                            Ok(r) => resume = r,
                            Err(s) => return s,
                        }
                        continue;
                    }
                    Exit::Kick => {
                        self.kicks += 1;
                        resume = true;
                        if self.last_exit.elapsed().as_secs_f64() > self.spin_secs {
                            let ex = self.m.exits;
                            self.say(&format!(
                                "hydo-kvm: SPIN: the rewind's replay did not reach the counter loop's canonical instruction in {:.0} s (exit {ex}); this run is invalid",
                                self.spin_secs
                            ));
                            return STOP_SPIN;
                        }
                        continue;
                    }
                    _ => {
                        // the replay made an exit the first run did not make before the loop:
                        // not the same run any more; the exit is handled as any exit
                        let ex = self.m.exits;
                        self.say(&format!(
                            "hydo-kvm: rewind: an exit before the counter loop's canonical instruction (exit {ex}); the loop stays unresolved"
                        ));
                        self.cpu_mut().step_to(None);
                        self.rw.armed = false;
                        self.rw.target = None;
                        self.rw.replay_to = None;
                    }
                }
            }
            if e == Exit::Kick {
                // not an exit of the guest: nothing changed in it
                self.kicks += 1;
                resume = true;
                if self.trace_unknown && self.spin_rips.len() < 16 {
                    let rip = self.cpu().rip();
                    self.spin_rips.push(rip);
                }
                if self.last_exit.elapsed().as_secs_f64() > self.spin_secs {
                    let rip = self.cpu().rip();
                    self.say(&format!(
                        "hydo-kvm: SPIN: no VM exit for {:.0} s of host time at exit {}, rip {rip:#x} -- \
                         the guest loops without exits; this run is not deterministic from here and is invalid",
                        self.spin_secs, self.m.exits
                    ));
                    if self.trace_unknown {
                        let d = self.spin_report();
                        self.say(&d);
                    }
                    return STOP_SPIN;
                }
                // the spin observer (crate::spin): is this a provable cycle?
                let mut seen = self.observe();
                if seen == crate::spin::Outcome::GaveUp && self.spin_state {
                    // HYDO_KVM_SPIN_STATE: a loop that stores, when the whole machine repeats
                    seen = self.observe_state();
                }
                match seen {
                    crate::spin::Outcome::Resolved => {
                        self.m.n_spin += 1;
                        self.set_wait(self.spin_look);
                        self.last_exit = Instant::now();
                        if self.smp.is_some() && self.others_runnable() {
                            // the loop waits for another vCPU: the turn ends at its canonical
                            // state, which the guest alone fixes
                            self.end_turn();
                            resume = false;
                            continue;
                        }
                        if self.smp.is_some() && self.cpu().regs().rflags & (1 << 9) == 0 {
                            // no other vCPU can run and this one cannot take an interrupt
                            let rip = self.cpu().rip();
                            self.say(&format!(
                                "hydo-kvm: spin: a proven cycle with interrupts off and no other vCPU to run (deadlock) at rip {rip:#x}"
                            ));
                            return STOP_IDLE;
                        }
                        let next = self.next_event();
                        if next > self.m.vtime {
                            self.m.vtime = next; // as at an idle HLT
                        } else if self.irq_pending().is_none() {
                            // nothing armed, or an event that is due but cannot interrupt (its
                            // line masked or in service): nothing can end the cycle
                            let rip = self.cpu().rip();
                            self.say(&format!(
                                "hydo-kvm: spin: a proven cycle that no interrupt can end (deadlock) at rip {rip:#x}"
                            ));
                            return STOP_IDLE;
                        }
                        resume = false;
                    }
                    crate::spin::Outcome::Deadlock | crate::spin::Outcome::Counter(None) => {
                        let rip = self.cpu().rip();
                        self.say(&format!(
                            "hydo-kvm: spin: a proven cycle with interrupts off (deadlock) at rip {rip:#x}"
                        ));
                        return STOP_IDLE;
                    }
                    crate::spin::Outcome::Counter(Some(canon)) => {
                        self.rw.proven += 1;
                        if !self.rw.on || !self.rw.valid || self.rw.ck.is_none() {
                            let (rip, ex) = (self.cpu().rip(), self.m.exits);
                            self.say(&format!(
                                "hydo-kvm: SPIN: a counter loop (proven infinite) at rip {rip:#x}, exit {ex} -- \
                                 only the rewind mode (spin=rewind) resolves it; this run is invalid"
                            ));
                            return STOP_SPIN;
                        }
                        if self.rw.replay_to.is_some() {
                            // a replay found a loop the first run did not: never expected
                            let ex = self.m.exits;
                            self.say(&format!(
                                "hydo-kvm: rewind: a counter loop found while replaying (exit {ex}); this run is invalid"
                            ));
                            return STOP_SPIN;
                        }
                        resume = self.rw_rewind(canon);
                    }
                    crate::spin::Outcome::GaveUp => {
                        let w = (self.wait() * 2.0).min(self.spin_secs / 4.0);
                        self.set_wait(w);
                    }
                    crate::spin::Outcome::Exit(e) => {
                        self.set_wait(self.spin_look);
                        self.last_exit = Instant::now();
                        self.spin_rips.clear();
                        if let Some(s) = self.exit(e) {
                            return s;
                        }
                        resume = false;
                    }
                }
                continue;
            }
            if self.wait() != self.spin_look {
                self.set_wait(self.spin_look);
            }
            self.last_exit = Instant::now();
            self.spin_rips.clear();
            if let Some(s) = self.exit(e) {
                return s;
            }
        }
    }

    // ---------------- the rewind mode ----------------
    /// Turn the rewind mode on or off (a run's phases, `spin=rewind`).
    pub fn set_rewind(&mut self, on: bool) {
        if self.rw.on == on || (on && self.smp.is_some()) {
            // several vCPUs: no rewind mode yet (a counter loop ends the run as a SPIN)
            return;
        }
        if !on {
            if self.rw.valid {
                self.cpu_mut().dirty_log(false);
            }
            let st = (
                self.rw.proven,
                self.rw.resolved,
                self.rw.checkpoints,
                self.rw.replayed_exits,
            );
            self.rw = Rewind::default();
            (
                self.rw.proven,
                self.rw.resolved,
                self.rw.checkpoints,
                self.rw.replayed_exits,
            ) = st;
            return;
        }
        self.rw.on = true;
    }

    /// At the start of `run_until` in the rewind mode: the dirty-page log on and RAM's copy equal
    /// to RAM (when a restore made them invalid), and a checkpoint due.
    fn rw_enter(&mut self) {
        if !self.rw.valid {
            self.cpu_mut().dirty_log(true);
            let mut sh = match self.rw.shadow.take() {
                Some(m) => m,
                None => match Mem::anon(self.ram.len()) {
                    Ok(m) => m,
                    Err(e) => fatal(1, &format!("hydo-kvm: the rewind mode's copy of RAM: {e}")),
                },
            };
            if let Some(b) = &self.base {
                if let Err(e) = sh.map_base(b) {
                    fatal(1, &format!("hydo-kvm: the rewind mode's copy of RAM: {e}"));
                }
            } else {
                sh.bytes_mut().fill(0);
            }
            for k in self.ram.private_pages(self.base.as_deref()) {
                let k = k as usize;
                sh.bytes_mut()[k * PAGE..(k + 1) * PAGE].copy_from_slice(self.ram.page(k));
            }
            self.rw.shadow = Some(sh);
            self.rw.dev_dirty.clear();
            self.rw.valid = true;
        }
        self.rw.due = true;
    }

    /// The pages written since the last checkpoint (KVM's log and the devices'), sorted, once
    /// each; both logs start afresh.
    fn rw_dirty(&mut self) -> Vec<u64> {
        let mut d = std::mem::take(&mut self.rw.dev_dirty);
        self.cpu_mut().take_dirty(&mut d);
        d.sort_unstable();
        d.dedup();
        let n = (self.ram.len() / PAGE) as u64;
        d.retain(|&p| p < n);
        d
    }

    /// A checkpoint at this loop top (after the stop hook, before the interrupt decision).
    fn rw_checkpoint(&mut self, resume: bool) {
        let d = self.rw_dirty();
        if let Some(sh) = self.rw.shadow.as_mut() {
            let live = self.ram.bytes();
            for &p in &d {
                let o = p as usize * PAGE;
                sh.bytes_mut()[o..o + PAGE].copy_from_slice(&live[o..o + PAGE]);
            }
        }
        let ck = Ck {
            cpu: self.cpu().save(),
            flags: self.cpu().run_flags(),
            m: self.m,
            vio: self.vio.attached().then(|| self.vio.save()),
            vs: self.vio.vs,
            tj: self.tj,
            pct: self.pct,
            rx: self.rx.clone(),
            resume,
        };
        self.rw.ck = Some(Box::new(ck));
        self.rw.ck_exits = self.m.exits;
        self.rw.due = false;
        self.rw.checkpoints += 1;
    }

    /// Back to the checkpoint, to replay up to this exit count and stop at `canon` there: the
    /// loop top's `resume`.
    fn rw_rewind(&mut self, canon: u64) -> bool {
        let x = self.m.exits;
        let d = self.rw_dirty();
        if let Some(sh) = self.rw.shadow.as_ref() {
            let live = self.ram.bytes_mut();
            for &p in &d {
                let o = p as usize * PAGE;
                live[o..o + PAGE].copy_from_slice(&sh.bytes()[o..o + PAGE]);
            }
        }
        let ck = self.rw.ck.take().expect("a checkpoint");
        self.fresh_vm();
        self.cpu_mut().dirty_log(true);
        self.cpu_mut().load(&ck.cpu);
        self.cpu_mut().set_run_flags(ck.flags);
        self.m = ck.m;
        if let Some(v) = &ck.vio {
            self.vio.restore(v);
        }
        self.vio.vs = ck.vs;
        self.tj = ck.tj;
        self.pct = ck.pct;
        // the replay fires the points after the checkpoint again: their records go too
        if let Some(l) = self.pct_log.as_mut() {
            l.retain(|r| r.exit <= ck.m.exits);
        }
        self.rx = ck.rx.clone();
        let resume = ck.resume;
        self.rw.ck = Some(ck);
        self.rw.replayed_exits += x - self.m.exits;
        self.rw.replay_to = Some(x);
        self.rw.target = Some(canon);
        self.rw.armed = false;
        self.set_wait(self.spin_look);
        self.last_exit = Instant::now();
        if self.trace.is_some() {
            let from = self.m.exits;
            if let Some(t) = &mut self.trace {
                let _ = writeln!(
                    t,
                    "REWIND a counter loop at exit {x}: back to exit {from}, canonical rip {canon:x}"
                );
            }
        }
        resume
    }

    /// The replay reached the canonical instruction (it has not executed): deliver as at an idle
    /// HLT when interrupts are on; else run on to the next arrival. Ok(resume), or the stop.
    fn rw_hit(&mut self) -> Result<bool, i32> {
        let t = self.rw.target.expect("a target");
        if self.cpu().regs().rflags & (1 << 9) == 0 {
            // interrupts off here (the way to the loop passes it): on to the next arrival
            self.cpu_mut()
                .step_to(Some(&crate::spin::Step { at: [t, 0], n: 1 }));
            return Ok(true);
        }
        self.cpu_mut().step_to(None);
        self.rw.armed = false;
        self.rw.target = None;
        self.rw.replay_to = None;
        self.rw.resolved += 1;
        self.rw.due = true;
        self.m.n_spin += 1;
        self.set_wait(self.spin_look);
        self.last_exit = Instant::now();
        let next = self.next_event();
        if next > self.m.vtime {
            self.m.vtime = next;
        } else if self.irq_pending().is_none() {
            self.say(&format!(
                "hydo-kvm: spin: a counter loop that no interrupt can end (deadlock) at rip {t:#x}"
            ));
            return Err(STOP_IDLE);
        }
        Ok(false)
    }

    /// Count one exit (the clock) and trace it.
    fn count(&mut self, e: &Exit) {
        self.watch.exits.fetch_add(1, Ordering::Relaxed);
        self.m.exits += 1;
        if let Some(sm) = self.smp.as_mut() {
            sm.count();
        }
        self.m.vtime = self.m.vtime.wrapping_add(self.step);
        self.pct_at_exit();
        if self.trace.is_some() {
            let (ex, rip) = (self.m.exits, self.cpu().rip());
            let line = match e {
                Exit::Io {
                    port, out, count, ..
                } => format!(
                    "{ex} io {} {port:#x} x{count} {rip:x}",
                    if *out { "out" } else { "in" }
                ),
                Exit::Mmio { addr, write, .. } => format!(
                    "{ex} mmio {} {addr:#x} {rip:x}",
                    if *write { "w" } else { "r" }
                ),
                Exit::RdMsr { index } => format!("{ex} rdmsr {index:#x} {rip:x}"),
                Exit::WrMsr { index, value } => {
                    format!("{ex} wrmsr {index:#x}={value:#x} {rip:x}")
                }
                other => format!("{ex} {other:?} {rip:x}"),
            };
            self.trace_line(&line);
        }
    }

    /// Handle one exit of the guest (and the exits its instruction's completion needs): Some(stop)
    /// when the run ends here.
    fn exit(&mut self, first: Exit) -> Option<i32> {
        let mut e = first;
        loop {
            if e == Exit::Kick || e == Exit::Debug {
                return None; // not an exit of the guest
            }
            self.count(&e);
            match e {
                Exit::Io {
                    port,
                    size,
                    out,
                    count,
                    data,
                } => self.handle_io(port, size, out, count, &data),
                Exit::Mmio {
                    addr,
                    len,
                    write,
                    data,
                } => self.handle_mmio(addr, len, write, data),
                Exit::RdMsr { index } => self.handle_msr(index, None),
                Exit::WrMsr { index, value } => self.handle_msr(index, Some(value)),
                Exit::Hlt => return self.idle(),
                Exit::IrqWindow => {
                    self.m.n_window += 1;
                    return None;
                }
                Exit::Shutdown => {
                    self.m.halted = 3;
                    return Some(STOP_HALT);
                }
                Exit::Kick | Exit::Debug => return None,
                Exit::Fail(msg) => {
                    let rip = self.cpu().rip();
                    fatal(
                        3,
                        &format!(
                            "hydo-kvm: {msg} after {} exits (rip {rip:#x})",
                            self.m.exits
                        ),
                    )
                }
            }
            // finish the instruction before anything else looks at the guest
            match self.cpu_mut().complete() {
                None => return None,
                Some(next) => e = next,
            }
        }
    }

    fn wait(&self) -> f64 {
        f64::from_bits(self.watch.wait.load(Ordering::Relaxed))
    }
    fn set_wait(&self, w: f64) {
        self.watch.wait.store(w.to_bits(), Ordering::Relaxed);
    }

    /// One look of the spin observer ([`crate::spin`]): single-step pure instructions until the
    /// register state cycles (then step to the cycle's canonical state), an instruction is not
    /// pure, a real exit comes, or the budget is spent. Neither the steps nor the kicks are exits.
    /// A counter loop ([`crate::spin::counter_loop`]) is reported only when the budget is spent
    /// without an exact cycle, so a loop the exact rule resolves is resolved as before.
    pub fn observe(&mut self) -> crate::spin::Outcome {
        use crate::spin::{cycle, state_hash, Flow, Outcome, STEPS};
        if !self.cpu().may_step() {
            return Outcome::GaveUp;
        }
        let cr3 = self.cpu().cr3();
        let mut hs: Vec<u64> = Vec::with_capacity(64);
        let mut ifs: Vec<bool> = Vec::with_capacity(64);
        let mut states: Vec<crate::abi::kvm_regs> = Vec::with_capacity(64);
        let mut flows: Vec<Flow> = Vec::with_capacity(64);
        let mut counter: Option<Option<u64>> = None;
        let mut target: Option<u64> = None;
        let mut steps = 0usize;
        let mut kicked = false;
        let out = loop {
            let r = self.cpu().regs();
            let h = state_hash(&r);
            if target == Some(h) {
                break Outcome::Resolved;
            }
            // after a kick the step may not have happened: the same state is not a new one
            let again = kicked && hs.last() == Some(&h);
            kicked = false;
            let fresh = target.is_none() && !again;
            if fresh {
                hs.push(h);
                ifs.push(r.rflags & (1 << 9) != 0);
                states.push(r);
                if let Some((start, p)) = cycle(&hs) {
                    let mut canon = (start..start + p).filter(|&i| ifs[i]).map(|i| hs[i]).min();
                    if canon.is_none() && self.smp.is_some() {
                        // several vCPUs: a cycle with interrupts off waits for another vCPU; its
                        // turn ends at the smallest state of the cycle
                        canon = (start..start + p).map(|i| hs[i]).min();
                    }
                    match canon {
                        None => break Outcome::Deadlock,
                        Some(c) if c == h => break Outcome::Resolved,
                        Some(c) => target = Some(c),
                    }
                } else if counter.is_none() {
                    counter = counter_of(&states, &flows);
                }
            }
            steps += 1;
            if steps >= STEPS {
                break match counter {
                    Some(c) if target.is_none() => Outcome::Counter(c),
                    _ => Outcome::GaveUp,
                };
            }
            let Some((step, flow)) = self.cpu().next_step(self.ram.bytes(), cr3) else {
                break Outcome::GaveUp;
            };
            if fresh {
                flows.push(flow);
            }
            self.cpu_mut().step_to(Some(&step));
            match self.cpu_mut().run() {
                Exit::Debug => {}
                Exit::Kick => kicked = true,
                e => break Outcome::Exit(e),
            }
        };
        self.cpu_mut().step_to(None);
        out
    }

    /// The full-state cycle (HYDO_KVM_SPIN_STATE, a prototype; docs/design/kvm-engine.md, "Loops
    /// that store: full-state cycles"), tried when [`Vmm::observe`] proved nothing. A loop that
    /// stores to memory (a call's return address, a buffer filled with the same bytes in every
    /// pass) or uses instructions the decoder does not know (VEX) is infinite when the whole
    /// machine returns to a state it was in: on one vCPU without an exit nothing else changes
    /// RAM or the devices, so the next pass is the same computation again.
    ///
    /// The proof: the vCPU's whole state at the kick (`s0`: registers, segment and control
    /// registers, the XSAVE image, the KVM-owned MSRs, the pending events) and a hardware
    /// breakpoint at its instruction; at each arrival whose whole vCPU state equals `s0`, KVM's
    /// dirty-page log names the pages written since the arrival before, and their bytes are
    /// compared with a copy taken then. When every page written since was in the copy with the
    /// same bytes, RAM is what it was at the arrival before, so the machine state repeats
    /// exactly. Then one period is walked with trap-flag single steps (no instruction of the
    /// period saves RFLAGS: [`crate::spin::saves_flags`]; no fault or exception can be in it,
    /// since the guest kernel's handlers count them in memory) and the vCPU is stepped to the
    /// period's canonical state: the smallest register-state hash among the states with
    /// interrupts enabled; when that hash occurs at several points, the whole vCPU state and the
    /// pages the period writes tell them apart ([`Vmm::state_walk`]). That state is a function of
    /// the cycle alone, not of where the kick came.
    pub fn observe_state(&mut self) -> crate::spin::Outcome {
        use crate::spin::{Outcome, Step};
        /// arrivals at the kick's instruction before giving up
        const ARRIVALS: u64 = 200_000;
        /// pages a pass may write (a larger loop is not a polling loop)
        const MAX_PAGES: usize = 1024;
        /// matches (passes) before giving up: the first pass may write pages the next do not
        const MATCHES: u32 = 8;
        if !self.cpu().can_single_step() || !self.cpu().may_step() {
            self.state_why = "the vCPU cannot be stepped";
            return Outcome::GaveUp;
        }
        let t0 = Instant::now();
        let budget = (self.spin_secs / 4.0).clamp(0.05, 2.0);
        self.state_why = "";
        // the rewind mode's log when it is on (its checkpoint then gets the pages too)
        let shared = self.rw.on && self.rw.valid;
        if !shared {
            self.cpu_mut().dirty_log(true);
        }
        let _ = self.state_dirty(shared);
        let s0 = self.cpu().save();
        let r0 = s0.regs.rip;
        let mut copy: std::collections::BTreeMap<u64, Vec<u8>> = Default::default();
        let mut matches = 0u32;
        let mut arrivals = 0u64;
        let mut period: Vec<u64> = Vec::new();
        let mut out = loop {
            self.cpu_mut().step_to(Some(&Step { at: [r0, 0], n: 1 }));
            self.watch.armed.store(true, Ordering::Relaxed);
            let e = self.cpu_mut().run();
            self.watch.armed.store(false, Ordering::Relaxed);
            match e {
                Exit::Debug => {}
                Exit::Kick => {
                    if t0.elapsed().as_secs_f64() > budget {
                        self.state_why = "out of time";
                        break Outcome::GaveUp;
                    }
                    continue;
                }
                e => break Outcome::Exit(e),
            }
            arrivals += 1;
            if arrivals > ARRIVALS || t0.elapsed().as_secs_f64() > budget {
                self.state_why = "no repeat in time";
                break Outcome::GaveUp;
            }
            if self.cpu().regs() != s0.regs || self.cpu().save() != s0 {
                continue;
            }
            let d = self.state_dirty(shared);
            if d.len() > MAX_PAGES {
                self.state_why = "a pass wrote too many pages";
                break Outcome::GaveUp;
            }
            let ram = self.ram.bytes();
            let page = |p: u64| &ram[p as usize * PAGE..(p as usize + 1) * PAGE];
            if matches > 0
                && d.iter()
                    .all(|p| copy.get(p).is_some_and(|c| c[..] == *page(*p)))
            {
                period = d;
                break Outcome::Resolved; // proven; the walk below finds the canonical state
            }
            for &p in &d {
                copy.insert(p, page(p).to_vec());
            }
            matches += 1;
            if matches > MATCHES {
                self.state_why = "RAM did not repeat";
                break Outcome::GaveUp;
            }
        };
        self.cpu_mut().step_to(None);
        if out == Outcome::Resolved {
            out = self.state_walk(&s0, &period, t0, budget);
        }
        let _ = self.state_dirty(shared);
        if !shared {
            self.cpu_mut().dirty_log(false);
        }
        match out {
            Outcome::Resolved => self.state_cycles += 1,
            Outcome::GaveUp => self.state_gave_up += 1,
            _ => {}
        }
        if self.trace.is_some() {
            let (ex, rip) = (self.m.exits, self.cpu().rip());
            self.trace_line(&format!(
                "STATE-CYCLE {out:?} at exit {ex}: kick at {r0:x}, {arrivals} arrivals, {matches} matches, now at {rip:x} {}",
                self.state_why
            ));
        }
        out
    }

    /// The pages the guest wrote since the last call (KVM's log), sorted, once each; with the
    /// rewind mode's log (`shared`) they are kept for its next checkpoint too.
    fn state_dirty(&mut self, shared: bool) -> Vec<u64> {
        let mut d = Vec::new();
        self.cpu_mut().take_dirty(&mut d);
        if shared {
            self.rw.dev_dirty.extend_from_slice(&d);
        }
        d.sort_unstable();
        d.dedup();
        let n = (self.ram.len() / PAGE) as u64;
        d.retain(|&p| p < n);
        d
    }

    /// One period of a proven full-state cycle from `s0` (where the vCPU is), by single steps,
    /// then on to the period's canonical state ([`Vmm::observe_state`]).
    fn state_walk(
        &mut self,
        s0: &CpuState,
        pages: &[u64],
        t0: Instant,
        budget: f64,
    ) -> crate::spin::Outcome {
        use crate::spin::{saves_flags, state_hash, syscall_len, Outcome};
        /// instructions in one period at most
        const WALK: usize = 1 << 20;
        let cr3 = self.cpu().cr3();
        let mut hs: Vec<u64> = Vec::new();
        let mut ifs: Vec<bool> = Vec::new();
        self.cpu_mut().single_step(true);
        // one step; Err(the outcome) when the walk ends here
        let step = |v: &mut Vmm| -> Result<(), Outcome> {
            let rip = v.cpu().regs().rip;
            let mut code = Vec::with_capacity(15);
            for k in 0..15 {
                match translate(v.ram.bytes(), cr3, rip.wrapping_add(k)) {
                    Some(pa) => code.push(v.ram.bytes()[pa as usize]),
                    None => break,
                }
            }
            if let Some(n) = syscall_len(&code) {
                // over the system call to the instruction after it (the kernel's part of the
                // period is not walked; it is the same in every pass)
                v.cpu_mut().step_to(Some(&crate::spin::Step {
                    at: [rip.wrapping_add(n), 0],
                    n: 1,
                }));
                let r = loop {
                    v.watch.armed.store(true, Ordering::Relaxed);
                    let e = v.cpu_mut().run();
                    v.watch.armed.store(false, Ordering::Relaxed);
                    match e {
                        Exit::Debug => break Ok(()),
                        Exit::Kick if t0.elapsed().as_secs_f64() <= budget * 4.0 => continue,
                        Exit::Kick => break Err(Outcome::GaveUp),
                        e => break Err(Outcome::Exit(e)),
                    }
                };
                v.cpu_mut().step_to(None);
                v.cpu_mut().single_step(true);
                return r;
            }
            if code.is_empty() || saves_flags(&code) {
                v.state_why = "an instruction that saves RFLAGS";
                return Err(Outcome::GaveUp);
            }
            loop {
                match v.cpu_mut().run() {
                    Exit::Debug => return Ok(()),
                    Exit::Kick => continue, // a late kick: nothing ran
                    e => return Err(Outcome::Exit(e)),
                }
            }
        };
        let out = 'walk: {
            loop {
                let r = self.cpu().regs();
                if !hs.is_empty() && r == s0.regs && self.cpu().save() == *s0 {
                    break; // a whole period
                }
                hs.push(state_hash(&r));
                ifs.push(r.rflags & (1 << 9) != 0);
                if hs.len() > WALK || t0.elapsed().as_secs_f64() > budget * 4.0 {
                    self.state_why = "the period is too long";
                    break 'walk Outcome::GaveUp;
                }
                if let Err(o) = step(self) {
                    break 'walk o;
                }
            }
            let mut canon = hs
                .iter()
                .zip(&ifs)
                .filter(|(_, &i)| i)
                .map(|(&h, _)| h)
                .min();
            if canon.is_none() && self.smp.is_some() {
                // several vCPUs: a cycle with interrupts off waits for another vCPU; any of its
                // states may be where the turn ends
                for v in ifs.iter_mut() {
                    *v = true;
                }
                canon = hs.iter().copied().min();
            }
            let Some(c) = canon else {
                break 'walk Outcome::Deadlock;
            };
            // the vCPU is at s0 again, the start of the period: index 0
            let tied: Vec<usize> = (0..hs.len()).filter(|&i| hs[i] == c && ifs[i]).collect();
            let target = if tied.len() == 1 {
                tied[0]
            } else {
                // the same registers at several points of the period (a function called twice
                // from one frame): those points differ in the rest of the vCPU's state or in
                // the pages the period writes, so a second walk tells them apart by both
                let mut keys: Vec<(u64, usize)> = Vec::with_capacity(tied.len());
                for i in 0..hs.len() {
                    if tied.contains(&i) {
                        let mut k = self.cpu().save().without_stale_events().hash(OFFSET);
                        let ram = self.ram.bytes();
                        for &p in pages {
                            let o = p as usize * PAGE;
                            k = mix64(&ram[o..o + PAGE], k);
                        }
                        keys.push((k, i));
                    }
                    if let Err(o) = step(self) {
                        break 'walk o;
                    }
                }
                keys.sort_unstable();
                if keys.len() < 2 || keys[0].0 == keys[1].0 {
                    self.state_why = "no single canonical state";
                    break 'walk Outcome::GaveUp; // two points with one state: no canonical point
                }
                keys[0].1
            };
            for _ in 0..target {
                if let Err(o) = step(self) {
                    break 'walk o;
                }
            }
            if state_hash(&self.cpu().regs()) != c {
                // the walk is deterministic: never expected
                self.state_why = "the second walk differs";
                break 'walk Outcome::GaveUp;
            }
            Outcome::Resolved
        };
        self.cpu_mut().single_step(false);
        out
    }

    /// What the vCPU was doing in an exit-free stretch (HYDO_KVM_TRACE_UNKNOWN, at a SPIN): the
    /// sampled instruction pointers, the registers, and the code and stack bytes through the
    /// guest's page tables.
    fn spin_report(&self) -> String {
        use std::fmt::Write as _;
        let s = self.cpu().save();
        let r = &s.regs;
        let mut t = String::new();
        let _ = write!(t, "hydo-kvm: spin samples");
        for x in &self.spin_rips {
            let _ = write!(t, " {x:#x}");
        }
        let _ = write!(
            t,
            "\nhydo-kvm: rip {:#x} rsp {:#x} rbp {:#x} rax {:#x} rbx {:#x} rcx {:#x} rdx {:#x} rsi {:#x} rdi {:#x} rflags {:#x} cr3 {:#x} cpl {}",
            r.rip, r.rsp, r.rbp, r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rflags,
            s.sregs.cr3, s.sregs.cs.dpl
        );
        let ram = self.ram.bytes();
        for (what, va, n) in [
            ("code", r.rip.wrapping_sub(32), 80u64),
            ("stack", r.rsp, 64),
            ("rdi", r.rdi, 64),
            ("rsi", r.rsi, 64),
        ] {
            let _ = write!(t, "\nhydo-kvm: {what} at {va:#x}:");
            for k in 0..n {
                match translate(ram, s.sregs.cr3, va + k) {
                    Some(pa) => {
                        let _ = write!(t, " {:02x}", ram[pa as usize]);
                    }
                    None => {
                        let _ = write!(t, " --");
                    }
                }
            }
        }
        t
    }

    // ---------------- snapshots ----------------
    /// Take a snapshot. `as_base` (the host's S0 and S1, which every run restores): its RAM
    /// becomes a new base ([`crate::mem::RamBase`]) that guest RAM is then mapped from, so the
    /// runs from it cost only the pages they write; else (and with a base already) the snapshot
    /// keeps the pages that differ from the current base. The first snapshot of a boot is always
    /// a base.
    pub fn snapshot(&mut self, s: &mut XSnap, as_base: bool) {
        s.cpu = self.cpus[0].save();
        s.flags = self.cpus[0].run_flags();
        s.aps = self.cpus[1..]
            .iter()
            .map(|c| (c.save(), c.run_flags()))
            .collect();
        s.smp = self.smp.clone();
        s.m = self.m;
        let own = self.ram.private_pages(self.base.as_deref());
        if as_base || self.base.is_none() {
            let base = {
                let (live, old) = (&self.ram, self.base.as_deref());
                RamBase::build(self.ram.len(), |k, put| {
                    if own.binary_search(&(k as u32)).is_ok() {
                        put(live.page(k));
                    } else if let Some(b) = old.filter(|b| b.nonzero(k)) {
                        put(b.page(k));
                    }
                })
            };
            let base = match base {
                Ok(b) => Arc::new(b),
                Err(e) => fatal(1, &format!("hydo-kvm: a RAM image for a snapshot: {e}")),
            };
            // guest RAM reads the new base from now on (the same bytes: the guest sees nothing;
            // KVM follows the new mapping)
            if let Err(e) = self.ram.map_base(&base) {
                fatal(
                    1,
                    &format!("hydo-kvm: guest RAM from a snapshot's image: {e}"),
                );
            }
            self.base = Some(base.clone());
            s.ram = RamImage {
                base: Some(base),
                pages: Vec::new(),
                data: Vec::new(),
            };
        } else {
            let mut data = Vec::with_capacity(own.len() * PAGE);
            for &k in &own {
                data.extend_from_slice(self.ram.page(k as usize));
            }
            s.ram = RamImage {
                base: self.base.clone(),
                pages: own,
                data,
            };
        }
        s.vio = self.vio.attached().then(|| self.vio.save());
        s.con = self.console[..self.m.console_len as usize].to_vec();
        if self.smp.is_some() {
            // several vCPUs: the machine goes on in a fresh VM with the snapshot's vCPU states
            // loaded, exactly as every run restored from the snapshot starts. On an Intel host a
            // run from the explorer worker's S1 ended with another fingerprint than the fresh
            // boot that went on from its own S1 (Milestone 226), with the same verdict, turns
            // and control transcript: KVM's vCPU state after KVM_SET_* is not always what
            // KVM_GET_* read before, and a fresh VM is not the VM that ran. With one vCPU the
            // machine goes on in its VM, as every recorded value was made.
            self.fresh_vm();
            self.load_cpus(s);
        }
    }

    /// Load a snapshot's vCPU states and `kvm_run` flags into the VM's vCPUs (a fresh VM).
    fn load_cpus(&mut self, s: &XSnap) {
        self.cpus[0].load(&s.cpu);
        self.cpus[0].set_run_flags(s.flags);
        for (i, (c, f)) in s.aps.iter().enumerate() {
            if let Some(cpu) = self.cpus.get_mut(i + 1) {
                cpu.load(c);
                cpu.set_run_flags(*f);
            }
        }
        if !s.aps.is_empty() {
            self.note_round_trip(s);
        }
    }

    /// Several vCPUs: say (on the engine's diagnostics, at most 16 times per process) which
    /// parts of a vCPU's state read back from KVM differ from the state just loaded. They are
    /// what KVM changes in a load, which the fingerprint sees and the guest may not.
    fn note_round_trip(&self, s: &XSnap) {
        static NOTES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let states = std::iter::once(&s.cpu).chain(s.aps.iter().map(|(c, _)| c));
        for (i, want) in states.enumerate() {
            let Some(cpu) = self.cpus.get(i) else {
                continue;
            };
            let got = cpu.save();
            let d = crate::cpu::differences(want, &got);
            if d.is_empty() || NOTES.fetch_add(1, Ordering::Relaxed) >= 16 {
                continue;
            }
            self.say(&format!(
                "hydo-kvm: vCPU {i} read back after a load differs at exit {}: {}",
                self.m.exits,
                d.join("; ")
            ));
        }
    }

    /// Restore a snapshot into a fresh VM (input typed but not read is dropped, as on the Mac):
    /// guest RAM mapped from the snapshot's base again (every page the runs since wrote is
    /// dropped at once) and its own pages written back.
    pub fn restore(&mut self, s: &XSnap) {
        self.cpus.clear();
        match &s.ram.base {
            Some(b) => {
                if let Err(e) = self.ram.map_base(b) {
                    fatal(
                        1,
                        &format!("hydo-kvm: guest RAM from a snapshot's image: {e}"),
                    );
                }
            }
            None => {
                // a snapshot without an image (never made by `snapshot`): zero RAM
                match Mem::anon(self.ram.len()) {
                    Ok(m) => self.ram = m,
                    Err(e) => fatal(1, &format!("hydo-kvm: guest RAM: {e}")),
                }
            }
        }
        {
            let live = self.ram.bytes_mut();
            for (i, &k) in s.ram.pages.iter().enumerate() {
                let o = k as usize * PAGE;
                live[o..o + PAGE].copy_from_slice(&s.ram.data[i * PAGE..(i + 1) * PAGE]);
            }
        }
        self.base = s.ram.base.clone();
        self.fresh_vm();
        self.load_cpus(s);
        self.smp = s.smp.clone();
        self.cur = s.smp.as_ref().map_or(0, |sm| sm.cur);
        self.m = s.m;
        let n = self.m.console_len as usize;
        self.console[..n].copy_from_slice(&s.con[..n]);
        self.echoed = self.m.console_len;
        if let Some(v) = &s.vio {
            self.vio.restore(v);
        }
        self.rx.clear();
        // the rewind mode's copy and log describe the RAM before: made again at the next phase
        self.rw.valid = false;
        self.rw.shadow = None;
        self.rw.ck = None;
        self.rw.replay_to = None;
        self.rw.target = None;
        self.rw.armed = false;
    }

    /// What guest RAM costs: the base's nonzero pages and RAM's own pages (pages of 4 KiB).
    pub fn ram_pages(&self) -> (usize, usize) {
        let base = self.base.as_ref().map(|b| b.pages()).unwrap_or(0);
        (base, self.ram.private_pages(self.base.as_deref()).len())
    }

    // ---------------- fingerprints ----------------
    /// All of guest RAM into the hash, page by page in order (the same value as `mix64` over the
    /// whole RAM: every page is a multiple of 8 bytes): RAM's own pages from RAM, the others
    /// from the base, the base's zero pages without reading them.
    fn ram_hash(&self, mut h: u64) -> u64 {
        static ZERO: [u8; PAGE] = [0; PAGE];
        let own = self.ram.private_pages(self.base.as_deref());
        let mut i = 0;
        for k in 0..self.ram.len() / PAGE {
            let page = if i < own.len() && own[i] as usize == k {
                i += 1;
                self.ram.page(k)
            } else {
                match self.base.as_deref().filter(|b| b.nonzero(k)) {
                    Some(b) => b.page(k),
                    None => &ZERO,
                }
            };
            h = mix64(page, h);
        }
        h
    }

    /// The run engine's fingerprint (hydo-hvm's `xfp` with this machine's parts): the console,
    /// the vCPU, the machine state without statistics, the RAM size, the devices and all RAM.
    pub fn xfp(&self) -> u64 {
        let mut s = self.cpus[0].save();
        if !self.stale_events || self.spin_state {
            // KVM's leftovers of a finished event depend on the host (CpuState docs): left out
            // since fingerprint epoch 2; an epoch-1 image keeps them unless the full-state cycles
            // (which turn the dirty-page log on at host-timed kicks) run, as at epoch 1
            s = s.without_stale_events();
        }
        let mut h = fnv(b"hydo-kvm-v1", OFFSET);
        h = fnv_u64(self.m.console_len, h);
        h = fnv(&self.console[..self.m.console_len as usize], h);
        h = s.hash(h);
        h = fnv(&self.m.without_stats().bytes(), h);
        h = fnv_u64(self.ram_size, h);
        if self.vio.attached() {
            h = self.vio.hash(h);
        }
        let mut h = self.ram_hash(h);
        if let Some(sm) = &self.smp {
            self.tsd_guard(sm);
            // several vCPUs: the other vCPUs and the several-vCPU state, after every part of a
            // one-vCPU fingerprint (none of it exists with one vCPU)
            h = fnv(b"smp", h);
            for c in &self.cpus[1..] {
                h = c.save().without_stale_events().hash(h);
            }
            h = fnv(&sm.bytes(), h);
        }
        h
    }

    /// Several vCPUs, at every fingerprint: when vCPU 0 has CR4.TSD set (the guest kernel makes
    /// user-space RDTSC fault and emulates it with the exit clock), every vCPU that started must
    /// have it too. A vCPU without it reads the host's TSC in user space with no exit to show it,
    /// so the run is not deterministic; it ends here (the determinism bug of Milestone 219, where
    /// the guest kernel set TSD on the boot CPU only). User code seldom makes an exit, so a check
    /// of user code at the turn ends would not see it.
    fn tsd_guard(&self, sm: &Smp) {
        if !self.cpus[0].tsd() {
            return;
        }
        for i in 1..self.cpus.len() {
            let up = sm.started.get(i).copied().unwrap_or(false)
                && sm.run.get(i).is_some_and(|r| *r != Run::WaitSipi);
            if up && !self.cpus[i].tsd() {
                fatal(
                    3,
                    &format!(
                        "hydo-kvm: vCPU {i} runs with CR4.TSD clear while vCPU 0 has it set: its user-space RDTSC reads the host's TSC; the guest kernel must set TSD on every CPU"
                    ),
                );
            }
        }
    }

    /// The parts of the end state, for HYDO_KVM_DIAG.
    pub fn diag_lines(&self, phase: &str) -> String {
        use std::fmt::Write as _;
        let s = self.cpu().save();
        let mut t = String::new();
        let cl = self.m.console_len;
        let _ = writeln!(
            t,
            "{phase} console {:016x} len {cl}",
            fnv(&self.console[..cl as usize], OFFSET)
        );
        let _ = writeln!(t, "{phase} cpu {:016x}", s.hash(OFFSET));
        let _ = writeln!(t, "{phase} regs {:?}", s.regs);
        let _ = writeln!(
            t,
            "{phase} sregs {:016x} xsave {:016x} xcr0 {:#x} msrs {:x?}",
            fnv(format!("{:?}", s.sregs).as_bytes(), OFFSET),
            fnv(format!("{:?}", s.xsave).as_bytes(), OFFSET),
            s.xcr0,
            s.msrs
        );
        let _ = writeln!(t, "{phase} events {:?}", s.events);
        let _ = writeln!(t, "{phase} debugregs {:x?}", s.debugregs);
        let _ = writeln!(
            t,
            "{phase} mach {:016x} exits {} vtime {}",
            fnv(&self.m.without_stats().bytes(), OFFSET),
            self.m.exits,
            self.m.vtime
        );
        if let Some(sm) = &self.smp {
            // several vCPUs: each vCPU's state (the lines above are the running vCPU's) and the
            // several-vCPU state, so that two runs' diagnostics show which part differs
            for (i, c) in self.cpus.iter().enumerate() {
                let s = c.save().without_stale_events();
                let _ = writeln!(
                    t,
                    "{phase} vcpu{i} {:016x} sregs {:016x} xsave {:016x} run {:?} regs {:?}",
                    s.hash(OFFSET),
                    fnv(format!("{:?}", s.sregs).as_bytes(), OFFSET),
                    fnv(format!("{:?}", s.xsave).as_bytes(), OFFSET),
                    sm.run[i],
                    s.regs
                );
                let _ = writeln!(
                    t,
                    "{phase} vcpu{i} apic {:016x}",
                    fnv(&sm.apic[i].bytes(), OFFSET)
                );
            }
            let _ = writeln!(
                t,
                "{phase} smp {:016x} cur {} left {} turns {} exits {:?}",
                fnv(&sm.bytes(), OFFSET),
                sm.cur,
                sm.left,
                sm.turns,
                sm.exits
            );
        }
        let ram = self.ram.bytes();
        let mut o = 0usize;
        while o < ram.len() {
            let e = (o + (1 << 20)).min(ram.len());
            let _ = writeln!(t, "{phase} ram@{o:08x} {:016x}", mix64(&ram[o..e], OFFSET));
            o = e;
        }
        t
    }

    /// The statistics line of a phase.
    pub fn stats(&self) -> String {
        let m = &self.m;
        let mut t = format!(
            "exits {} io {} mmio {} msr {} hlt {} (jumps {}) windows {} irqs {} clock reads {} unknown {} kicks {} spins resolved {}",
            m.exits, m.n_io, m.n_mmio, m.n_msr, m.n_hlt, m.n_hlt_jump, m.n_window, m.n_irq,
            m.n_clock, m.n_unknown, self.kicks, m.n_spin
        );
        if self.spin_state {
            t.push_str(&format!(
                " full-state cycles resolved {} (looks that proved nothing {})",
                self.state_cycles, self.state_gave_up
            ));
        }
        if let Some(sm) = &self.smp {
            t.push_str(&format!(
                " turns {} exits per vCPU {:?}",
                sm.turns, sm.exits
            ));
        }
        if self.rw.on || self.rw.proven != 0 {
            let r = &self.rw;
            t.push_str(&format!(
                " counter loops proven {} resolved {} (rewind mode {}, checkpoints {}, exits replayed {})",
                r.proven,
                r.resolved,
                if r.on { "on" } else { "off" },
                r.checkpoints,
                r.replayed_exits
            ));
        }
        t
    }
}

/// A counter loop ending at the last of `states` (`flows[i]` the instruction executed at
/// `states[i]`): two passes back to the same instruction that [`crate::spin::counter_loop`]
/// proves infinite, and the pass's canonical instruction ([`crate::spin::canonical_pc`]).
fn counter_of(states: &[crate::abi::kvm_regs], flows: &[crate::spin::Flow]) -> Option<Option<u64>> {
    let n = states.len().checked_sub(1)?;
    let rip = states[n].rip;
    let mut tried = 0;
    for p in 1..=(n / 2).min(1024) {
        if states[n - p].rip != rip {
            continue;
        }
        tried += 1;
        if tried > 16 {
            break;
        }
        if crate::spin::counter_loop(&states[n - 2 * p..=n], &flows[n - 2 * p..n]) {
            return Some(crate::spin::canonical_pc(&states[n - p..n]));
        }
    }
    None
}

/// The devices' view of RAM in two blocks (`hydo_hvm::virtio::Ram::high`) for a guest of more
/// than 3 GiB: the same rule as [`crate::layout::ram_offset`].
fn ram_high(ram_size: u64) -> Option<(u64, u64)> {
    let b = crate::layout::ram_blocks(ram_size);
    b.get(1).map(|&(at, off, _)| (off, at))
}

/// A guest virtual address through the guest's 4-level page tables at `cr3`: the offset in
/// `ram` (the RAM buffer, [`crate::layout::ram_offset`]) of the physical address it maps to, or
/// None when it is not mapped to RAM.
pub fn translate(ram: &[u8], cr3: u64, va: u64) -> Option<u64> {
    const ADDR: u64 = 0x000f_ffff_ffff_f000;
    let len = ram.len() as u64;
    let rd = |pa: u64| -> Option<u64> {
        let o = crate::layout::ram_offset(len, pa, 8)?;
        ram.get(o..o + 8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    let off = |pa: u64| crate::layout::ram_offset(len, pa, 1).map(|o| o as u64);
    let mut table = cr3 & ADDR;
    for (level, shift) in [(4u32, 39u32), (3, 30), (2, 21), (1, 12)] {
        let e = rd(table + ((va >> shift) & 0x1ff) * 8)?;
        if e & 1 == 0 {
            return None;
        }
        if (level == 3 || level == 2) && e & 0x80 != 0 {
            let size = 1u64 << shift;
            let pa = (e & ADDR & !(size - 1)) | (va & (size - 1));
            // a large page may point past guest RAM (the guest writes its page tables)
            return off(pa);
        }
        table = e & ADDR;
    }
    off(table | (va & 0xfff))
}

impl Drop for Vmm {
    fn drop(&mut self) {
        self.watch.stop.store(true, Ordering::Relaxed);
        if let Some(t) = &mut self.trace {
            let _ = t.flush();
        }
        self.cpus.clear();
        if let Ok(mut k) = self.kicker.lock() {
            *k = None;
        }
        if let Some(j) = self.wd.take() {
            let _ = j.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(ram: &mut [u8], gpa: u64, v: u64) {
        let o = crate::layout::ram_offset(ram.len() as u64, gpa, 8).unwrap();
        ram[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn page_tables_and_pages_above_4_gib_are_read_from_the_high_block() {
        let len = 4usize << 30; // 3 GiB low, 1 GiB at 4 GiB
        let mut ram = vec![0u8; len];
        let hi = crate::layout::RAM_HIGH_BASE;
        // PML4 at 0x1000 -> PDPT in the high block -> PD at 0x3000 -> PT in the high block
        put(&mut ram, 0x1000, (hi + 0x2000) | 3);
        put(&mut ram, hi + 0x2000, 0x3000 | 3);
        put(&mut ram, 0x3000, (hi + 0x5000) | 3);
        put(&mut ram, hi + 0x5000, (hi + 0x10_0000) | 3); // va 0 -> 4 GiB + 1 MiB
        put(&mut ram, hi + 0x5000 + 8, 0xC000_0000 | 3); // va 0x1000 -> the hole
        put(&mut ram, 0x3000 + 8, (hi + 0x20_0000) | 0x83); // va 2 MiB: a 2 MiB page
        let off = |gpa| crate::layout::ram_offset(len as u64, gpa, 1).map(|o| o as u64);
        assert_eq!(translate(&ram, 0x1000, 0x123), off(hi + 0x10_0123));
        assert_eq!(off(hi + 0x10_0123), Some(0xC000_0000 + 0x10_0123));
        assert_eq!(translate(&ram, 0x1000, 0x1000), None, "the hole is not RAM");
        assert_eq!(translate(&ram, 0x1000, 0x20_0042), off(hi + 0x20_0042));
        // below 3 GiB the offset is the address, as it always was
        let mut small = vec![0u8; 64 << 20];
        put(&mut small, 0x1000, 0x2000 | 3);
        put(&mut small, 0x2000, 0x3000 | 3);
        put(&mut small, 0x3000, 0x83);
        assert_eq!(translate(&small, 0x1000, 0x1234), Some(0x1234));
        assert_eq!(translate(&small, 0x1000, 0x40_0000), None);
    }

    #[test]
    fn the_devices_see_the_same_two_blocks() {
        use hydo_hvm::virtio::Ram;
        let len = 4u64 << 30;
        let mut b = vec![0u8; len as usize];
        let high = ram_high(len);
        assert_eq!(high, Some((0xC000_0000, 0x1_0000_0000)));
        assert_eq!(ram_high(3 << 30), None);
        let ram = Ram {
            b: &mut b,
            base: 0,
            high,
            dirty: None,
        };
        for (gpa, n) in [
            (0u64, 8u64),
            (0xC000_0000 - 8, 8),
            (0xC000_0000 - 4, 8),
            (0xC000_0000, 4),
            (0xD000_0000, 4),
            (0x1_0000_0000 - 4, 8),
            (0x1_0000_0000, 4096),
            (0x1_4000_0000 - 8, 8),
            (0x1_4000_0000 - 4, 8),
        ] {
            assert_eq!(
                ram.offset(gpa, n),
                crate::layout::ram_offset(len, gpa, n),
                "{gpa:#x}+{n}"
            );
        }
    }
}
