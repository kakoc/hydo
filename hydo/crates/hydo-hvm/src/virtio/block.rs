//! The block faults of Milestone 313 (docs/design/block-faults.md), on the data disk of the Mac
//! and KVM engines: a torn write at a power cut (`torn=`), a misdirected write (`mis=`), latent
//! sector errors (`lse=`, and `G=...:bad|rot` through the host's `DISK` command), and a request
//! that never completes (`hang=`).
//!
//! The state lives here, outside [`super::VState`]: that struct is hashed with the exact C
//! layout of every recorded fingerprint, and a new field there would move them all.
//! [`BlkFaults::hash`] adds this state to a fingerprint only when it is not the default, so a
//! run without the faults keeps its fingerprint and its control transcript. Every decision is a
//! pure function of the schedule's `dseed=` and the request numbers counted from the workload's
//! start, so a schedule repeats bit for bit.

use crate::fnv::{fnv, fnv_u64};
use crate::machine::{mix, GOLDEN};
use std::collections::BTreeMap;

pub const SECTOR: u64 = 512;

/// The `lse=` tokens of a schedule or a `DISK` command: each one's (first sector, count) ranges,
/// and whether a read of them returns damaged bytes (`:rot`) instead of an error.
pub type LseList = Vec<(Vec<(u64, u64)>, bool)>;
const TORN_SALT: u64 = 0x544f_524e_0000_0001;
const MIS_SALT: u64 = 0x4d49_5344_0000_0002;
const ROT_SALT: u64 = 0x524f_5421_0000_0003;
/// The farthest a seeded misdirected write goes, in blocks of 4 KiB (8 sectors).
pub const MIS_BLOCKS: u64 = 16;
/// The guest ticks per second when the engine sets none (the Mac engine's CNTFRQ).
const TICKS_DEFAULT: u64 = 24_000_000;

/// What a schedule arms for its workload (the host tokens of Milestone 313).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlkCfg {
    /// `dseed=`
    pub seed: u64,
    /// `torn=<p>[:any]`: (percent, any subset)
    pub torn: Option<(u32, bool)>,
    /// `mis=<k>[:<sectors>]`
    pub mis: Option<(u64, Option<i64>)>,
    /// `hang=<k>[+<ms>]`
    pub hang: Option<(u64, Option<u64>)>,
}

impl BlkCfg {
    pub fn any(&self) -> bool {
        self.torn.is_some() || self.mis.is_some() || self.hang.is_some()
    }

    /// The block faults of a schedule's host part: what the workload arms, and the `lse=`
    /// ranges (with `:rot`) to add from its start. The Mac and KVM engines share it.
    pub fn from_schedule(s: &hydo_schedule::Schedule) -> (BlkCfg, LseList) {
        use hydo_schedule::HostToken;
        let mut c = BlkCfg {
            seed: s.dseed(),
            ..BlkCfg::default()
        };
        let mut lse = Vec::new();
        for t in s.main_host() {
            match t {
                HostToken::Torn { pct, any } => c.torn = Some((*pct as u32, *any)),
                HostToken::Mis { k, delta } => c.mis = Some((*k, *delta)),
                HostToken::Hang { k, ms } => c.hang = Some((*k, *ms)),
                HostToken::Lse { ranges, rot } => lse.push((ranges.clone(), *rot)),
                _ => {}
            }
        }
        if !c.any() {
            // a seed alone arms nothing, so the state stays inert
            c.seed = if lse.is_empty() { 0 } else { c.seed };
        }
        (c, lse)
    }
}

/// The bad sectors of a `DISK <host tokens>` command (Milestone 313): its `lse=` tokens; Err
/// for anything else.
pub fn disk_cmd_ranges(tokens: &str) -> Result<LseList, String> {
    let s = hydo_schedule::Schedule::parse(&format!("{tokens} | run")).map_err(|e| e.message)?;
    let mut out = Vec::new();
    for t in &s.host {
        match t {
            hydo_schedule::HostToken::Lse { ranges, rot } => out.push((ranges.clone(), *rot)),
            t => return Err(format!("{}= is not a block fault of DISK (lse=)", t.key())),
        }
    }
    if out.is_empty() {
        return Err("needs an lse= token".into());
    }
    Ok(out)
}

