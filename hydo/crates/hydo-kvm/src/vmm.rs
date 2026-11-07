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
//! HYDO_KVM_SPIN_SECS ends the run as a SPIN (invalid), as on the Mac without its rewind mode.

use crate::cpu::{Cpu, CpuState, Exit, Hypervisor, Reply};
use crate::fnv::{fnv, fnv_u64, mix64, OFFSET};
use crate::layout::*;
use crate::machine::{Mach, Tjit};
use crate::mem::Mem;
use crate::msr;
use crate::pic::Pic;
use crate::uart;
use crate::{fatal, Diag};
use hydo_hvm::machine::Pct;
use hydo_hvm::virtio::{self, Ram, Virtio};
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
}

/// A snapshot of the machine (the vCPU, the engine's state, RAM, the devices, the console).
pub struct XSnap {
    pub cpu: CpuState,
    pub m: Mach,
    pub ram: Vec<u8>,
    pub vio: Option<virtio::Save>,
    pub con: Vec<u8>,
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
        loop {
            if self.m.halted != 0 {
                return STOP_HALT;
            }
            if self.m.exits >= self.max_exits {
                return STOP_LIMIT;
            }
            if hook(self) {
                return STOP_MARKER;
            }
            if !resume {
                self.deliver();
            }
            resume = false;
            self.watch.armed.store(true, Ordering::Relaxed);
            let e = self.cpu_mut().run();
            self.watch.armed.store(false, Ordering::Relaxed);
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
                    crate::spin::Outcome::Deadlock => {
                        let rip = self.cpu().rip();
                        self.say(&format!(
                            "hydo-kvm: spin: a proven cycle with interrupts off (deadlock) at rip {rip:#x}"
                        ));
                        return STOP_IDLE;
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
    pub fn observe(&mut self) -> crate::spin::Outcome {
        use crate::spin::{cycle, state_hash, Outcome, STEPS};
        if !self.cpu().may_step() {
            return Outcome::GaveUp;
        }
        let cr3 = self.cpu().cr3();
        let mut hs: Vec<u64> = Vec::with_capacity(64);
        let mut ifs: Vec<bool> = Vec::with_capacity(64);
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
            if target.is_none() && !again {
                hs.push(h);
                ifs.push(r.rflags & (1 << 9) != 0);
                if let Some((start, p)) = cycle(&hs) {
                    let canon = (start..start + p).filter(|&i| ifs[i]).map(|i| hs[i]).min();
                    match canon {
                        None => break Outcome::Deadlock,
                        Some(c) if c == h => break Outcome::Resolved,
                        Some(c) => target = Some(c),
                    }
                }
            }
            steps += 1;
            if steps >= STEPS {
                break Outcome::GaveUp;
            }
            let Some(step) = self.cpu().next_step(self.ram.bytes(), cr3) else {
                break Outcome::GaveUp;
            };
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
    pub fn snapshot(&self, s: &mut XSnap) {
        s.cpu = self.cpu().save();
        s.m = self.m;
        if s.ram.len() != self.ram.len() {
            s.ram = vec![0u8; self.ram.len()];
        }
        s.ram.copy_from_slice(self.ram.bytes());
        s.vio = self.vio.attached().then(|| self.vio.save());
        s.con = self.console[..self.m.console_len as usize].to_vec();
    }

    /// Restore a snapshot into a fresh VM (input typed but not read is dropped, as on the Mac).
    pub fn restore(&mut self, s: &XSnap) {
        self.fresh_vm();
        self.ram.bytes_mut().copy_from_slice(&s.ram);
        self.cpu_mut().load(&s.cpu);
        self.m = s.m;
        let n = self.m.console_len as usize;
        self.console[..n].copy_from_slice(&s.con[..n]);
        if let Some(v) = &s.vio {
            self.vio.restore(v);
        }
        self.rx.clear();
    }

    // ---------------- fingerprints ----------------
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
        mix64(self.ram.bytes(), h)
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
        format!(
            "exits {} io {} mmio {} msr {} hlt {} (jumps {}) windows {} irqs {} clock reads {} unknown {} kicks {} spins resolved {}",
            m.exits, m.n_io, m.n_mmio, m.n_msr, m.n_hlt, m.n_hlt_jump, m.n_window, m.n_irq,
            m.n_clock, m.n_unknown, self.kicks, m.n_spin
        )
    }
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
