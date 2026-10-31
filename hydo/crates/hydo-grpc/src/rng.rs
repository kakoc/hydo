//! The generator's random source: splitmix64, the same mixer hvx uses for its fault schedules
//! (hvx.c fault_mix). A pure function of the seed, the same stream on every platform.

/// splitmix64's increment.
pub const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;

/// The splitmix64 finalizer.
pub fn mix(z: u64) -> u64 {
    let z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Combines a seed with stream labels (step number, worker, ...) into a new seed, so independent
/// streams never overlap: derive(s, [a, b]) != derive(s, [b, a]) in general.
pub fn derive(seed: u64, labels: &[u64]) -> u64 {
    let mut h = mix(seed.wrapping_add(GOLDEN));
    for &l in labels {
        h = mix(mix(h.wrapping_add(GOLDEN)) ^ mix(l.wrapping_add(GOLDEN)));
    }
    h
}

/// One random stream.
#[derive(Clone, Debug)]
pub struct R {
    s: u64,
}

impl R {
    /// A stream starting at `seed`.
    pub fn new(seed: u64) -> R {
        R { s: seed }
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.s = self.s.wrapping_add(GOLDEN);
        mix(self.s)
    }

    /// A value in [0, n); n <= 0 gives 0.
    pub fn intn(&mut self, n: i64) -> i64 {
        if n <= 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as i64
    }

    /// A value in [lo, hi] (inclusive); lo > hi gives lo.
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        let span = hi.wrapping_sub(lo) as u64;
        if span == u64::MAX {
            return self.next_u64() as i64;
        }
        lo.wrapping_add((self.next_u64() % (span + 1)) as i64)
    }

    /// `range` for unsigned values.
    pub fn urange(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            return lo;
        }
        let span = hi - lo;
        if span == u64::MAX {
            return self.next_u64();
        }
        lo + self.next_u64() % (span + 1)
    }

    /// A value in [0, 1).
    pub fn float(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// True with probability p.
    pub fn chance(&mut self, p: f64) -> bool {
        self.float() < p
    }
}
