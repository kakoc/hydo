//! Deterministic sources that replace the two biggest wells of nondeterminism a
//! guest can draw from: randomness and time.
//!
//! Both are pure functions of state the VMM fully controls, so replaying from the
//! same seed reproduces the identical value stream. A guest that would normally
//! call RDRAND or RDTSC instead asks the VMM through the paravirtual device, and
//! gets an answer that is a deterministic function of how far execution has come.

/// SplitMix64: a tiny, fast, well-distributed PRNG. Deterministic given the seed.
#[derive(Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Snapshot of internal state, so the PRNG can be saved and restored exactly.
    pub fn state(&self) -> u64 {
        self.state
    }
    pub fn set_state(&mut self, s: u64) {
        self.state = s;
    }
}

/// A virtual clock. It does not track wall-clock time at all; it counts VMM
/// control events (VM exits). Because the exit sequence is itself deterministic
/// for a single vCPU with mediated inputs, the clock is deterministic too — the
/// guest's notion of "now" advances with work performed, not with real seconds.
#[derive(Clone, Default)]
pub struct Clock {
    ticks: u64,
}

impl Clock {
    pub fn new() -> Self {
        Clock { ticks: 0 }
    }
    pub fn advance(&mut self, n: u64) {
        self.ticks = self.ticks.wrapping_add(n);
    }
    pub fn now(&self) -> u64 {
        self.ticks
    }
    pub fn set(&mut self, t: u64) {
        self.ticks = t;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prng_is_reproducible_from_seed() {
        let mut a = SplitMix64::new(0xABCD_1234);
        let mut b = SplitMix64::new(0xABCD_1234);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn prng_state_round_trips() {
        let mut a = SplitMix64::new(1);
        for _ in 0..10 {
            a.next_u64();
        }
        // Capture, advance the original, then restore into a fresh PRNG.
        let saved = a.state();
        let expected = a.next_u64();
        let mut b = SplitMix64::new(0);
        b.set_state(saved);
        assert_eq!(b.next_u64(), expected);
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = SplitMix64::new(1);
        let mut b = SplitMix64::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }
}
