//! **The guest RAM's tree hash** (Milestone 294: the fingerprint of an image built at the
//! fingerprint epoch 4 or later, [`tree_hash`]). The RAM's part of a fingerprint is a hash of
//! hashes, in the shape of the disks' tree of Milestone 290 (`crate::disk`). Each 4 KiB page has a
//! hash ([`leaf`]), each 2 MiB chunk a hash over its pages' hashes in order, and the root covers
//! the RAM's length and the chunks' hashes in order.
//!
//! The leaf is the word-wise mix of `crate::fnv::mix64` in eight lanes: word `i` of the page goes
//! into lane `i % 8`, and the lanes' values are mixed in order at the end. The eight lanes are
//! independent chains that the core runs at once, so one core hashes about five times as many
//! bytes per second as the single chain of `mix64`. The pages are hashed on several threads, a
//! piece of 64 MiB at a time, so the RAM part of a fingerprint of web-shop's 4 GiB costs tens of
//! milliseconds instead of a second.
//!
//! The root is a function of the RAM's bytes alone: the shape is fixed by the RAM's length, not by
//! the number of threads. A caller may give a page's hash without its bytes when it knows them, as
//! the KVM engine does for its base's pages ([`tree_hash`]'s `page`); the value is the same. Each
//! step is a bijection of the state when the other inputs are fixed (`h ^= w`, a multiplication by
//! the odd FNV prime, `h ^= h >> 29`, and FNV-1a's bytes), so a change of any one byte of the RAM
//! always changes the root. An image of an older epoch keeps `mix64` over every byte.

use crate::fnv::{fnv, fnv_u64, OFFSET, PRIME};
use std::sync::{Mutex, OnceLock};

/// The size of a leaf of the tree.
pub const PAGE: usize = 4096;
/// Pages per node (2 MiB of RAM).
const CHUNK: usize = 512;
/// Lanes of a leaf.
const LANES: usize = 8;

#[inline(always)]
fn step(mut h: u64, w: u64) -> u64 {
    h ^= w;
    h = h.wrapping_mul(PRIME);
    h ^ (h >> 29)
}

/// A page's hash: eight lanes of the word-wise mix (word `i` into lane `i % 8`), then the lanes
/// mixed in order, then FNV-1a of a tail shorter than a word (the last page of a RAM whose length
/// is not a multiple of 8; guest RAM never has one).
pub fn leaf(p: &[u8]) -> u64 {
    let mut l = [0u64; LANES];
    for (j, v) in l.iter_mut().enumerate() {
        *v = OFFSET ^ j as u64;
    }
    let groups = p.chunks_exact(8 * LANES);
    let rest = groups.remainder();
    for g in groups {
        for (j, v) in l.iter_mut().enumerate() {
            let w = u64::from_le_bytes(g[8 * j..8 * j + 8].try_into().unwrap());
            *v = step(*v, w);
        }
    }
    let words = rest.chunks_exact(8);
    let tail = words.remainder();
    for (j, w) in words.enumerate() {
        l[j] = step(l[j], u64::from_le_bytes(w.try_into().unwrap()));
    }
    let h = l.iter().fold(OFFSET, |h, &v| step(h, v));
    fnv(tail, h)
}

/// The hash of a page of zeros (the pages a guest never wrote).
pub fn zero_leaf() -> u64 {
    static Z: OnceLock<u64> = OnceLock::new();
    *Z.get_or_init(|| leaf(&[0u8; PAGE]))
}

/// A chunk's hash over its pages' hashes, in order.
fn node(pages: impl Iterator<Item = u64>) -> u64 {
    pages.fold(OFFSET, step)
}

/// The threads a fingerprint hashes on: half of the host's cores (the explorer runs several
/// engines at once), at least one.
pub fn threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .div_ceil(2)
}

/// Each page's hash, for `len` bytes of RAM: `page(k)` gives page k's hash (usually
/// `leaf(&ram[k * PAGE..])`), on `threads` threads, 64 MiB at a time.
pub fn leaves(len: usize, threads: usize, page: impl Fn(usize) -> u64 + Sync) -> Vec<u64> {
    let pages = len.div_ceil(PAGE);
    let mut out = vec![0u64; pages];
    let mut chunks = vec![0u64; pages.div_ceil(CHUNK)];
    par_pages(&mut out, &mut chunks, threads, &page);
    out
}

/// Page k's hash `f(k)` into `out[k]` and each chunk's node into `chunks`, on `threads` threads.
fn par_pages(
    out: &mut [u64],
    chunks: &mut [u64],
    threads: usize,
    f: &(dyn Fn(usize) -> u64 + Sync),
) {
    /// chunks per piece of work
    const RUN: usize = 32;
    let n = out.len();
    let nchunks = n.div_ceil(CHUNK);
    let threads = threads.min(nchunks.div_ceil(RUN)).max(1);
    let work = Mutex::new(
        out.chunks_mut(CHUNK * RUN)
            .zip(chunks.chunks_mut(RUN))
            .enumerate(),
    );
    let one = |i: usize, ps: &mut [u64], cs: &mut [u64]| {
        let first = i * CHUNK * RUN;
        for (j, h) in ps.iter_mut().enumerate() {
            *h = f(first + j);
        }
        for (c, h) in ps.chunks(CHUNK).zip(cs.iter_mut()) {
            *h = node(c.iter().copied());
        }
    };
    if threads == 1 {
        for (i, (ps, cs)) in work.into_inner().unwrap_or_else(|e| e.into_inner()) {
            one(i, ps, cs);
        }
        return;
    }
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let next = work.lock().unwrap_or_else(|e| e.into_inner()).next();
                let Some((i, (ps, cs))) = next else { break };
                one(i, ps, cs);
            });
        }
    });
}