/// A request the disk holds (`hang=`): no status, no used-ring entry, until `release` (never
/// when None).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    /// the request's number from the workload's start
    pub n: u64,
    pub head: u16,
    pub used_len: u32,
    /// where the status byte goes (0: the request had none)
    pub status_gpa: u64,
    /// the guest clock when the request came
    pub since: u64,
    /// the guest clock when it completes with an I/O error
    pub release: Option<u64>,
}

/// The block faults' state (saved and restored with the devices).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlkFaults {
    pub cfg: BlkCfg,
    /// requests (every type) and writes since the workload's start, counted while `cfg` arms
    /// something
    pub nreq: u64,
    pub nwrite: u64,
    /// the bad sectors and whether a read of each returns damaged bytes (`rot`) instead of an
    /// error; they last for the whole run, across a power cut
    pub bad: BTreeMap<u64, bool>,
    pub held: Vec<Held>,
}

/// What one request of the data disk is to suffer, decided at its header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReqFault {
    /// the request's number, when counted
    pub n: u64,
    /// hold it (`hang=`)
    pub hold: bool,
    /// write its data this many sectors away (`mis=`)
    pub mis: Option<i64>,
}

impl BlkFaults {
    /// Nothing armed and nothing left: the state a run without the faults keeps all along.
    pub fn inert(&self) -> bool {
        *self == BlkFaults::default()
    }

    /// The fingerprint's part: nothing while [`BlkFaults::inert`].
    pub fn hash(&self, mut h: u64) -> u64 {
        if self.inert() {
            return h;
        }
        h = fnv(b"blk313", h);
        let c = &self.cfg;
        h = fnv_u64(c.seed, h);
        let (tp, ta) = c.torn.unwrap_or((0, false));
        h = fnv_u64(
            tp as u64 | (ta as u64) << 32 | (c.torn.is_some() as u64) << 40,
            h,
        );
        let (mk, md) = c.mis.unwrap_or((0, None));
        h = fnv_u64(mk, h);
        h = fnv_u64(md.unwrap_or(0) as u64, h);
        let (hk, hm) = c.hang.unwrap_or((0, None));
        h = fnv_u64(hk, h);
        h = fnv_u64(hm.unwrap_or(0), h);
        h = fnv_u64(self.nreq, h);
        h = fnv_u64(self.nwrite, h);
        h = fnv_u64(self.bad.len() as u64, h);
        for (s, rot) in &self.bad {
            h = fnv_u64(*s, h);
            h = fnv_u64(*rot as u64, h);
        }
        for x in &self.held {
            h = fnv_u64(x.n, h);
            h = fnv_u64(x.head as u64, h);
            h = fnv_u64(x.used_len as u64, h);
            h = fnv_u64(x.status_gpa, h);
            h = fnv_u64(x.since, h);
            h = fnv_u64(x.release.map_or(u64::MAX, |r| r), h);
        }
        h
    }

    /// Arm a workload's faults: the counters from 0, no request held. The bad sectors stay.
    pub fn arm(&mut self, cfg: BlkCfg) {
        self.cfg = cfg;
        self.nreq = 0;
        self.nwrite = 0;
        self.held.clear();
    }

    /// The workload's end (the heal): the faults that act on requests go off. The bad sectors
    /// and a request already held stay, as on a real disk.
    pub fn off(&mut self) {
        self.cfg = BlkCfg {
            seed: self.cfg.seed,
            ..BlkCfg::default()
        };
        if self.inert_but_seed() {
            *self = BlkFaults::default();
        }
    }

    fn inert_but_seed(&self) -> bool {
        !self.cfg.any() && self.bad.is_empty() && self.held.is_empty() && self.nreq == 0
    }

    /// Add bad sectors (`lse=`, the host's `DISK`).
    pub fn add_bad(&mut self, ranges: &[(u64, u64)], rot: bool) {
        for (a, n) in ranges {
            for s in *a..a.saturating_add(*n) {
                self.bad.insert(s, rot);
            }
        }
    }

    /// The decision for a request of type `ty` (0 read, 1 write, other: flush and the rest) at
    /// its header; counts it when something is armed.
    pub fn decide(&mut self, write: bool) -> ReqFault {
        if !self.cfg.any() {
            return ReqFault::default();
        }
        self.nreq += 1;
        let mut f = ReqFault {
            n: self.nreq,
            ..ReqFault::default()
        };
        if let Some((k, _)) = self.cfg.hang {
            f.hold = self.nreq == k;
        }
        if write {
            self.nwrite += 1;
            if let Some((k, d)) = self.cfg.mis {
                if self.nwrite == k {
                    f.mis = Some(d.unwrap_or_else(|| self.mis_delta(k)));
                }
            }
        }
        f
    }

