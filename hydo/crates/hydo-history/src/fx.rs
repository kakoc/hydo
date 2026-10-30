//! Fast deterministic hashing for the checkers' internal maps, and the WGL search's memo table.
//! Hash values only decide speed, never an answer: every lookup compares the full key.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// FxHash (rustc's).
#[derive(Default, Clone, Copy)]
pub struct Fx(u64);

const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut it = bytes.chunks_exact(8);
        for c in &mut it {
            let mut w = [0u8; 8];
            w.copy_from_slice(c);
            self.write_u64(u64::from_le_bytes(w));
        }
        for &b in it.remainder() {
            self.write_u64(b as u64);
        }
    }
    fn write_u8(&mut self, x: u8) {
        self.write_u64(x as u64);
    }
    fn write_u32(&mut self, x: u32) {
        self.write_u64(x as u64);
    }
    fn write_u64(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(K);
    }
    fn write_usize(&mut self, x: usize) {
        self.write_u64(x as u64);
    }
}

pub type FxBuild = BuildHasherDefault<Fx>;
pub type FxHashMap<K, V> = HashMap<K, V, FxBuild>;
pub type FxHashSet<T> = HashSet<T, FxBuild>;

/// SplitMix64: the per-operation keys of the incremental (Zobrist) hash of a set of operations.
pub fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// The set of (linearized operations, state) pairs the search has visited (Lowe's memo). Each
/// entry's bitset lives in one arena; entries with the same hash are chained.
pub struct Memo {
    words: usize,
    heads: FxHashMap<u64, u32>,
    next: Vec<u32>,
    states: Vec<u64>,
    bits: Vec<u64>,
}

const NONE: u32 = u32::MAX;

impl Memo {
    pub fn new(words: usize) -> Memo {
        Memo {
            words,
            heads: FxHashMap::default(),
            next: Vec::new(),
            states: Vec::new(),
            bits: Vec::new(),
        }
    }

    /// Add (lin, state) whose set hash is `h`; false if it was already there.
    pub fn insert(&mut self, h: u64, lin: &[u64], state: u64) -> bool {
        let key = h ^ splitmix(state);
        let head = self.heads.get(&key).copied().unwrap_or(NONE);
        let mut i = head;
        while i != NONE {
            let j = i as usize;
            if self.states[j] == state && self.bits[j * self.words..(j + 1) * self.words] == *lin {
                return false;
            }
            i = self.next[j];
        }
        let j = self.states.len() as u32;
        self.states.push(state);
        self.next.push(head);
        self.bits.extend_from_slice(lin);
        self.heads.insert(key, j);
        true
    }
}
