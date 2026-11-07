//! The deterministic machine state the engine keeps (everything here is in a snapshot and, but
//! the statistics, in the fingerprints) and the host-side timing policies, tick jitter
//! ([`Tjit`]) and forced preemption points (hydo-hvm's `Pct`), both counted in exits as on the
//! Mac.
//!
//! The clock: `vtime` (nanoseconds) advances by the step at every VM exit that reaches the
//! engine, and only then (`exits` counts them). The hydo timer (HYDO_PORT_TIMER) raises IRQ 0
//! while it is armed and `vtime` has reached its deadline (plus a tick-jitter delay); the engine
//! disarms it when it delivers the interrupt.

use crate::layout::*;
use crate::msr::Msrs;
use crate::pic::Pic;
use crate::uart;
use hydo_hvm::machine::{mix, GOLDEN};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mach {
    /// the guest's clock, ns
    pub vtime: u64,
    /// VM exits that reached the engine
    pub exits: u64,
    pub timer_armed: u32,
    pub timer_cval: u64,
    pub pic: Pic,
    pub uart: uart::Regs,
    pub console_len: u64,
    /// 1: powered off (HYDO_PORT_CTL 1), 2: reset (2), 3: shutdown (a triple fault)
    pub halted: u32,
    pub msrs: Msrs,
    // statistics, not hashed
    pub n_io: u64,
    pub n_mmio: u64,
    pub n_msr: u64,
    pub n_hlt: u64,
    pub n_hlt_jump: u64,
    pub n_window: u64,
    pub n_irq: u64,
    pub n_clock: u64,
    pub n_unknown: u64,
    /// exit-free cycles the spin observer resolved
    pub n_spin: u64,
}

impl Mach {
    /// The state minus the statistics (what the fingerprints hash).
    pub fn without_stats(&self) -> Mach {
        Mach {
            n_io: 0,
            n_mmio: 0,
            n_msr: 0,
            n_hlt: 0,
            n_hlt_jump: 0,
            n_window: 0,
            n_irq: 0,
            n_clock: 0,
            n_unknown: 0,
            n_spin: 0,
            ..*self
        }
    }

    /// The canonical bytes, field by field in this order, little-endian, no padding.
    pub fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(160);
        for v in [self.vtime, self.exits] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&self.timer_armed.to_le_bytes());
        b.extend_from_slice(&self.timer_cval.to_le_bytes());
        b.extend_from_slice(&self.pic.bytes());
        b.extend_from_slice(&self.uart.bytes());
        b.extend_from_slice(&self.console_len.to_le_bytes());
        b.extend_from_slice(&self.halted.to_le_bytes());
        b.extend_from_slice(&self.msrs.bytes());
        for v in [
            self.n_io,
            self.n_mmio,
            self.n_msr,
            self.n_hlt,
            self.n_hlt_jump,
            self.n_window,
            self.n_irq,
            self.n_clock,
            self.n_unknown,
            self.n_spin,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }
}

/// Seeded tick jitter, counted in exits (hydo-hvm's `Tjit` with this machine's tick period).
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
    /// The delay of tick `k`, in exits, capped inside the tick period.
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
        let cap = (TJ_PERIOD / step.max(1)).wrapping_sub(1);
        d.min(cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_are_not_hashed() {
        let a = Mach::default();
        let b = Mach {
            n_io: 5,
            n_irq: 2,
            ..Mach::default()
        };
        assert_ne!(a.bytes(), b.bytes());
        assert_eq!(a.without_stats().bytes(), b.without_stats().bytes());
        let c = Mach {
            vtime: 1,
            ..Mach::default()
        };
        assert_ne!(a.without_stats().bytes(), c.without_stats().bytes());
    }

    #[test]
    fn tick_jitter_is_seeded_and_capped() {
        let t = Tjit {
            on: true,
            seed: 7,
            permille: 1000,
            max_delay: 1_000_000,
            ..Tjit::default()
        };
        let step = 2500;
        for k in 2..50 {
            let d = t.delay(k, step);
            assert!((1..TJ_PERIOD / step).contains(&d), "{k} {d}");
            assert_eq!(d, t.delay(k, step));
        }
        assert_eq!(t.delay(1, step), 0);
        let mut x = Tjit {
            on: true,
            n_explicit: 1,
            ..Tjit::default()
        };
        x.ek[0] = 9;
        x.ed[0] = 3;
        assert_eq!((x.delay(9, step), x.delay(8, step)), (3, 0));
    }
}