    /// A seeded distance of 1 to [`MIS_BLOCKS`] blocks of 4 KiB, in either direction.
    fn mis_delta(&self, k: u64) -> i64 {
        let h = mix((self.cfg.seed ^ MIS_SALT).wrapping_add(k.wrapping_mul(GOLDEN)));
        let blocks = 1 + (h % MIS_BLOCKS) as i64;
        if (h >> 32) & 1 == 0 {
            blocks * 8
        } else {
            -blocks * 8
        }
    }

    /// Where a misdirected write of `len` bytes meant for `doff` goes on a disk of `size`
    /// bytes: the other direction when the drawn one leaves the disk, None when both do.
    pub fn mis_target(doff: u64, len: u64, delta: i64, size: u64) -> Option<u64> {
        let fits = |d: i64| -> Option<u64> {
            let t = doff as i128 + d as i128 * SECTOR as i128;
            (t >= 0 && t as u128 + len as u128 <= size as u128).then_some(t as u64)
        };
        fits(delta).or_else(|| fits(-delta))
    }

    /// Whether any byte of [doff, doff+len) lies on a bad sector: (an error sector, a rotten
    /// sector).
    pub fn bad_in(&self, doff: u64, len: u64) -> (bool, bool) {
        if self.bad.is_empty() || len == 0 {
            return (false, false);
        }
        let (a, b) = (doff / SECTOR, (doff + len).div_ceil(SECTOR));
        let mut out = (false, false);
        for (_, rot) in self.bad.range(a..b) {
            if *rot {
                out.1 = true;
            } else {
                out.0 = true;
            }
        }
        out
    }

    /// Damage `buf` (the bytes read from `doff`) where it lies on a rotten sector: 16 seeded
    /// bytes of each such sector, the same every time it is read.
    pub fn rot(&self, doff: u64, buf: &mut [u8]) {
        let len = buf.len() as u64;
        let (a, b) = (doff / SECTOR, (doff + len).div_ceil(SECTOR));
        for (s, rot) in self.bad.range(a..b) {
            if !*rot {
                continue;
            }
            let mut h = mix((self.cfg.seed ^ ROT_SALT).wrapping_add(s.wrapping_mul(GOLDEN)));
            let at = s * SECTOR + (h % (SECTOR - 16));
            for i in 0..16u64 {
                h = mix(h);
                let p = at + i;
                if p >= doff && p < doff + len {
                    buf[(p - doff) as usize] ^= (h as u8) | 1;
                }
            }
        }
    }

    /// A write of [doff, doff+len) repairs the whole sectors it covers (the disk remaps them).
    pub fn repair(&mut self, doff: u64, len: u64) {
        if self.bad.is_empty() || len == 0 {
            return;
        }
        let (a, b) = (doff.div_ceil(SECTOR), (doff + len) / SECTOR);
        if a >= b {
            return;
        }
        let gone: Vec<u64> = self.bad.range(a..b).map(|(s, _)| *s).collect();
        for s in gone {
            self.bad.remove(&s);
        }
    }

    /// At a power cut: which sectors of an unflushed write of `len` bytes (the `i`-th valid
    /// entry of the cache, from 0) reach the disk. None: the whole write is lost, as without
    /// `torn=`.
    pub fn torn_keep(&self, i: u64, len: u64) -> Option<Vec<bool>> {
        let (pct, any) = self.cfg.torn?;
        let n = len.div_ceil(SECTOR);
        if n < 2 {
            return None;
        }
        let mut h = mix((self.cfg.seed ^ TORN_SALT).wrapping_add((i + 1).wrapping_mul(GOLDEN)));
        if (h % 100) as u32 >= pct {
            return None;
        }
        let mut keep = vec![false; n as usize];
        if !any {
            let k = 1 + (h >> 8) % (n - 1);
            for x in keep.iter_mut().take(k as usize) {
                *x = true;
            }
            return Some(keep);
        }
        for x in keep.iter_mut() {
            h = mix(h);
            *x = h & 1 == 1;
        }
        // a part: never none, never all
        let kept = keep.iter().filter(|k| **k).count();
        if kept == 0 {
            keep[((h >> 8) % n) as usize] = true;
        } else if kept == n as usize {
            keep[((h >> 8) % n) as usize] = false;
        }
        Some(keep)
    }

