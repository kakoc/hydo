//! hv/hvm/hvm.c in Rust: one vCPU at EL1 with an exit-count clock, an emulated GICv2, a 16550
//! UART, PSCI over HVC, the virtio devices, tick jitter and forced preemption points, the vCPU
//! state snapshot, the spin watchdog, and the run loop with its interrupt delivery rule. The
//! spin observer is in [`crate::spin`]. Every function keeps the C function's name and order of
//! effects, so a run is the same pure function of its inputs as with the C engine.

use crate::fnv::{fnv, fnv_u64, OFFSET};
use crate::hv::{self, sr, Vcpu, HV_REG_CPSR, HV_REG_FPCR, HV_REG_FPSR, HV_REG_PC, HV_REG_X0};
use crate::layout::*;
use crate::machine::{bit, setb, Mach, Pct, Tjit};
use crate::mem::Mem;
use crate::spin::Spin;
use crate::virtio::{Ram, Virtio};
use crate::{fatal, Diag};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// The system registers of a vCPU snapshot (`SR[]`), in the C order.
pub const SR: [u16; 41] = [
    sr::SCTLR_EL1,
    sr::CPACR_EL1,
    sr::TTBR0_EL1,
    sr::TTBR1_EL1,
    sr::TCR_EL1,
    sr::SPSR_EL1,
    sr::ELR_EL1,
    sr::SP_EL0,
    sr::SP_EL1,
    sr::AFSR0_EL1,
    sr::AFSR1_EL1,
    sr::ESR_EL1,
    sr::FAR_EL1,
    sr::PAR_EL1,
    sr::MAIR_EL1,
    sr::AMAIR_EL1,
    sr::VBAR_EL1,
    sr::CONTEXTIDR_EL1,
    sr::TPIDR_EL1,
    sr::CNTKCTL_EL1,
    sr::CSSELR_EL1,
    sr::TPIDR_EL0,
    sr::TPIDRRO_EL0,
    sr::CNTV_CTL_EL0,
    sr::CNTV_CVAL_EL0,
    sr::MDSCR_EL1,
    sr::MDCCINT_EL1,
    sr::ACTLR_EL1,
    sr::SCXTNUM_EL0,
    sr::SCXTNUM_EL1,
    sr::TPIDR2_EL0,
    sr::APIAKEYLO_EL1,
    sr::APIAKEYHI_EL1,
    sr::APIBKEYLO_EL1,
    sr::APIBKEYHI_EL1,
    sr::APDAKEYLO_EL1,
    sr::APDAKEYHI_EL1,
    sr::APDBKEYLO_EL1,
    sr::APDBKEYHI_EL1,
    sr::APGAKEYLO_EL1,
    sr::APGAKEYHI_EL1,
];
pub const NSR: usize = SR.len();
/// the first breakpoint/watchpoint pair: restored, not fingerprinted (`DR[]`)
pub const DR: [u16; 4] = [
    sr::DBGBVR0_EL1,
    sr::DBGBCR0_EL1,
    sr::DBGWVR0_EL1,
    sr::DBGWCR0_EL1,
];

/// `struct vsnap`: the vCPU state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VSnap {
    pub x: [u64; 31],
    pub pc: u64,
    pub cpsr: u64,
    pub fpcr: u64,
    pub fpsr: u64,
    pub sr: [u64; NSR],
    pub q: [hv::Q; 32],
    pub ok: [u8; NSR],
    pub dbg: [u64; 4],
    pub dok: [u8; 4],
}

impl Default for VSnap {
    fn default() -> VSnap {
        VSnap {
            x: [0; 31],
            pc: 0,
            cpsr: 0,
            fpcr: 0,
            fpsr: 0,
            sr: [0; NSR],
            q: [hv::Q::default(); 32],
            ok: [0; NSR],
            dbg: [0; 4],
            dok: [0; 4],
        }
    }
}

impl VSnap {
    /// The registers part of the fingerprints: x, pc, cpsr, fpcr, fpsr, q, sr, ok (in that order).
    pub fn hash_regs(&self, mut h: u64) -> u64 {
        for v in self.x {
            h = fnv_u64(v, h);
        }
        for v in [self.pc, self.cpsr, self.fpcr, self.fpsr] {
            h = fnv_u64(v, h);
        }
        for q in &self.q {
            h = fnv(&q.0, h);
        }
        for v in self.sr {
            h = fnv_u64(v, h);
        }
        fnv(&self.ok, h)
    }
}

/// The spin watchdog's shared state (`wd_exits`, `wd_armed`, `kick_backoff`, the vCPU id).
pub struct Watch {
    pub exits: AtomicU64,
    pub armed: AtomicBool,
    pub backoff: AtomicU64,
    pub vcpu: AtomicU64,
    pub stop: AtomicBool,
    pub kick_s: f64,
    pub spin_secs: f64,
    pub observe: bool,
}

