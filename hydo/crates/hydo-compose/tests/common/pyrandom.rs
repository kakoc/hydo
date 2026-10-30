//! CPython's `random.Random` (MT19937 with `init_by_array`, `random()`, `getrandbits`,
//! `randrange`, `randint`, `choice`, `sample`), so the generator of the old Python reference
//! (tests/pycheck.py at commit 2a58c4b) makes exactly the same inputs here. A copy of
//! hydo-history's tests/gen/pyrandom.rs (test helpers are not shared between crates).

#![allow(dead_code)]

pub struct PyRandom {
    mt: [u32; 624],
    i: usize,
}

impl PyRandom {
    /// `random.Random(seed)` for a non-negative int seed below 2^64.
    pub fn new(seed: u64) -> PyRandom {
        let mut key = Vec::new();
        let mut n = seed;
        while n > 0 {
            key.push(n as u32);
            n >>= 32;
        }
        if key.is_empty() {
            key.push(0);
        }
        let mut r = PyRandom {
            mt: [0; 624],
            i: 624,
        };
        r.init_genrand(19_650_218);
        let (mut i, mut j) = (1usize, 0usize);
        let mut k = 624.max(key.len());
        while k > 0 {
            let prev = r.mt[i - 1] ^ (r.mt[i - 1] >> 30);
            r.mt[i] = (r.mt[i] ^ prev.wrapping_mul(1_664_525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= 624 {
                r.mt[0] = r.mt[623];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
            k -= 1;
        }
        k = 623;
        while k > 0 {
            let prev = r.mt[i - 1] ^ (r.mt[i - 1] >> 30);
            r.mt[i] = (r.mt[i] ^ prev.wrapping_mul(1_566_083_941)).wrapping_sub(i as u32);
            i += 1;
            if i >= 624 {
                r.mt[0] = r.mt[623];
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
            self.mt[i] = 1_812_433_253u32
                .wrapping_mul(self.mt[i - 1] ^ (self.mt[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        self.i = 624;
    }

    fn next_u32(&mut self) -> u32 {
        if self.i >= 624 {
            for k in 0..624 {
                let y = (self.mt[k] & 0x8000_0000) | (self.mt[(k + 1) % 624] & 0x7fff_ffff);
                let mut v = self.mt[(k + 397) % 624] ^ (y >> 1);
                if y & 1 == 1 {
                    v ^= 0x9908_b0df;
                }
                self.mt[k] = v;
            }
            self.i = 0;
        }
        let mut y = self.mt[self.i];
        self.i += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    /// `random()`.
    pub fn random(&mut self) -> f64 {
        let a = (self.next_u32() >> 5) as f64;
        let b = (self.next_u32() >> 6) as f64;
        (a * 67_108_864.0 + b) * (1.0 / 9_007_199_254_740_992.0)
    }

    fn getrandbits(&mut self, k: u32) -> u64 {
        if k == 0 {
            return 0;
        }
        assert!(k <= 32, "getrandbits({k}) is not needed here");
        (self.next_u32() >> (32 - k)) as u64
    }

    fn randbelow(&mut self, n: usize) -> usize {
        let k = usize::BITS - n.leading_zeros();
        let mut r = self.getrandbits(k);
        while r >= n as u64 {
            r = self.getrandbits(k);
        }
        r as usize
    }

    /// `randrange(n)`.
    pub fn below(&mut self, n: usize) -> usize {
        assert!(n > 0, "empty range for randrange()");
        self.randbelow(n)
    }

    /// `randrange(a, b)`.
    pub fn range(&mut self, a: i64, b: i64) -> i64 {
        assert!(b > a, "empty range for randrange()");
        a + self.randbelow((b - a) as usize) as i64
    }

    /// `randint(a, b)`.
    pub fn randint(&mut self, a: i64, b: i64) -> i64 {
        self.range(a, b + 1)
    }

    /// `choice(seq)`.
    pub fn choice<'a, T>(&mut self, seq: &'a [T]) -> &'a T {
        assert!(!seq.is_empty(), "Cannot choose from an empty sequence");
        &seq[self.randbelow(seq.len())]
    }

    /// `sample(population, k)` for a small population (the pool algorithm).
    pub fn sample<T: Clone>(&mut self, pop: &[T], k: usize) -> Vec<T> {
        let n = pop.len();
        assert!(k <= n && n <= 21, "only the pool branch is ported");
        let mut pool = pop.to_vec();
        let mut out = Vec::with_capacity(k);
        for i in 0..k {
            let j = self.randbelow(n - i);
            out.push(pool[j].clone());
            pool[j] = pool[n - i - 1].clone();
        }
        out
    }
}
