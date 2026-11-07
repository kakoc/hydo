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

use crate::cpu::{Cpu, CpuState, Exit, Hypervisor, Reply};
use crate::fnv::{fnv, fnv_u64, mix64, OFFSET};
use crate::layout::*;
use crate::machine::{Mach, Tjit};
use crate::mem::{Mem, RamBase, RamImage, PAGE};
use crate::msr;
use crate::pic::Pic;
use crate::uart;
use crate::{fatal, Diag};
use hydo_hvm::machine::Pct;
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
    pub cmdline: String,
    pub virtio: virtio::Config,
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
    cpu: Option<Box<dyn Cpu>>,
    pub m: Mach,
    pub step: u64,
    pub console: Vec<u8>,
    echo: Option<Box<dyn Write + Send>>,
    trace: Option<std::io::BufWriter<std::fs::File>>,
    trace_unknown: bool,
    pub tj: Tjit,
    pub pct: Pct,
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
}

/// A snapshot of the machine (the vCPU, the engine's state, RAM, the devices, the console). RAM
/// is a base and the pages that differ from it ([`RamImage`]), the disk a copy-on-write copy:
/// a snapshot costs what the run changed, not the size of RAM and the disk.
#[derive(Default)]
pub struct XSnap {
    pub cpu: CpuState,
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
        let mut vm = Vmm {
            ram,
            ram_size: o.ram_size,
            hv,
            cpu: None,
            m: Mach::default(),
            step: o.step,
            console: vec![0u8; CONSOLE_MAX],
            echo,
            trace,
            trace_unknown: o.trace_unknown,
            tj: Tjit::default(),
            pct: Pct::default(),
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
        };
        let cmdline = cmdline(&o.cmdline, vm.vio.ndev());
        match crate::boot::load(vm.ram.bytes_mut(), kernel, initrd, &cmdline) {
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
        vm.cpu_mut().load(&s);
        vm
    }

    /// A fresh VM and vCPU around the same RAM (the boot, and every restore).
    pub fn fresh_vm(&mut self) {
        drop(self.cpu.take());
        let (p, n) = (self.ram.ptr(), self.ram.len());
        // SAFETY: the RAM mapping lives as long as this value, which drops its vCPU first
        let c = match unsafe { self.hv.fresh(p, n) } {
            Ok(c) => c,
            Err(e) => fatal(1, &e),
        };
        if let Ok(mut k) = self.kicker.lock() {
            *k = Some(c.kicker());
        }
        self.cpu = Some(c);
    }

    fn cpu(&self) -> &dyn Cpu {
        self.cpu.as_deref().expect("a vCPU")
    }
    fn cpu_mut(&mut self) -> &mut dyn Cpu {
        self.cpu.as_deref_mut().expect("a vCPU")
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
        if !self.pct.on {
            return;
        }
        while self.pct.next < self.pct.n && self.pct.pt[self.pct.next as usize] <= self.m.exits {
            if self.pct.pt[self.pct.next as usize] == self.m.exits {
                let armed = self.m.timer_armed != 0;
                let due = self.timer_due();
                if armed && due > self.m.vtime {
                    self.m.vtime = due;
                    self.pct.fired += 1;
                } else if !armed {
                    self.pct.missed += 1;
                } else {
                    self.pct.fired += 1;
                }
            }
            self.pct.next += 1;
        }
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

    /// The vector the guest would take now, if any.
    pub fn irq_pending(&mut self) -> Option<u8> {
        self.update_levels();
        self.m.pic.pending()
    }

    /// The interrupt delivery rule at the top of the run loop (see the module documentation).
    fn deliver(&mut self) {
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

    /// The next clock value the guest can wait for, 0 when nothing is armed.
    pub fn next_event(&mut self) -> u64 {
        let mut next = if self.m.timer_armed != 0 {
            self.timer_due()
        } else {
            0
        };
        let now = self.m.vtime;
        let ram = Ram {
            b: self.ram.bytes_mut(),
            base: 0,
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
        let a = match value {
            None => self.m.msrs.read(index, now),
            Some(v) => self.m.msrs.write(index, v),
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
                match self.observe() {
                    crate::spin::Outcome::Resolved => {
                        self.m.n_spin += 1;
                        self.set_wait(self.spin_look);
                        self.last_exit = Instant::now();
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
        if self.rw.on == on {
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
                    let canon = (start..start + p).filter(|&i| ifs[i]).map(|i| hs[i]).min();
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
        s.cpu = self.cpu().save();
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
    }

    /// Restore a snapshot into a fresh VM (input typed but not read is dropped, as on the Mac):
    /// guest RAM mapped from the snapshot's base again (every page the runs since wrote is
    /// dropped at once) and its own pages written back.
    pub fn restore(&mut self, s: &XSnap) {
        drop(self.cpu.take());
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
        self.cpu_mut().load(&s.cpu);
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
        let s = self.cpu().save();
        let mut h = fnv(b"hydo-kvm-v1", OFFSET);
        h = fnv_u64(self.m.console_len, h);
        h = fnv(&self.console[..self.m.console_len as usize], h);
        h = s.hash(h);
        h = fnv(&self.m.without_stats().bytes(), h);
        h = fnv_u64(self.ram_size, h);
        if self.vio.attached() {
            h = self.vio.hash(h);
        }
        self.ram_hash(h)
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
            "{phase} mach {:016x} exits {} vtime {}",
            fnv(&self.m.without_stats().bytes(), OFFSET),
            self.m.exits,
            self.m.vtime
        );
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

/// A guest virtual address through the guest's 4-level page tables at `cr3`: its physical
/// address in `ram`, or None when it is not mapped.
pub fn translate(ram: &[u8], cr3: u64, va: u64) -> Option<u64> {
    const ADDR: u64 = 0x000f_ffff_ffff_f000;
    let rd = |pa: u64| -> Option<u64> {
        let o = pa as usize;
        ram.get(o..o + 8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    };
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
            return ((pa as usize) < ram.len()).then_some(pa);
        }
        table = e & ADDR;
    }
    let pa = table | (va & 0xfff);
    ((pa as usize) < ram.len()).then_some(pa)
}

impl Drop for Vmm {
    fn drop(&mut self) {
        self.watch.stop.store(true, Ordering::Relaxed);
        if let Some(t) = &mut self.trace {
            let _ = t.flush();
        }
        drop(self.cpu.take());
        if let Ok(mut k) = self.kicker.lock() {
            *k = None;
        }
        if let Some(j) = self.wd.take() {
            let _ = j.join();
        }
    }
}
