//! The deterministic machine state of hvm.c (`struct mach`, the GIC and the UART), with its exact
//! C byte layout for the fingerprints, and the host-side timing policies: tick jitter (`TJ`) and
//! the forced preemption points (`PCT`).

use crate::layout::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gic {
    pub dctlr: u32,
    pub cctlr: u32,
    pub pmr: u32,
    pub bpr: u32,
    pub enabled: [u32; NIRQ / 32],
    pub pending: [u32; NIRQ / 32],
    pub active: [u32; NIRQ / 32],
    pub cfg: [u32; NIRQ / 16],
    pub group: [u32; NIRQ / 32],
    pub prio: [u8; NIRQ],
    pub targets: [u8; NIRQ],
}

impl Default for Gic {
    fn default() -> Gic {
        Gic {
            dctlr: 0,
            cctlr: 0,
            pmr: 0,
            bpr: 0,
            enabled: [0; NIRQ / 32],
            pending: [0; NIRQ / 32],
            active: [0; NIRQ / 32],
            cfg: [0; NIRQ / 16],
            group: [0; NIRQ / 32],
            prio: [0; NIRQ],
            targets: [0; NIRQ],
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Uart {
    pub ier: u8,
    pub lcr: u8,
    pub mcr: u8,
    pub scr: u8,
    pub dll: u8,
    pub dlm: u8,
    pub fcr: u8,
}

/// `struct mach`: everything here is in the snapshot.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mach {
    /// virtual counter, CNTFRQ ticks
    pub vtime: u64,
    /// all exits taken
    pub exits: u64,
    pub cntp_ctl: u64,
    pub cntp_cval: u64,
    pub gic: Gic,
    pub uart: Uart,
    pub console_len: u64,
    /// PSCI SYSTEM_OFF/RESET
    pub halted: u32,
    /// single-stepping to the instruction where the guest unmasks IRQs
    pub stepping: u32,
    pub mdscr_saved: u64,
    pub n_wfi: u64,
    pub n_wfi_jump: u64,
    pub n_cntpct: u64,
    pub n_mmio: u64,
    pub n_hvc: u64,
    pub n_sysreg_other: u64,
    pub n_irq_line: u64,
    pub n_steps: u64,
    pub n_step_runs: u64,
}

/// sizeof(struct mach) on arm64 macOS.
pub const MACH_SIZE: usize = 328;

impl Mach {
    /// The struct's bytes as the C compiler lays it out (the one padding byte, after the UART, is
    /// zero: the C state is memset and only ever copied whole).
    pub fn bytes(&self) -> [u8; MACH_SIZE] {
        let mut b = [0u8; MACH_SIZE];
        let mut o = 0;
        let put = |b: &mut [u8; MACH_SIZE], o: &mut usize, v: &[u8]| {
            b[*o..*o + v.len()].copy_from_slice(v);
            *o += v.len();
        };
        for v in [self.vtime, self.exits, self.cntp_ctl, self.cntp_cval] {
            put(&mut b, &mut o, &v.to_le_bytes());
        }
        let g = &self.gic;
        for v in [g.dctlr, g.cctlr, g.pmr, g.bpr] {
            put(&mut b, &mut o, &v.to_le_bytes());
        }
        for a in [
            &g.enabled[..],
            &g.pending[..],
            &g.active[..],
            &g.cfg[..],
            &g.group[..],
        ] {
            for v in a {
                put(&mut b, &mut o, &v.to_le_bytes());
            }
        }
        put(&mut b, &mut o, &g.prio);
        put(&mut b, &mut o, &g.targets);
        let u = &self.uart;
        put(
            &mut b,
            &mut o,
            &[u.ier, u.lcr, u.mcr, u.scr, u.dll, u.dlm, u.fcr],
        );
        o += 1; // padding
        put(&mut b, &mut o, &self.console_len.to_le_bytes());
        put(&mut b, &mut o, &self.halted.to_le_bytes());
        put(&mut b, &mut o, &self.stepping.to_le_bytes());
        for v in [
            self.mdscr_saved,
            self.n_wfi,
            self.n_wfi_jump,
            self.n_cntpct,
            self.n_mmio,
            self.n_hvc,
            self.n_sysreg_other,
            self.n_irq_line,
            self.n_steps,
            self.n_step_runs,
        ] {
            put(&mut b, &mut o, &v.to_le_bytes());
        }
        debug_assert_eq!(o, MACH_SIZE);
        b
    }

    /// The machine state minus the pure statistics counters (what the fingerprints hash).
    pub fn without_stats(&self) -> Mach {
        Mach {
            n_wfi: 0,
            n_wfi_jump: 0,
            n_cntpct: 0,
            n_mmio: 0,
            n_hvc: 0,
            n_sysreg_other: 0,
            n_irq_line: 0,
            n_steps: 0,
            n_step_runs: 0,
            ..*self
        }
    }
}

pub fn bit(a: &[u32], i: usize) -> bool {
    (a[i / 32] >> (i % 32)) & 1 != 0
}
pub fn setb(a: &mut [u32], i: usize, v: bool) {
    if v {
        a[i / 32] |= 1 << (i % 32);
    } else {
        a[i / 32] &= !(1u32 << (i % 32));
    }
}

/// The splitmix64 finalizer, after adding the golden ratio (hvx's tick-jitter and fault mix).
pub fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}
pub const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;

/// Seeded tick jitter (`struct tjit`), counted in exits; host-side, in no snapshot.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Tjit {
    pub on: bool,
    pub seed: u64,
    pub permille: u32,
    pub max_delay: u32,
    pub lo: u64,
    pub hi: u64,
    pub n_explicit: usize,
    pub ek: [u64; TJ_MAX_EXPLICIT],
    pub ed: [u32; TJ_MAX_EXPLICIT],
    /// statistics: delays that were actually taken
    pub applied: u64,
    pub last_applied_k: u64,
}