/// The tree hash of `len` bytes of RAM, continued from `h` (the module documentation): `page(k)`
/// gives page k's hash, which is `leaf` of its bytes (a caller that knows a page's hash may give
/// it without reading the page), on `threads` threads.
pub fn tree_hash(h: u64, len: usize, threads: usize, page: impl Fn(usize) -> u64 + Sync) -> u64 {
    let pages = len.div_ceil(PAGE);
    let mut out = vec![0u64; pages];
    let mut chunks = vec![0u64; pages.div_ceil(CHUNK)];
    par_pages(&mut out, &mut chunks, threads, &page);
    let mut h = fnv(b"hydo-ram-tree-v1", h);
    h = fnv_u64(len as u64, h);
    for c in chunks {
        h = fnv_u64(c, h);
    }
    h
}

/// The tree hash of the bytes `ram`, continued from `h`, on `threads` threads.
pub fn hash_bytes(h: u64, ram: &[u8], threads: usize) -> u64 {
    tree_hash(h, ram.len(), threads, |k| {
        leaf(&ram[k * PAGE..((k + 1) * PAGE).min(ram.len())])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ram(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        let mut b = vec![0u8; len];
        // a nonzero first half, zeros after
        for w in b[..len / 2].chunks_exact_mut(8) {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            w.copy_from_slice(&x.to_le_bytes());
        }
        b
    }

    /// The tree written out plainly: every leaf, every node, the root.
    fn reference(h: u64, b: &[u8]) -> u64 {
        let leaves: Vec<u64> = b.chunks(PAGE).map(leaf).collect();
        let mut h = fnv(b"hydo-ram-tree-v1", h);
        h = fnv_u64(b.len() as u64, h);
        for c in leaves.chunks(CHUNK) {
            let mut n = OFFSET;
            for &w in c {
                n = step(n, w);
            }
            h = fnv_u64(n, h);
        }
        h
    }

    #[test]
    fn the_root_does_not_depend_on_the_threads() {
        let b = ram(70 << 20, 7);
        let one = hash_bytes(OFFSET, &b, 1);
        assert_eq!(one, reference(OFFSET, &b));
        for t in [2, 3, 12] {
            assert_eq!(hash_bytes(OFFSET, &b, t), one);
        }
        // a pinned value: the scheme is the epoch-4 fingerprint's
        assert_eq!(
            hash_bytes(OFFSET, &ram(4 << 20, 1), 4),
            0x89b2_00f4_c6a6_6706
        );
    }

    #[test]
    fn known_pages_give_the_same_root() {
        let b = ram(9 << 20, 3);
        let known = leaves(b.len(), 4, |k| leaf(&b[k * PAGE..(k + 1) * PAGE]));
        let z = zero_leaf();
        let r = tree_hash(OFFSET, b.len(), 4, |k| {
            let p = &b[k * PAGE..(k + 1) * PAGE];
            if p.iter().all(|&x| x == 0) {
                z
            } else {
                known[k]
            }
        });
        assert_eq!(r, hash_bytes(OFFSET, &b, 2));
    }

    #[test]
    fn a_changed_byte_changes_the_root() {
        let mut b = ram(6 << 20, 5);
        let r0 = hash_bytes(OFFSET, &b, 3);
        for at in [0, 7, 8, 63, 64, 4095, 4096, 3 << 20, (6 << 20) - 1] {
            for v in [1u8, 0x80, 0xff] {
                b[at] ^= v;
                assert_ne!(hash_bytes(OFFSET, &b, 3), r0, "byte {at} ^ {v:#x}");
                b[at] ^= v;
            }
        }
        assert_eq!(hash_bytes(OFFSET, &b, 3), r0);
        // two pages swapped: the order counts
        let (x, y) = b.split_at_mut(PAGE);
        x.swap_with_slice(&mut y[..PAGE]);
        assert_ne!(hash_bytes(OFFSET, &b, 3), r0);
        // the length counts
        assert_ne!(
            hash_bytes(OFFSET, &vec![0u8; 8 << 20], 3),
            hash_bytes(OFFSET, &vec![0u8; 4 << 20], 3)
        );
    }

    #[test]
    fn a_short_page_is_hashed_to_its_end() {
        let b = ram(PAGE + 100, 9);
        let mut c = b.clone();
        c[PAGE + 99] ^= 1;
        assert_ne!(hash_bytes(OFFSET, &b, 1), hash_bytes(OFFSET, &c, 1));
        assert_eq!(
            leaf(&[]),
            [OFFSET; 8]
                .iter()
                .enumerate()
                .fold(OFFSET, |h, (j, &o)| step(h, o ^ j as u64))
        );
    }
}
