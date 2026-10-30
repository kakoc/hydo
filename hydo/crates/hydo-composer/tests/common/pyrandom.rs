//! Python's `random.Random` (MT19937 seeded by `init_by_array`) for the methods the composer's
//! equivalence driver uses: `getrandbits`, `randrange`, `choice`, `shuffle`, `sample`. The same
//! integer seed gives the same stream as CPython 3.9+ (checked against Python 3.15 when the
//! goldens were recorded: the driver's streams are identical).

#![allow(dead_code)]

pub struct PyRandom {
    mt: [u32; 624],
    idx: usize,
}

impl PyRandom {
    pub fn new(seed: u64) -> PyRandom {
        let mut key = vec![seed as u32];
        if seed >> 32 != 0 {
            key.push((seed >> 32) as u32);
        }
        let mut r = PyRandom {
            mt: [0; 624],
            idx: 624,
        };
        r.init_genrand(19650218);
        let n = 624usize;
        let (mut i, mut j) = (1usize, 0usize);
        let mut k = n.max(key.len());
        while k > 0 {
            let p = r.mt[i - 1] ^ (r.mt[i - 1] >> 30);
            r.mt[i] = (r.mt[i] ^ p.wrapping_mul(1664525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= n {
                r.mt[0] = r.mt[n - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
            k -= 1;
        }
        k = n - 1;
        while k > 0 {
            let p = r.mt[i - 1] ^ (r.mt[i - 1] >> 30);
            r.mt[i] = (r.mt[i] ^ p.wrapping_mul(1566083941)).wrapping_sub(i as u32);
            i += 1;
            if i >= n {
                r.mt[0] = r.mt[n - 1];
                i = 1;
            }
            k -= 1;
        }
        r.mt[0] = 0x8000_0000;
        r
    }

    fn init_genrand(&mut self, s: u32) {
        self.mt[0] = s;
        for i in 1..624 {
            self.mt[i] = 1812433253u32
                .wrapping_mul(self.mt[i - 1] ^ (self.mt[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        self.idx = 624;
    }

    fn genrand(&mut self) -> u32 {
        const UPPER: u32 = 0x8000_0000;
        const LOWER: u32 = 0x7fff_ffff;
        const MATRIX: u32 = 0x9908_b0df;
        if self.idx >= 624 {
            for kk in 0..624 {
                let y = (self.mt[kk] & UPPER) | (self.mt[(kk + 1) % 624] & LOWER);
                let mag = if y & 1 == 1 { MATRIX } else { 0 };
                self.mt[kk] = self.mt[(kk + 397) % 624] ^ (y >> 1) ^ mag;
            }
            self.idx = 0;
        }
        let mut y = self.mt[self.idx];
        self.idx += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    /// `getrandbits(k)` for k <= 64.
    pub fn getrandbits(&mut self, k: u32) -> u64 {
        assert!(k <= 64);
        if k == 0 {
            return 0;
        }
        if k <= 32 {
            return u64::from(self.genrand() >> (32 - k));
        }
        let mut left = k;
        let mut out = 0u64;
        let mut shift = 0;
        while left > 0 {
            let mut r = self.genrand();
            if left < 32 {
                r >>= 32 - left;
            }
            out |= u64::from(r) << shift;
            shift += 32;
            left = left.saturating_sub(32);
        }
        out
    }

    /// `_randbelow(n)` (= `randrange(n)`).
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0);
        let k = 64 - n.leading_zeros();
        let mut r = self.getrandbits(k);
        while r >= n {
            r = self.getrandbits(k);
        }
        r
    }

    /// `randrange(a, b)`.
    pub fn range(&mut self, a: u64, b: u64) -> u64 {
        a + self.below(b - a)
    }

    pub fn choice<T: Clone>(&mut self, v: &[T]) -> T {
        v[self.below(v.len() as u64) as usize].clone()
    }

    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            v.swap(i, j);
        }
    }

    /// `sample(population, k)` (Python 3.11+'s algorithm; the driver's k never exceeds 5).
    pub fn sample<T: Clone>(&mut self, pop: &[T], k: usize) -> Vec<T> {
        let n = pop.len();
        assert!(k <= n);
        let mut setsize = 21usize;
        if k > 5 {
            setsize += 4usize.pow(((k * 3) as f64).log(4.0).ceil() as u32);
        }
        let mut out = Vec::with_capacity(k);
        if n <= setsize {
            let mut pool = pop.to_vec();
            for i in 0..k {
                let j = self.below((n - i) as u64) as usize;
                out.push(pool[j].clone());
                pool[j] = pool[n - i - 1].clone();
            }
        } else {
            let mut sel = std::collections::HashSet::new();
            for _ in 0..k {
                let mut j = self.below(n as u64) as usize;
                while sel.contains(&j) {
                    j = self.below(n as u64) as usize;
                }
                sel.insert(j);
                out.push(pop[j].clone());
            }
        }
        out
    }
}