impl Tjit {
    /// `tj_delay(k)`: in exits, capped inside the tick period.
    pub fn delay(&self, k: u64, step: u64) -> u64 {
        if !self.on || k <= 1 {
            return 0;
        }
        let mut d: u64 = 0;
        if self.n_explicit > 0 {
            for i in 0..self.n_explicit {
                if self.ek[i] == k {
                    d = self.ed[i] as u64;
                }
            }
        } else {
            if k < self.lo || (self.hi != 0 && k >= self.hi) {
                return 0;
            }
            let h = mix(self.seed.wrapping_add(k.wrapping_mul(GOLDEN)));
            if h % 1000 >= self.permille as u64 {
                return 0;
            }
            d = 1 + (h >> 16) % self.max_delay as u64;
        }
        let cap = (TJ_PERIOD / step).wrapping_sub(1);
        d.min(cap)
    }
}

/// PCT-style forced preemption points (`struct pctp`), counted in exits; host-side.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pct {
    pub on: bool,
    pub n: u32,
    pub next: u32,
    pub pt: [u64; PCT_MAX],
    pub fired: u64,
    pub missed: u64,
}

impl Default for Pct {
    fn default() -> Pct {
        Pct {
            on: false,
            n: 0,
            next: 0,
            pt: [0; PCT_MAX],
            fired: 0,
            missed: 0,
        }
    }
}

impl Pct {
    /// `pct_set(seed, n, lo, hi)`.
    pub fn set(seed: u64, n: u32, lo: u64, hi: u64) -> Pct {
        let mut p = Pct::default();
        if n == 0 || hi <= lo {
            return p;
        }
        let n = n.min(PCT_MAX as u32);
        for i in 0..n {
            let x = mix(seed.wrapping_add((i as u64 + 1).wrapping_mul(GOLDEN)));
            p.pt[i as usize] = lo + x % (hi - lo);
        }
        for i in 1..n as usize {
            let mut j = i;
            while j > 0 && p.pt[j - 1] > p.pt[j] {
                p.pt.swap(j, j - 1);
                j -= 1;
            }
        }
        p.n = n;
        p.on = true;
        p
    }
}
