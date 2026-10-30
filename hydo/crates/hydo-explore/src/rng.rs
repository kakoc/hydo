//! The explorer's random streams: splitmix64, exactly as explore.py's `SplitMix` (a schedule is a
//! pure function of its seed and the options), with Python's `%` for `below`/`rng`/`pick`.

use crate::py::pmod;

pub const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// explore.py `mix64(x)` (and one splitmix64 step from state `x`).
pub fn mix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(GOLDEN);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[derive(Clone, Debug)]
pub struct SplitMix {
    s: u64,
}

#[allow(clippy::should_implement_trait)]
impl SplitMix {
    pub fn new(seed: u64) -> SplitMix {
        SplitMix { s: seed }
    }

    pub fn next(&mut self) -> u64 {
        let z = mix64(self.s);
        self.s = self.s.wrapping_add(GOLDEN);
        z
    }

    /// `next() % n` with Python's sign rule (n may be negative when a range is empty).
    pub fn below(&mut self, n: i64) -> i64 {
        pmod(self.next() as i128, n as i128) as i64
    }

    /// Inclusive `lo..=hi` (Python: `lo + below(hi - lo + 1)`).
    pub fn rng(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below(hi - lo + 1)
    }

    pub fn pick<T: Clone>(&mut self, xs: &[T]) -> T {
        let i = self.below(xs.len() as i64);
        xs[i as usize].clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_reference() {
        // python3 -c "import explore as e; r=e.SplitMix(1); print(r.next(), r.next())"
        let mut r = SplitMix::new(1);
        assert_eq!(r.next(), 0x910a_2dec_8902_5cc1);
        assert_eq!(r.next(), 0xbeeb_8da1_658e_ec67);
    }
}