    /// The longest a held request without a release has waited, in guest ticks.
    pub fn hung_for(&self, now: u64) -> Option<u64> {
        self.held
            .iter()
            .filter(|h| h.release.is_none())
            .map(|h| now.saturating_sub(h.since))
            .max()
    }

    /// The guest ticks of `ms` milliseconds.
    pub fn ticks(ticks_per_s: u64, ms: u64) -> u64 {
        let t = if ticks_per_s == 0 {
            TICKS_DEFAULT
        } else {
            ticks_per_s
        };
        ms.saturating_mul(t / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inert_without_faults() {
        let mut b = BlkFaults::default();
        assert!(b.inert());
        assert_eq!(b.hash(7), 7);
        assert_eq!(b.decide(true), ReqFault::default());
        assert!(b.inert(), "nothing is counted without an armed fault");
        b.off();
        assert!(b.inert());
        b.arm(BlkCfg {
            seed: 3,
            ..BlkCfg::default()
        });
        b.off();
        assert!(b.inert(), "a seed alone leaves nothing behind");
    }

    #[test]
    fn hang_and_misdirection_pick_their_request() {
        let mut b = BlkFaults::default();
        b.arm(BlkCfg {
            seed: 1,
            mis: Some((2, None)),
            hang: Some((3, None)),
            ..BlkCfg::default()
        });
        let r1 = b.decide(false);
        let r2 = b.decide(true);
        let r3 = b.decide(true);
        assert_eq!((r1.hold, r1.mis), (false, None));
        assert!(!r2.hold);
        assert!(
            r3.hold && r3.mis.is_some(),
            "the second write is the third request"
        );
        let d = r3.mis.unwrap();
        assert!(d % 8 == 0 && d.unsigned_abs() <= MIS_BLOCKS * 8 && d != 0);
        assert!(!b.inert());
        assert_ne!(b.hash(7), 7);
    }

    #[test]
    fn a_misdirected_write_stays_on_the_disk() {
        assert_eq!(
            BlkFaults::mis_target(4096, 4096, -16, 1 << 20),
            Some(4096 + 8192)
        );
        assert_eq!(BlkFaults::mis_target(8192, 4096, -16, 1 << 20), Some(0));
        assert_eq!(BlkFaults::mis_target(0, 4096, 1 << 30, 8192), None);
    }

    #[test]
    fn bad_sectors_fail_rot_and_repair() {
        let mut b = BlkFaults::default();
        b.add_bad(&[(10, 2)], false);
        b.add_bad(&[(20, 1)], true);
        assert_eq!(b.bad_in(10 * 512, 512), (true, false));
        assert_eq!(b.bad_in(12 * 512, 4096), (false, false));
        assert_eq!(b.bad_in(16 * 512, 4096), (false, true));
        let mut buf = vec![0u8; 4096];
        b.rot(16 * 512, &mut buf);
        let changed = buf.iter().filter(|x| **x != 0).count();
        assert_eq!(changed, 16, "16 bytes of the rotten sector");
        let mut again = vec![0u8; 4096];
        b.rot(16 * 512, &mut again);
        assert_eq!(buf, again, "the same damage every read");
        b.repair(10 * 512 + 1, 1024);
        assert!(
            b.bad.contains_key(&10),
            "a part of a sector repairs nothing"
        );
        assert!(!b.bad.contains_key(&11));
        b.repair(10 * 512, 512);
        assert!(!b.bad.contains_key(&10));
    }

    #[test]
    fn torn_writes_keep_a_part() {
        let mut b = BlkFaults::default();
        b.arm(BlkCfg {
            seed: 9,
            torn: Some((100, false)),
            ..BlkCfg::default()
        });
        for i in 0..50 {
            let k = b.torn_keep(i, 4096).unwrap();
            let n = k.iter().filter(|x| **x).count();
            assert!((1..8).contains(&n));
            assert!(k[..n].iter().all(|x| *x), "a prefix");
        }
        assert_eq!(b.torn_keep(0, 512), None, "one sector cannot tear");
        b.cfg.torn = Some((100, true));
        for i in 0..50 {
            let k = b.torn_keep(i, 4096).unwrap();
            let n = k.iter().filter(|x| **x).count();
            assert!((1..8).contains(&n));
        }
        b.cfg.torn = None;
        assert_eq!(b.torn_keep(0, 4096), None);
    }
}
