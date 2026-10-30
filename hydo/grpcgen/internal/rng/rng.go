// Package rng is the generator's random source: splitmix64, the same mixer hvx uses for its
// fault schedules (hvx.c fault_mix). It is a pure function of the seed, needs no global state
// and gives the same stream on every platform and Go version (math/rand's seeded streams are
// stable too, but this one is also trivial to reproduce outside Go).
package rng

import "math"

// Golden is splitmix64's increment.
const Golden = 0x9e3779b97f4a7c15

// Mix is the splitmix64 finalizer.
func Mix(z uint64) uint64 {
	z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9
	z = (z ^ (z >> 27)) * 0x94d049bb133111eb
	return z ^ (z >> 31)
}

// Derive combines a seed with stream labels (step number, worker, ...) into a new seed, so
// independent streams never overlap: Derive(s, a, b) != Derive(s, b, a) in general.
func Derive(seed uint64, labels ...uint64) uint64 {
	h := Mix(seed + Golden)
	for _, l := range labels {
		h = Mix(Mix(h+Golden) ^ Mix(l+Golden))
	}
	return h
}

// R is one random stream.
type R struct{ s uint64 }

// New starts a stream at seed.
func New(seed uint64) *R { return &R{s: seed} }

// Uint64 returns the next 64 random bits.
func (r *R) Uint64() uint64 {
	r.s += Golden
	return Mix(r.s)
}

// Intn returns a value in [0, n); n <= 0 gives 0. (Modulo bias is irrelevant here.)
func (r *R) Intn(n int) int {
	if n <= 0 {
		return 0
	}
	return int(r.Uint64() % uint64(n))
}

// Range returns a value in [lo, hi] (inclusive); lo > hi gives lo.
func (r *R) Range(lo, hi int64) int64 {
	if hi <= lo {
		return lo
	}
	span := uint64(hi - lo)
	if span == math.MaxUint64 {
		return int64(r.Uint64())
	}
	return lo + int64(r.Uint64()%(span+1))
}

// URange is Range for unsigned values.
func (r *R) URange(lo, hi uint64) uint64 {
	if hi <= lo {
		return lo
	}
	span := hi - lo
	if span == math.MaxUint64 {
		return r.Uint64()
	}
	return lo + r.Uint64()%(span+1)
}

// Float returns a value in [0, 1).
func (r *R) Float() float64 { return float64(r.Uint64()>>11) / (1 << 53) }

// Chance is true with probability p.
func (r *R) Chance(p float64) bool { return r.Float() < p }