impl Watch {
    pub fn backoff(&self) -> f64 {
        f64::from_bits(self.backoff.load(Ordering::Relaxed))
    }
    pub fn set_backoff(&self, v: f64) {
        self.backoff.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// Host time in seconds (`now_s`).
pub fn now_s() -> f64 {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
}

/// `watchdog()`: kick the vCPU out after a while of host time without an exit.
fn watchdog(w: Arc<Watch>) {
    let mut last = u64::MAX;
    let mut since = now_s();
    while !w.stop.load(Ordering::Relaxed) {
        let wait = if w.observe {
            w.kick_s * w.backoff()
        } else {
            w.spin_secs
        };
        std::thread::sleep(std::time::Duration::from_micros(
            (wait * 1e6 / 4.0) as u64 + 200,
        ));
        if !w.armed.load(Ordering::Relaxed) {
            last = u64::MAX;
            since = now_s();
            continue;
        }
        let x = w.exits.load(Ordering::Relaxed);
        if x != last {
            last = x;
            since = now_s();
            continue;
        }
        if now_s() - since > wait {
            hv::vcpu_kick(w.vcpu.load(Ordering::Relaxed));
            since = now_s();
        }
    }
}

/// The run loop's settings (hvm.c's environment variables, read once).
#[derive(Clone, Debug)]
pub struct VmOpts {
    pub ram_size: u64,
    pub step: u64,
    pub echo: bool,
    pub trace: Option<String>,
    pub trace_watch: Option<String>,
    pub trace_sysreg: bool,
    pub spin_secs: f64,
    pub kick_s: f64,
    pub spin_observe: bool,
    pub rw_on: bool,
    pub splog: Option<String>,
    /// HVM_SOFT_IRQ (hvm: default 0; hvmx: default 1)
    pub soft_irq: bool,
    pub bootargs: String,
    pub virtio: crate::virtio::Config,
}

/// One VM held by this process (Hypervisor.framework allows one VM per process).
static VM_HELD: AtomicBool = AtomicBool::new(false);

pub struct Vmm {
    pub ram: Mem,
    pub ram_size: u64,
    vcpu: Option<Vcpu>,
    pub m: Mach,
    pub step: u64,
    pub console: Vec<u8>,
    echo: Option<Box<dyn Write + Send>>,
    trace: Option<std::io::BufWriter<std::fs::File>>,
    trace_watch: i64,
    trace_sysreg: bool,
    pub tj: Tjit,
    pub pct: Pct,
    rxq: Box<[u8; RXQ]>,
    pub rx_head: u32,
    pub rx_tail: u32,
    pub vio: Virtio,
    soft_irq: bool,
    pub max_exits: u64,
    pub sp: Spin,
    pub watch: Arc<Watch>,
    wd: Option<std::thread::JoinHandle<()>>,
    pub diag: Diag,
    vm_live: bool,
}

/// The guest RAM view the devices use.
macro_rules! ramv {
    ($s:expr) => {
        Ram {
            b: $s.ram.bytes_mut(),
            base: RAM_BASE,
        }
    };
}

impl Vmm {
    /// The watchdog, the devices, the VM with the kernel, initrd and device tree in RAM, and a
    /// vCPU at the kernel's entry (hvm.c's main up to the run: `virtio_setup`, `vm_create`,
    /// `virtio_bind`, `vcpu_boot`; `lock` is hvmx's mlock).
    pub fn new(
        o: &VmOpts,
        kernel: &[u8],
        initrd: &[u8],
        diag: Diag,
        echo: Option<Box<dyn Write + Send>>,
        lock: bool,
    ) -> Vmm {
        let watch = Arc::new(Watch {
            exits: AtomicU64::new(0),
            armed: AtomicBool::new(false),
            backoff: AtomicU64::new(1f64.to_bits()),
            vcpu: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            kick_s: o.kick_s,
            spin_secs: o.spin_secs,
            observe: o.spin_observe,
        });
        let w2 = watch.clone();
        let wd = std::thread::Builder::new()
            .name("hvm-watchdog".into())
            .spawn(move || watchdog(w2))
            .ok();
        let vio = match Virtio::setup(&o.virtio) {
            Ok(v) => v,
            Err(e) => {
                watch.stop.store(true, Ordering::Relaxed);
                fatal(1, &e)
            }
        };
        let trace = o.trace.as_ref().and_then(|p| {
            std::fs::File::create(p)
                .ok()
                .map(std::io::BufWriter::new)
        });
        let trace_watch = o
            .trace_watch
            .as_deref()
            .map(|v| hydo_hvx::cstr::strtoull0(v.as_bytes()) as i64)
            .unwrap_or(-1);
        let ram = match Mem::anon(o.ram_size as usize) {
            Ok(m) => m,
            Err(e) => fatal(1, &format!("mmap: {e}")),
        };
        let mut vm = Vmm {
            ram,
            ram_size: o.ram_size,
            vcpu: None,
            m: Mach::default(),
            step: o.step,
            console: vec![0u8; CONSOLE_MAX],
            echo: if o.echo { echo } else { None },
            trace,
            trace_watch,
            trace_sysreg: o.trace_sysreg,
            tj: Tjit::default(),
            pct: Pct::default(),
            rxq: Box::new([0u8; RXQ]),
            rx_head: 0,
            rx_tail: 0,
            vio,
            soft_irq: o.soft_irq,
            max_exits: 200_000_000,
            sp: Spin::new(o.rw_on, o.splog.as_deref()),
            watch,
            wd,
            diag,
            vm_live: false,
        };
        if VM_HELD.swap(true, Ordering::SeqCst) {
            fatal(
                1,
                "hvm: this process already runs a VM (Hypervisor.framework allows one VM per \
                 process): run one engine per process (the hydo-hvm program)",
            );
        }
        vm.vm_live = true;
        // vm_create
        let r = hv::vm_create();
        if r != 0 {
            let hint = if r as u32 == 0xfae9_4007 {
                " (HV_DENIED: the program is not signed with the hypervisor entitlement; \
                 codesign --force --entitlements hv/hvm/hv.entitlements -s - <program>)"
            } else {
                ""
            };
            fatal(1, &format!("FAIL hv_vm_create(NULL) = 0x{:x}{hint}", r as u32));
        }
        vm.map_ram();
        let b = vm.ram.bytes_mut();
        let k = (KERNEL_IPA - RAM_BASE) as usize;
        let i = (INITRD_IPA - RAM_BASE) as usize;
        if k + kernel.len() > b.len() || i + initrd.len() > b.len() {
            fatal(1, "hvm: the kernel or the initramfs does not fit in guest RAM");
        }
        b[k..k + kernel.len()].copy_from_slice(kernel);
        b[i..i + initrd.len()].copy_from_slice(initrd);
        let devs: Vec<(u64, u32)> = (0..vm.vio.ndev()).map(Virtio::dev_info).collect();
        let fdt = crate::fdt::build(
            &o.bootargs,
            INITRD_IPA,
            INITRD_IPA + initrd.len() as u64,
            o.ram_size,
            &devs,
        );
        let d = (DTB_IPA - RAM_BASE) as usize;
        b[d..d + fdt.len()].copy_from_slice(&fdt);
        vm.vio.bind(o.step);
        if lock {
            if let Err(e) = vm.ram.lock() {
                vm.say(&format!("hvmx: mlock guest RAM (continuing): {e}"));
            }
        }
        vm.vcpu_boot();
        vm
    }

    fn map_ram(&mut self) {
        // SAFETY: the RAM mapping lives as long as this value, which destroys the VM first
        let r = unsafe { hv::vm_map(self.ram.ptr(), RAM_BASE, self.ram_size as usize, hv::RWX) };
        hv::chk(
            "hv_vm_map(ram, RAM_BASE, ram_size, HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC)",
            r,
        );
    }

    fn vcpu_create(&mut self) {
        let v = Vcpu::create();
        self.watch.vcpu.store(v.id(), Ordering::Relaxed);
        self.vcpu = Some(v);
        self.sp.trap_dbg_applied = -1;
    }

    /// `vcpu_boot`.
    fn vcpu_boot(&mut self) {
        self.vcpu_create();
        self.wr(HV_REG_PC, KERNEL_IPA);
        self.wr(HV_REG_CPSR, 0x3c5); // EL1h, DAIF masked
        self.wr(HV_REG_X0, DTB_IPA);
        self.wr(HV_REG_X0 + 1, 0);
        self.wr(HV_REG_X0 + 2, 0);
        self.wr(HV_REG_X0 + 3, 0);
        self.m = Mach::default();
        self.m.gic.pmr = 0;
    }

    /// A fresh VM around the same RAM and a fresh vCPU (xsnap_restore's first half).
    pub fn fresh_vm(&mut self) {
        if let Some(v) = self.vcpu.take() {
            v.destroy();
        }
        hv::chk("hv_vm_destroy()", hv::vm_destroy());
        hv::chk("hv_vm_create(NULL)", hv::vm_create());
        self.map_ram();
        self.vcpu_create();
    }

    /// A fresh vCPU in the same VM (hvm's branch mode).
    pub fn fresh_vcpu(&mut self) {
        if let Some(v) = self.vcpu.take() {
            v.destroy();
        }
        self.vcpu_create();
    }

    pub fn v(&self) -> &Vcpu {
        self.vcpu.as_ref().expect("a vCPU")
    }

    /// One diagnostic line (stderr in the program).
    pub fn say(&self, s: &str) {
        crate::say(&self.diag, s);
    }

    /// A line in the HVM_TRACE file.
    pub fn trace_line(&mut self, s: &str) {
        if let Some(t) = &mut self.trace {
            let _ = writeln!(t, "{s}");
        }
    }

    // ---------------- helpers ----------------
    pub fn rd(&self, r: u32) -> u64 {
        self.v().get(r)
    }
    pub fn wr(&self, r: u32, v: u64) {
        self.v().set(r, v)
    }
    pub fn xget(&self, n: u32) -> u64 {
        if n == 31 {
            0
        } else {
            self.rd(HV_REG_X0 + n)
        }
    }
    pub fn xset(&self, n: u32, v: u64) {
        if n != 31 {
            self.wr(HV_REG_X0 + n, v)
        }
    }
    pub fn skip(&self) {
        self.wr(HV_REG_PC, self.rd(HV_REG_PC).wrapping_add(4));
    }
    pub fn esr(&self) -> u64 {
        self.v().exit().syndrome
    }
    pub fn ram_view(&mut self) -> Ram<'_> {
        ramv!(self)
    }

    // ---------------- tick jitter, preemption points, the timer ----------------
    /// the virtual time at which the timer interrupt is raised: the deadline, plus the delay
    pub fn timer_due(&self) -> u64 {
        if !self.tj.on {
            return self.m.cntp_cval;
        }
        self.m.cntp_cval.wrapping_add(
            self.tj
                .delay(self.m.cntp_cval / TJ_PERIOD, self.step)
                .wrapping_mul(self.step),
        )
    }
    /// called once per exit, after the exit is counted
    fn pct_at_exit(&mut self) {
        if !self.pct.on {
            return;
        }
        while self.pct.next < self.pct.n && self.pct.pt[self.pct.next as usize] <= self.m.exits {
            if self.pct.pt[self.pct.next as usize] == self.m.exits {
                let armed = (self.m.cntp_ctl & 3) == 1;
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
        let en = self.m.cntp_ctl & 1 != 0;
        let mask = (self.m.cntp_ctl >> 1) & 1 != 0;
        if !self.tj.on {
            return en && !mask && self.m.vtime >= self.m.cntp_cval;
        }
        let lv = en && !mask && self.m.vtime >= self.timer_due();
        if lv
            && self.m.vtime >= self.m.cntp_cval
            && self.tj.delay(self.m.cntp_cval / TJ_PERIOD, self.step) != 0
        {
            let k = self.m.cntp_cval / TJ_PERIOD;
            if k != self.tj.last_applied_k {
                self.tj.applied += 1;
                self.tj.last_applied_k = k;
            }
        }
        lv
    }

    // ---------------- GICv2 ----------------
    /// level-triggered sources feed the pending bit continuously
    fn gic_update_levels(&mut self) {
        let t = self.timer_level();
        setb(&mut self.m.gic.pending, TIMER_INTID, t);
        let u = self.uart_level();
        setb(&mut self.m.gic.pending, UART_INTID, u);
        let now = self.m.vtime;
        self.vio.poll(&mut ramv!(self), now);
        for d in 0..self.vio.ndev() {
            let (_, spi) = Virtio::dev_info(d);
            let l = self.vio.level(d);
            setb(&mut self.m.gic.pending, 32 + spi as usize, l);
        }
    }
    /// the highest-priority pending, enabled, inactive interrupt, or 1023
    fn gic_best(&self) -> u32 {
        let g = &self.m.gic;
        let (mut best, mut bp) = (1023u32, 256i32);
        for i in 0..NIRQ {
            if bit(&g.pending, i)
                && bit(&g.enabled, i)
                && !bit(&g.active, i)
                && (g.prio[i] as i32) < bp
            {
                best = i as u32;
                bp = g.prio[i] as i32;
            }
        }
        if best != 1023 && bp >= (g.pmr & 0xff) as i32 {
            return 1023;
        }
        best
    }
    fn gic_any_active(&self) -> bool {
        self.m.gic.active.iter().any(|&a| a != 0)
    }
    pub fn irq_line(&mut self) -> bool {
        self.gic_update_levels();
        if self.m.gic.dctlr & 1 == 0 || self.m.gic.cctlr & 1 == 0 {
            return false;
        }
        if self.gic_any_active() {
            return false; // Linux handles one IRQ at a time (non-split EOI)
        }
        self.gic_best() != 1023
    }

    fn gicd_read(&mut self, off: u64, size: u32) -> u32 {
        let n8 = (NIRQ / 8) as u64;
        let ix = |base: u64| ((off - base) / 4) as usize;
        if off == 0x000 {
            return self.m.gic.dctlr;
        }
        if off == 0x004 {
            return (NIRQ / 32) as u32 - 1;
        }
        if off == 0x008 {
            return 0x0200_143b;
        }
        if (0x080..0x080 + n8).contains(&off) {
            return self.m.gic.group[ix(0x080)];
        }
        if (0x100..0x100 + n8).contains(&off) {
            return self.m.gic.enabled[ix(0x100)];
        }
        if (0x180..0x180 + n8).contains(&off) {
            return self.m.gic.enabled[ix(0x180)];
        }
        if (0x200..0x200 + n8).contains(&off) {
            self.gic_update_levels();
            return self.m.gic.pending[ix(0x200)];
        }
        if (0x280..0x280 + n8).contains(&off) {
            self.gic_update_levels();
            return self.m.gic.pending[ix(0x280)];
        }
        if (0x300..0x300 + n8).contains(&off) {
            return self.m.gic.active[ix(0x300)];
        }
        if (0x380..0x380 + n8).contains(&off) {
            return self.m.gic.active[ix(0x380)];
        }
        if (0x400..0x400 + NIRQ as u64).contains(&off) {
            let mut v = 0u32;
            for k in 0..size as usize {
                let i = (off - 0x400) as usize + k;
                // C reads past the array's end for an access at its last bytes (the next field)
                let b = if i < NIRQ {
                    self.m.gic.prio[i]
                } else {
                    self.m.gic.targets[i - NIRQ]
                };
                v |= (b as u32) << (8 * k);
            }
            return v;
        }
        if (0x800..0x800 + NIRQ as u64).contains(&off) {
            let mut v = 0u32;
            for k in 0..size as usize {
                let i = (off - 0x800) as usize + k;
                let t = if i < 32 {
                    1
                } else if i < NIRQ {
                    self.m.gic.targets[i]
                } else {
                    // past the end of the struct's last array: the padding byte's neighbours
                    self.tail_byte(i - NIRQ)
                };
                v |= (t as u32) << (8 * k);
            }
            return v;
        }
        if (0xc00..0xc00 + (NIRQ / 4) as u64).contains(&off) {
            return self.m.gic.cfg[ix(0xc00)];
        }
        0
    }
    /// The bytes of `struct mach` after `gic.targets` (the UART, then padding), which a C access
    /// at the last bytes of `targets` reads.
    fn tail_byte(&self, j: usize) -> u8 {
        let u = &self.m.uart;
        [u.ier, u.lcr, u.mcr, u.scr, u.dll, u.dlm, u.fcr, 0]
            .get(j)
            .copied()
            .unwrap_or(0)
    }
    fn gicd_write(&mut self, off: u64, v: u32, size: u32) {
        let n8 = (NIRQ / 8) as u64;
        let ix = |base: u64| ((off - base) / 4) as usize;
        let g = &mut self.m.gic;
        if off == 0x000 {
            g.dctlr = v & 1;
        } else if (0x080..0x080 + n8).contains(&off) {
            g.group[ix(0x080)] = v;
        } else if (0x100..0x100 + n8).contains(&off) {
            g.enabled[ix(0x100)] |= v;
        } else if (0x180..0x180 + n8).contains(&off) {
            g.enabled[ix(0x180)] &= !v;
        } else if (0x200..0x200 + n8).contains(&off) {
            g.pending[ix(0x200)] |= v;
        } else if (0x280..0x280 + n8).contains(&off) {
            g.pending[ix(0x280)] &= !v;
        } else if (0x300..0x300 + n8).contains(&off) {
            g.active[ix(0x300)] |= v;
        } else if (0x380..0x380 + n8).contains(&off) {
            g.active[ix(0x380)] &= !v;
        } else if (0x400..0x400 + NIRQ as u64).contains(&off) {
            for k in 0..size as usize {
                let i = (off - 0x400) as usize + k;
                if i < NIRQ {
                    g.prio[i] = (v >> (8 * k)) as u8;
                } else {
                    g.targets[i - NIRQ] = (v >> (8 * k)) as u8;
                }
            }
        } else if (0x800..0x800 + NIRQ as u64).contains(&off) {
            for k in 0..size as usize {
                let i = (off - 0x800) as usize + k;
                let b = (v >> (8 * k)) as u8;
                if i < NIRQ {
                    g.targets[i] = b;
                } else {
                    let u = &mut self.m.uart;
                    match i - NIRQ {
                        0 => u.ier = b,
                        1 => u.lcr = b,
                        2 => u.mcr = b,
                        3 => u.scr = b,
                        4 => u.dll = b,
                        5 => u.dlm = b,
                        6 => u.fcr = b,
                        _ => {}
                    }
                }
            }
        } else if (0xc00..0xc00 + (NIRQ / 4) as u64).contains(&off) {
            g.cfg[ix(0xc00)] = v;
        } else if off == 0xf00 {
            // GICD_SGIR: one CPU, so any target that includes us
            let filter = (v >> 24) & 3;
            let list = (v >> 16) & 0xff;
            let id = (v & 0xf) as usize;
            if filter == 2 || (filter == 0 && list & 1 != 0) {
                setb(&mut g.pending, id, true);
            }
        }
    }
    fn gicc_read(&mut self, off: u64) -> u32 {
        match off {
            0x00 => self.m.gic.cctlr,
            0x04 => self.m.gic.pmr,
            0x08 => self.m.gic.bpr,
            0x0c => {
                self.gic_update_levels();
                let i = self.gic_best();
                if i == 1023 {
                    return 1023;
                }
                if self.trace.is_some() {
                    let elr = self.v().sys(sr::ELR_EL1, 0);
                    let spsr = self.v().sys(sr::SPSR_EL1, 0);
                    let ex = self.m.exits;
                    if let Some(t) = &mut self.trace {
                        let _ = writeln!(
                            t,
                            "IAR {i} at exit {ex}: interrupted pc {elr:x} spsr {spsr:x}"
                        );
                    }
                }
                setb(&mut self.m.gic.active, i as usize, true);
                setb(&mut self.m.gic.pending, i as usize, false);
                i
            }
            0x14 => 0xff,
            0x18 => {
                self.gic_update_levels();
                self.gic_best()
            }
            0xfc => 0x0202_143b,
            _ => 0,
        }
    }
    fn gicc_write(&mut self, off: u64, v: u32) {
        match off {
            0x00 => self.m.gic.cctlr = v,
            0x04 => self.m.gic.pmr = v & 0xff,
            0x08 => self.m.gic.bpr = v,
            0x10 | 0x1000 => {
                if ((v & 0x3ff) as usize) < NIRQ {
                    setb(&mut self.m.gic.active, (v & 0x3ff) as usize, false);
                }
            }
            _ => {}
        }
    }

    // ---------------- 16550 UART ----------------
    fn rx_ready(&self) -> bool {
        self.rx_head != self.rx_tail
    }
    /// `hvm_put_input`: bytes the host types on the console, up to a NUL or a full queue.
    pub fn put_input(&mut self, s: &[u8]) {
        for &c in s {
            if c == 0 || self.rx_tail.wrapping_sub(self.rx_head) >= RXQ as u32 {
                break;
            }
            self.rxq[self.rx_tail as usize % RXQ] = c;
            self.rx_tail = self.rx_tail.wrapping_add(1);
        }
    }
    /// Room in the receive queue, and one byte typed (the lock-step SEND).
    pub fn rx_room(&self) -> u32 {
        (RXQ as u32).wrapping_sub(self.rx_tail.wrapping_sub(self.rx_head))
    }
    pub fn rx_push(&mut self, c: u8) {
        self.rxq[self.rx_tail as usize % RXQ] = c;
        self.rx_tail = self.rx_tail.wrapping_add(1);
    }
    fn uart_level(&self) -> bool {
        (self.m.uart.ier & 0x02) != 0 || ((self.m.uart.ier & 0x01) != 0 && self.rx_ready())
    }
    fn uart_read(&mut self, off: u64) -> u8 {
        let dlab = self.m.uart.lcr & 0x80 != 0;
        let u = self.m.uart;
        match off {
            0 => {
                if dlab {
                    return u.dll;
                }
                if self.rx_ready() {
                    let c = self.rxq[self.rx_head as usize % RXQ];
                    self.rx_head = self.rx_head.wrapping_add(1);
                    c
                } else {
                    0
                }
            }
            1 => {
                if dlab {
                    u.dlm
                } else {
                    u.ier
                }
            }
            2 => {
                let fifo = if u.fcr & 1 != 0 { 0xc0 } else { 0 };
                if u.ier & 0x01 != 0 && self.rx_ready() {
                    return fifo | 0x04;
                }
                fifo | if u.ier & 0x02 != 0 { 0x02 } else { 0x01 }
            }
            3 => u.lcr,
            4 => u.mcr,
            5 => 0x60 | if self.rx_ready() { 0x01 } else { 0 },
            6 => {
                if u.mcr & 0x10 != 0 {
                    let m = u.mcr;
                    ((m & 2) << 3) | ((m & 1) << 5) | ((m & 4) << 4) | ((m & 8) << 4)
                } else {
                    0xb0
                }
            }
            7 => u.scr,
            _ => 0,
        }
    }
    fn uart_write(&mut self, off: u64, v: u8) {
        let dlab = self.m.uart.lcr & 0x80 != 0;
        match off {
            0 => {
                if dlab {
                    self.m.uart.dll = v;
                    return;
                }
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
            1 => {
                if dlab {
                    self.m.uart.dlm = v
                } else {
                    self.m.uart.ier = v & 0x0f
                }
            }
            2 => self.m.uart.fcr = v,
            3 => self.m.uart.lcr = v,
            4 => self.m.uart.mcr = v,
            7 => self.m.uart.scr = v,
            _ => {}
        }
    }

    // ---------------- exit handlers ----------------
    fn handle_mmio(&mut self) {
        let e = self.v().exit();
        let (s, ipa) = (e.syndrome, e.pa);
        if (s >> 24) & 1 == 0 {
            let pc = self.rd(HV_REG_PC);
            fatal(
                3,
                &format!("hvm: MMIO abort without ISV at ipa 0x{ipa:x} pc 0x{pc:x}"),
            );
        }
        let size = 1u32 << ((s >> 22) & 3);
        let rt = ((s >> 16) & 31) as u32;
        let write = (s >> 6) & 1 != 0;
        self.m.n_mmio += 1;
        if (GICD_BASE..GICD_BASE + 0x1000).contains(&ipa) {
            if write {
                let v = self.xget(rt) as u32;
                self.gicd_write(ipa - GICD_BASE, v, size);
            } else {
                let v = self.gicd_read(ipa - GICD_BASE, size);
                self.xset(rt, v as u64);
            }
        } else if (GICC_BASE..GICC_BASE + 0x2000).contains(&ipa) {
            if write {
                let v = self.xget(rt) as u32;
                self.gicc_write(ipa - GICC_BASE, v);
            } else {
                let v = self.gicc_read(ipa - GICC_BASE);
                self.xset(rt, v as u64);
            }
        } else if (UART_BASE..UART_BASE + 0x100).contains(&ipa) {
            if write {
                let v = self.xget(rt) as u8;
                self.uart_write(ipa - UART_BASE, v);
            } else {
                let v = self.uart_read(ipa - UART_BASE);
                self.xset(rt, v as u64);
            }
        } else {
            let mut v = if write { self.xget(rt) } else { 0 };
            let now = self.m.vtime;
            let ours = self.vio.mmio(&mut ramv!(self), ipa, write, size, &mut v, now);
            if ours {
                if !write {
                    self.xset(rt, v);
                }
            } else if !write {
                self.xset(rt, 0); // unmapped: reads as zero, writes ignored
            }
        }
        self.skip();
    }

    fn handle_sysreg(&mut self) {
        let s = self.esr();
        let op0 = (s >> 20) & 3;
        let op2 = (s >> 17) & 7;
        let op1 = (s >> 14) & 7;
        let crn = (s >> 10) & 15;
        let rt = ((s >> 5) & 31) as u32;
        let crm = (s >> 1) & 15;
        let rdir = s & 1 != 0;
        if op0 == 3 && op1 == 3 && crn == 14 {
            if crm == 0 && op2 == 1 && rdir {
                self.m.n_cntpct += 1;
                self.xset(rt, self.m.vtime);
                self.skip();
                return;
            }
            if crm == 0 && op2 == 0 && rdir {
                self.xset(rt, CNTFRQ);
                self.skip();
                return;
            }
            if crm == 2 {
                if op2 == 1 {
                    if rdir {
                        let st = if self.m.vtime >= self.m.cntp_cval && self.m.cntp_ctl & 1 != 0 {
                            4
                        } else {
                            0
                        };
                        self.xset(rt, (self.m.cntp_ctl & 3) | st);
                    } else {
                        self.m.cntp_ctl = self.xget(rt) & 3;
                    }
                    self.skip();
                    return;
                }
                if op2 == 2 {
                    if rdir {
                        self.xset(rt, self.m.cntp_cval);
                    } else {
                        self.m.cntp_cval = self.xget(rt);
                    }
                    self.skip();
                    return;
                }
                if op2 == 0 {
                    if rdir {
                        self.xset(rt, self.m.cntp_cval.wrapping_sub(self.m.vtime) as u32 as u64);
                    } else {
                        self.m.cntp_cval = self
                            .m
                            .vtime
                            .wrapping_add(self.xget(rt) as i32 as i64 as u64);
                    }
                    self.skip();
                    return;
                }
            }
        }
        self.m.n_sysreg_other += 1;
        if self.trace_sysreg {
            let pc = self.rd(HV_REG_PC);
            self.say(&format!(
                "hvm: sysreg {} op0={op0} op1={op1} CRn={crn} CRm={crm} op2={op2} x{rt} pc=0x{pc:x} -> RAZ/WI",
                if rdir { "read" } else { "write" }
            ));
        }
        if rdir {
            self.xset(rt, 0);
        }
        self.skip();
    }

    /// Returns true when the guest idles with nothing armed (like handle_wfi).
    fn handle_hvc(&mut self) -> bool {
        let f = self.rd(HV_REG_X0);
        self.m.n_hvc += 1;
        if f as u32 == 0xc600_0020 {
            // hydo idle (CONFIG_ARM64_HVM_IDLE_HVC): a WFI that always exits
            self.wr(HV_REG_X0, 0);
            return self.idle_wait();
        }
        let ret: u64 = match f as u32 {
            0x8400_0000 => 0x0000_0002,
            0x8400_0006 => 2,
            0x8400_0008 | 0x8400_0009 => {
                self.m.halted = f as u32;
                0
            }
            0x8400_0001 => 0,
            _ => u64::MAX,
        };
        self.wr(HV_REG_X0, ret);
        false
    }

    // ---------------- interrupt delivery: stepping to the unmasking instruction ----------------
    fn start_stepping(&mut self) {
        if self.m.stepping != 0 {
            return;
        }
        self.m.stepping = 1;
        self.m.n_step_runs += 1;
        self.m.mdscr_saved = self.v().sys(sr::MDSCR_EL1, self.m.mdscr_saved);
    }
    fn stop_stepping(&mut self) {
        self.m.stepping = 0;
        self.v().set_sys(
            "hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, M.mdscr_saved)",
            sr::MDSCR_EL1,
            self.m.mdscr_saved,
        );
        self.wr(HV_REG_CPSR, self.rd(HV_REG_CPSR) & !(1u64 << 21));
    }
    pub fn apply_stepping(&mut self) {
        let want = self.m.stepping != 0 || self.sp.ob_on || self.sp.ob_stepover || self.sp.rp_on;
        let w = want as i32;
        if self.sp.trap_dbg_applied != w {
            self.v().trap_debug_exceptions(want);
            self.sp.trap_dbg_applied = w;
        }
        if self.m.stepping != 0 {
            self.v().set_sys(
                "hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_MDSCR_EL1, M.mdscr_saved | 1 | (1 << 13))",
                sr::MDSCR_EL1,
                self.m.mdscr_saved | 1 | (1 << 13),
            );
            self.wr(HV_REG_CPSR, self.rd(HV_REG_CPSR) | (1u64 << 21));
        }
    }

    fn handle_wfi(&mut self) -> bool {
        self.skip();
        self.idle_wait()
    }
    /// The virtual time of the next event the guest can wait for, 0 if nothing is armed.
    pub fn next_event(&mut self) -> u64 {
        let mut next = if (self.m.cntp_ctl & 3) == 1 {
            self.timer_due()
        } else {
            0
        };
        let now = self.m.vtime;
        let vd = self.vio.next_due(&ramv!(self), now);
        if vd != 0 && (next == 0 || vd < next) {
            next = vd;
        }
        next
    }
    fn idle_wait(&mut self) -> bool {
        self.m.n_wfi += 1;
        if self.irq_line() {
            if self.rd(HV_REG_CPSR) & 0x80 != 0 {
                self.start_stepping();
            }
            return false;
        }
        let next = self.next_event();
        if next > self.m.vtime {
            self.m.vtime = next;
            self.m.n_wfi_jump += 1;
            return false;
        }
        next == 0 // 0: idle with nothing armed
    }

    /// HVM_SOFT_IRQ=1: deliver an IRQ by emulating the exception entry in software.
    fn inject_irq_soft(&mut self) -> bool {
        let cpsr = self.rd(HV_REG_CPSR);
        let pc = self.rd(HV_REG_PC);
        if cpsr & 0x10 != 0 {
            return false;
        }
        let off = match cpsr & 0xf {
            0x0 => 0x480,
            0x4 => 0x080,
            0x5 => 0x280,
            _ => return false,
        };
        let v = self.v();
        let vbar = v.sys_chk(
            "hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_VBAR_EL1, &vbar)",
            sr::VBAR_EL1,
        );
        let sctlr = v.sys_chk(
            "hv_vcpu_get_sys_reg(vcpu, HV_SYS_REG_SCTLR_EL1, &sctlr)",
            sr::SCTLR_EL1,
        );
        v.set_sys(
            "hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_SPSR_EL1, cpsr)",
            sr::SPSR_EL1,
            cpsr,
        );
        v.set_sys(
            "hv_vcpu_set_sys_reg(vcpu, HV_SYS_REG_ELR_EL1, pc)",
            sr::ELR_EL1,
            pc,
        );
        let mut n = 0u64;
        n |= cpsr & (1 << 24);
        n |= cpsr & (1 << 22);
        if sctlr & (1 << 23) == 0 {
            n |= 1 << 22;
        }
        if sctlr & (1 << 44) != 0 {
            n |= 1 << 12;
        }
        n |= 0x3c0 | 0x5;
        self.wr(HV_REG_CPSR, n);
        self.wr(HV_REG_PC, vbar.wrapping_add(off));
        true
    }

    // ---------------- the run loop ----------------
    /// `run_until(marker)` with `hvm_stop_hook` = `hook`: the stop reason (`STOP_*`).
    pub fn run_until(
        &mut self,
        marker: Option<&[u8]>,
        hook: &mut dyn FnMut(&mut Vmm) -> bool,
    ) -> i32 {
        let mlen = marker.map(|m| m.len()).unwrap_or(0);
        let mut scan_from = 0usize;
        let mut resume = false;
        let mut line = false;
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
            if let Some(mk) = marker {
                let len = self.m.console_len as usize;
                if len >= mlen {
                    let start = scan_from.saturating_sub(mlen);
                    if hydo_hvx::cstr::find(&self.console[start..len], mk).is_some() {
                        return STOP_MARKER;
                    }
                    scan_from = len;
                }
            }
            if !resume {
                line = self.irq_line() && self.rd(HV_REG_CPSR) & 0x80 == 0;
                self.m.n_irq_line += line as u64;
                if line && self.soft_irq && self.m.stepping == 0 && self.inject_irq_soft() {
                    line = false;
                }
                if self.sp.rw_on && !self.sp.ob_on && !self.sp.rp_on && !self.sp.ob_stepover {
                    self.ck_take();
                }
            }
            resume = false;
            self.apply_stepping();
            self.v().set_pending_irq(line);
            self.watch.armed.store(true, Ordering::Relaxed);
            self.v().run();
            self.watch.armed.store(false, Ordering::Relaxed);
            let e = self.v().exit();
            if e.reason != hv::HV_EXIT_REASON_CANCELED {
                self.watch.exits.fetch_add(1, Ordering::Relaxed);
            }
            if e.reason == hv::HV_EXIT_REASON_CANCELED {
                // a kick: no exit for a while of host time. It changes nothing in the guest.
                self.sp.kicks += 1;
                resume = true;
                let obs = self.watch.observe;
                if obs && (self.m.stepping != 0 || self.sp.ob_stepover || self.sp.rp_on) {
                    continue;
                }
                if obs
                    && (!self.sp.ob_on || self.sp.ob_hits == 0 || self.rd(HV_REG_PC) != self.sp.ob_pc)
                {
                    let pc = self.rd(HV_REG_PC);
                    self.ob_arm(pc);
                }
                let b = self.watch.backoff();
                if obs && b * self.watch.kick_s < self.watch.spin_secs {
                    self.watch.set_backoff(b * 2.0);
                    continue;
                }
                let (pc, cpsr) = (self.rd(HV_REG_PC), self.rd(HV_REG_CPSR));
                self.say(&format!(
                    "hvm: SPIN: no VM exit and no provable loop for {:.0} s of host time at exit {}, pc 0x{pc:x} cpsr 0x{cpsr:x} -- \
                     the guest loops without exits; this run is not deterministic from here and is invalid",
                    self.watch.spin_secs, self.m.exits
                ));
                self.ob_disarm();
                return STOP_SPIN;
            }
            let e2 = (e.syndrome >> 26) & 0x3f;
            let in_ram = e.pa >= RAM_BASE && e.pa < RAM_BASE + self.ram_size;
            if e.reason == hv::HV_EXIT_REASON_EXCEPTION
                && self.sp.rw_on
                && self.sp.ck_valid
                && !self.sp.protected
                && e2 == 0x24
                && in_ram
            {
                // checkpoint copy-on-write
                let pg = e.pa & !(PG - 1);
                self.ck_note(pg);
                hv::vm_protect(pg, PG, hv::RWX);
                resume = true;
                continue;
            }
            if e.reason == hv::HV_EXIT_REASON_EXCEPTION && self.sp.rp_on {
                // replaying from the checkpoint to a counter loop
                if self.sp.ob_stepover && (e2 == 0x32 || e2 == 0x33) {
                    self.ob_stepover_done();
                    resume = true;
                    continue;
                }
                if e2 == 0x18 {
                    self.sp.rp_on = false;
                    self.sp.ob_on = true;
                    let h = self.ob_debug_reg();
                    self.sp.ob_on = false;
                    self.sp.rp_on = true;
                    if h {
                        resume = true;
                        continue;
                    }
                }
                if !self.sp.ob_stepover && (e2 == 0x30 || e2 == 0x31) {
                    let r = self.rp_hit();
                    if r == crate::spin::Ob::Resolved {
                        self.watch.set_backoff(1.0);
                        continue;
                    }
                    if r == crate::spin::Ob::Deadlock {
                        let pc = self.rd(HV_REG_PC);
                        self.say(&format!(
                            "hvm: spin: counter loop with nothing deliverable at pc 0x{pc:x}"
                        ));
                        return STOP_IDLE;
                    }
                    resume = true;
                    continue;
                }
                fatal(3, &format!("hvm: spin: replay from the checkpoint took a real exit (EC 0x{e2:x}) before the loop -- nondeterminism, abort"));
            }
            if e.reason == hv::HV_EXIT_REASON_EXCEPTION && (self.sp.ob_on || self.sp.ob_stepover) {
                if e2 == 0x24 && self.sp.protected && in_ram {
                    // our write protection
                    self.dirty_add(e.pa & !(PG - 1));
                    resume = true;
                    continue;
                }
                if self.sp.ob_stepover && (e2 == 0x32 || e2 == 0x33) {
                    self.ob_stepover_done();
                    resume = true;
                    continue;
                }
                if e2 == 0x18 && self.sp.ob_on && self.ob_debug_reg() {
                    resume = true;
                    continue;
                }
                if self.sp.ob_on && !self.sp.ob_stepover && (e2 == 0x30 || e2 == 0x31) {
                    use crate::spin::Ob;
                    match self.ob_hit() {
                        Ob::Resolved => {
                            self.watch.set_backoff(1.0);
                            continue;
                        }
                        Ob::Rewound => {
                            resume = false;
                            self.watch.set_backoff(1.0);
                            continue;
                        }
                        Ob::NeedRewind => {
                            let pc = self.rd(HV_REG_PC);
                            self.say(&format!(
                                "hvm: SPIN: a counter loop (proven infinite) at pc 0x{pc:x}, exit {} -- only the rewind mode \
                                 (HVM_SPIN_REWIND=1) resolves it; this run is invalid",
                                self.m.exits
                            ));
                            return STOP_SPIN;
                        }
                        Ob::Deadlock => {
                            let pc = self.rd(HV_REG_PC);
                            self.say(&format!("hvm: spin: provable loop with nothing deliverable (deadlock) at pc 0x{pc:x}"));
                            return STOP_IDLE;
                        }
                        _ => {
                            resume = true;
                            continue;
                        }
                    }
                }
                self.ob_disarm(); // a real exit: the guest is not spinning; handle it
            }
            self.m.exits += 1;
            self.m.vtime = self.m.vtime.wrapping_add(self.step);
            self.pct_at_exit();
            self.watch.set_backoff(1.0);
            if e.reason != hv::HV_EXIT_REASON_EXCEPTION {
                let pc = self.rd(HV_REG_PC);
                fatal(
                    3,
                    &format!(
                        "hvm: unexpected exit reason {} after {} exits (pc 0x{pc:x})",
                        e.reason, self.m.exits
                    ),
                );
            }
            let ec = e2;
            if self.trace.is_some() {
                let (pc, cpsr) = (self.rd(HV_REG_PC), self.rd(HV_REG_CPSR));
                let ex = self.m.exits;
                let w = if self.trace_watch >= 0 {
                    let o = self.trace_watch as usize;
                    let b = self.ram.bytes();
                    Some(u64::from_le_bytes(b[o..o + 8].try_into().unwrap()))
                } else {
                    None
                };
                if let Some(t) = &mut self.trace {
                    let _ = match w {
                        Some(w) => writeln!(t, "{ex} {ec:x} {pc:x} {cpsr:x} {} w={w:x}", line as i32),
                        None => writeln!(t, "{ex} {ec:x} {pc:x} {cpsr:x} {}", line as i32),
                    };
                }
            }
            match ec {
                0x24 => self.handle_mmio(),
                0x18 => self.handle_sysreg(),
                0x16 => {
                    if self.handle_hvc() {
                        return STOP_IDLE;
                    }
                }
                0x01 => {
                    if self.handle_wfi() {
                        return STOP_IDLE;
                    }
                }
                0x32 | 0x33 => {
                    self.m.n_steps += 1;
                    let pc = self.rd(HV_REG_PC);
                    let mut insn = 0u32;
                    if pc >= KIMAGE_VA {
                        let o = pc - KIMAGE_VA + (KERNEL_IPA - RAM_BASE);
                        if o + 4 <= self.ram_size {
                            let b = self.ram.bytes();
                            insn = u32::from_le_bytes(b[o as usize..o as usize + 4].try_into().unwrap());
                        }
                    }
                    if (insn & 0x3fc0_0000) == 0x0840_0000 {
                        fatal(3, &format!("hvm: single-step reached an exclusive load at pc 0x{pc:x} (build the guest with LSE atomics)"));
                    }
                    if self.rd(HV_REG_CPSR) & 0x80 == 0 {
                        self.stop_stepping();
                    }
                }
                _ => {
                    let pc = self.rd(HV_REG_PC);
                    fatal(
                        3,
                        &format!(
                            "hvm: unhandled EC 0x{ec:x} ESR 0x{:x} pc 0x{pc:x} far 0x{:x} after {} exits",
                            e.syndrome, e.va, self.m.exits
                        ),
                    );
                }
            }
        }
    }

    // ---------------- vCPU state snapshot ----------------
    pub fn vtake(&self) -> VSnap {
        let v = self.v();
        let mut s = VSnap::default();
        for i in 0..31 {
            s.x[i] = v.get(HV_REG_X0 + i as u32);
        }
        s.pc = v.get(HV_REG_PC);
        s.cpsr = v.get(HV_REG_CPSR);
        s.fpcr = v.get(HV_REG_FPCR);
        s.fpsr = v.get(HV_REG_FPSR);
        for i in 0..32 {
            s.q[i] = v.get_q(i as u32);
        }
        for (i, &r) in SR.iter().enumerate() {
            let (rc, val) = v.sys_try(r, 0);
            s.sr[i] = val;
            s.ok[i] = (rc == 0) as u8;
        }
        for (i, &r) in DR.iter().enumerate() {
            let (rc, val) = v.sys_try(r, 0);
            s.dbg[i] = val;
            s.dok[i] = (rc == 0) as u8;
        }
        s
    }
    pub fn vput(&self, s: &VSnap) {
        let v = self.v();
        for i in 0..31 {
            v.set(HV_REG_X0 + i as u32, s.x[i]);
        }
        v.set(HV_REG_PC, s.pc);
        v.set(HV_REG_CPSR, s.cpsr);
        v.set(HV_REG_FPCR, s.fpcr);
        v.set(HV_REG_FPSR, s.fpsr);
        for i in 0..32 {
            v.set_q(i as u32, s.q[i]);
        }
        for (i, &r) in SR.iter().enumerate() {
            if s.ok[i] != 0 {
                v.set_sys_try(r, s.sr[i]);
            }
        }
        for (i, &r) in DR.iter().enumerate() {
            if s.dok[i] != 0 {
                v.set_sys_try(r, s.dbg[i]);
            }
        }
    }

    // ---------------- fingerprints ----------------
    /// hvm.c's `fingerprint()`: (console, regs, mach, ram, all).
    pub fn parts(&self) -> [u64; 5] {
        let s = self.vtake();
        let console = fnv(&self.console[..self.m.console_len as usize], OFFSET);
        let regs = s.hash_regs(OFFSET);
        let mut mach = fnv(&self.m.without_stats().bytes(), OFFSET);
        if self.vio.attached() {
            mach = self.vio.hash(mach);
        }
        let ram = fnv(self.ram.bytes(), OFFSET);
        let mut h = fnv_u64(console, OFFSET);
        h = fnv_u64(regs, h);
        h = fnv_u64(mach, h);
        h = fnv_u64(ram, h);
        [console, regs, mach, ram, h]
    }

    /// serve.c's `xfp()`: console, registers, machine, devices and all RAM.
    pub fn xfp(&self) -> u64 {
        let s = self.vtake();
        let mut h = fnv(b"hvm-explore-v1", OFFSET);
        h = fnv_u64(self.m.console_len, h);
        h = fnv(&self.console[..self.m.console_len as usize], h);
        h = s.hash_regs(h);
        h = fnv(&self.m.without_stats().bytes(), h);
        h = fnv_u64(self.ram_size, h);
        if self.vio.attached() {
            h = self.vio.hash(h);
        }
        crate::fnv::mix64(self.ram.bytes(), h)
    }
}

impl Drop for Vmm {
    fn drop(&mut self) {
        self.watch.stop.store(true, Ordering::Relaxed);
        if let Some(t) = &mut self.trace {
            let _ = t.flush();
        }
        if self.vm_live {
            drop(self.vcpu.take());
            hv::vm_destroy();
            VM_HELD.store(false, Ordering::SeqCst);
        }
        if let Some(j) = self.wd.take() {
            let _ = j.join();
        }
    }
}
