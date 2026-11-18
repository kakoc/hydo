//! The host part of a schedule: what the engine applies outside the guest (disk faults, the tick
//! clock's jitter and preemption, vCPU turns, the run budget, the power cut at a FLUSH).
//!
//! The engines read these tokens with `strstr` (hvx_explore.c `parse_sched`, serve.c `run_once`),
//! in any order. Here every token is one variant; the order as written is kept. Host numbers are
//! read the way the C reads them (decimal, or hex with an optional `0x` and either case, leading
//! zeros allowed) and printed canonically (decimal; lower-case hex without prefix or leading
//! zeros, as explore.py prints them): the host part never reaches the guest, so normalising it
//! changes nothing in a run.

use crate::num::{err, host_dec, host_hex};
use crate::ParseError;
use std::fmt;

/// `disk=`: the data disk's seeded fault class (hvx_explore.c parse_sched; serve.c parse_disk).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Disk {
    /// `none`: no disk fault.
    None,
    /// `cut`: the volatile write cache is honoured, so a power cut loses the unflushed writes.
    Cut,
    /// `ioerr:<P>`: seeded I/O errors with probability P per mille-ish (the module's units).
    IoErr(u64),
    /// `lying:<P>`: a lying disk that drops writes it acknowledged.
    Lying(u64),
}

impl Disk {
    fn parse(v: &str) -> Result<Disk, ParseError> {
        // hvx_explore.c: anything else returns -1 ("bad schedule"). The C reads the probability
        // with atoi, so "ioerr:" alone is accepted there as probability 0; it is refused here.
        match v {
            "none" => Ok(Disk::None),
            "cut" => Ok(Disk::Cut),
            _ => {
                if let Some(p) = v.strip_prefix("ioerr:") {
                    Ok(Disk::IoErr(host_dec(p, "disk=ioerr:")?))
                } else if let Some(p) = v.strip_prefix("lying:") {
                    Ok(Disk::Lying(host_dec(p, "disk=lying:")?))
                } else {
                    Err(err(format!(
                        "disk={v}: the disk is none, cut, ioerr:<P> or lying:<P>"
                    )))
                }
            }
        }
    }
}

impl fmt::Display for Disk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Disk::None => f.write_str("none"),
            Disk::Cut => f.write_str("cut"),
            Disk::IoErr(p) => write!(f, "ioerr:{p}"),
            Disk::Lying(p) => write!(f, "lying:{p}"),
        }
    }
}

/// One host token. Each variant is one `key=value` word (or the bare word `cfn`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HostToken {
    /// `disk=` (default `none`).
    Disk(Disk),
    /// `dseed=<hex>`: the seed of the disk's fault decisions (default 0).
    Dseed(u64),
    /// `tj=<hex>`: seeded tick jitter (hvx: PMU ticks delayed by guest branches; hvm: by exits).
    Tj(u64),
    /// `tjpm=<n>`: delayed ticks per 1000 (default 10).
    Tjpm(u64),
    /// `tjmax=<n>`: the largest delay (hvx default 500000 branches, hvm 1000 exits).
    Tjmax(u64),
    /// `tjlo=<k>`: jitter only from tick k on.
    Tjlo(u64),
    /// `tjhi=<k>`: jitter only before tick k (0 = no bound).
    Tjhi(u64),
    /// `tjx=<k:d,...>`: an explicit list of (tick, delay), at most 32 (HVX_TJ_MAX_EXPLICIT).
    Tjx(Vec<(u64, u64)>),
    /// `pp=<branches>`: the PMU tick period during the workload (hvx).
    Pp(u64),
    /// `pptsc=<tsc>`: the virtual TSC advance per tick with `pp=` (hvx).
    Pptsc(u64),
    /// `xt=<hex>`: seeded extra ticks between the grid ticks (hvx, HVX_SET_XTICK).
    Xt(u64),
    /// `xtr=<n>`: extra-tick slots per period (default 4).
    Xtr(u64),
    /// `xtpm=<n>`: chance per 1000 that a slot is used (default 500).
    Xtpm(u64),
    /// `xtlo=<j>`.
    Xtlo(u64),
    /// `xthi=<j>`.
    Xthi(u64),
    /// `xtsc=<tsc>`.
    Xtsc(u64),
    /// `xtx=<b,b,...>`: extra ticks at these absolute guest branch counts, at most 32.
    Xtx(Vec<u64>),
    /// `cf=<k>`: the disk loses power at the k-th FLUSH of the workload (needs `disk=cut`, no `C=`).
    Cf(u64),
    /// `cfn`: count the FLUSHes only (the explorer's probe for the `cf=` range).
    Cfn,
    /// `rb=<n>`: every run call returns after at most n events (hvx, HVX_RUNC).
    Rb(u64),
    /// `q=<branches>`: every vCPU turn this long (hvx, several vCPUs).
    Q(u64),
    /// `qs=<hex>`: seeded turn lengths in [1, q].
    Qs(u64),
    /// `qb=<B>:<s>:<permille>`: seeded bursts of short turns (FASTTURN.md).
    Qb { len: u64, q: u64, permille: u64 },
    /// `qbs=<hex>`: the bursts' seed (needs `qb=`).
    Qbs(u64),
    /// `qm=<n>`: the landing margin of turn ends (at least 8).
    Qm(u64),
    /// `pct=<hex>`: forced preemption points at seeded exits (hvm, M57).
    Pct(u64),
    /// `pctn=<points>`.
    Pctn(u64),
    /// `pctk=<exits>` (default 20000).
    Pctk(u64),
    /// `spin=rewind`: run this schedule in the engine's rewind mode (hvm, serve.c; the KVM
    /// engine since Milestone 161).
    SpinRewind,
    /// `br=<clock>`: a branch point of the state search (docs/design/state-search.md). The
    /// timing tokens after it replace the run's timing from the first stop whose clock value is
    /// at least `<clock>`. The host controller takes it and what follows out of the engine's
    /// schedule and sends the engine `TIME` at that stop.
    Br(u64),
    /// `torn=<p>[:any]` (Milestone 313, docs/design/block-faults.md): at a power cut, each
    /// write of two or more sectors that the disk took and nobody flushed is torn with
    /// probability `p` percent: a seeded part of its sectors reaches the disk, a prefix, or
    /// with `:any` any subset (never none and never all). Needs `disk=cut`. The Mac and KVM
    /// engines; hvx refuses it.
    Torn { pct: u64, any: bool },
    /// `mis=<k>[:<sectors>]` (Milestone 313): the k-th write request of the data disk from
    /// the workload's start (from 1) is written `<sectors>` away (signed), or without it a seeded
    /// distance of 1 to 16 blocks of 4 KiB in either direction; the sectors it was meant for
    /// keep their old bytes.
    Mis { k: u64, delta: Option<i64> },
    /// `lse=<sector>+<count>[,<sector>+<count>...][:rot]` (Milestone 313): latent sector
    /// errors on the data disk. A read that touches one of the sectors fails with an I/O error,
    /// or with `:rot` returns seeded damaged bytes for it; a write of the sector repairs it, as
    /// a disk remaps a sector it can write. They last for the whole run, across a power cut.
    Lse { ranges: Vec<(u64, u64)>, rot: bool },
    /// `hang=<k>[+<ms>]` (Milestone 313): the k-th request (read, write or flush) of the data
    /// disk from the workload's start (from 1) gets no completion, or with `+<ms>` completes
    /// with an I/O error after that many milliseconds of guest time. A guest that waits on a
    /// request that never completes for [`HANG_LIMIT_S`] seconds of guest time ends the run
    /// with a verdict that says so.
    Hang { k: u64, ms: Option<u64> },
}

/// The keys of the block faults of Milestone 313 (docs/design/block-faults.md): the Mac and
/// KVM engines apply them, hvx refuses them.
pub const BLOCK_KEYS: [&str; 4] = ["torn", "mis", "lse", "hang"];

/// The guest seconds a request of `hang=` without `+<ms>` may stay open before the engine ends
/// the run (Linux's own hung-task report comes after 120 s).
pub const HANG_LIMIT_S: u64 = 120;

/// The most ranges of one `lse=` token.
pub const LSE_MAX_RANGES: usize = 16;

/// The most sectors of one `lse=` range (1 MiB).
pub const LSE_MAX_COUNT: u64 = 2048;

/// The farthest a `mis=` write may be sent, in sectors (1 GiB).
pub const MIS_MAX_DELTA: u64 = 1 << 21;

/// The longest delay of a `hang=<k>+<ms>` completion (one hour of guest time).
pub const HANG_MAX_MS: u64 = 3_600_000;

/// The keys a branch point's segment may hold: the timing an engine changes at a stop.
pub const BRANCH_KEYS: [&str; 9] = [
    "tj", "tjpm", "tjmax", "tjlo", "tjhi", "tjx", "pct", "pctn", "pctk",
];

/// The longest explicit lists the module takes (hvx_abi.h HVX_TJ_MAX_EXPLICIT, HVX_XT_MAX_EXPLICIT).
pub const MAX_EXPLICIT: usize = 32;

impl HostToken {
    /// The token's key (`cfn` for the bare word): two tokens with one key are refused.
    pub fn key(&self) -> &'static str {
        match self {
            HostToken::Disk(_) => "disk",
            HostToken::Dseed(_) => "dseed",
            HostToken::Tj(_) => "tj",
            HostToken::Tjpm(_) => "tjpm",
            HostToken::Tjmax(_) => "tjmax",
            HostToken::Tjlo(_) => "tjlo",
            HostToken::Tjhi(_) => "tjhi",
            HostToken::Tjx(_) => "tjx",
            HostToken::Pp(_) => "pp",
            HostToken::Pptsc(_) => "pptsc",
            HostToken::Xt(_) => "xt",
            HostToken::Xtr(_) => "xtr",
            HostToken::Xtpm(_) => "xtpm",
            HostToken::Xtlo(_) => "xtlo",
            HostToken::Xthi(_) => "xthi",
            HostToken::Xtsc(_) => "xtsc",
            HostToken::Xtx(_) => "xtx",
            HostToken::Cf(_) => "cf",
            HostToken::Cfn => "cfn",
            HostToken::Rb(_) => "rb",
            HostToken::Q(_) => "q",
            HostToken::Qs(_) => "qs",
            HostToken::Qb { .. } => "qb",
            HostToken::Qbs(_) => "qbs",
            HostToken::Qm(_) => "qm",
            HostToken::Pct(_) => "pct",
            HostToken::Pctn(_) => "pctn",
            HostToken::Pctk(_) => "pctk",
            HostToken::SpinRewind => "spin",
            HostToken::Br(_) => "br",
            HostToken::Torn { .. } => "torn",
            HostToken::Mis { .. } => "mis",
            HostToken::Lse { .. } => "lse",
            HostToken::Hang { .. } => "hang",
        }
    }

    /// One host word.
    pub fn parse(word: &str) -> Result<HostToken, ParseError> {
        if word == "cfn" {
            return Ok(HostToken::Cfn);
        }
        let (k, v) = word
            .split_once('=')
            .ok_or_else(|| err(format!("host token {word:?}: expected key=value (or cfn)")))?;
        let w = |what: &str| format!("{k}= ({what})");
        let t = match k {
            "disk" => HostToken::Disk(Disk::parse(v)?),
            "dseed" => HostToken::Dseed(host_hex(v, "dseed=")?),
            "tj" => HostToken::Tj(host_hex(v, "tj=")?),
            "tjpm" => HostToken::Tjpm(host_dec(v, "tjpm=")?),
            "tjmax" => HostToken::Tjmax(host_dec(v, "tjmax=")?),
            "tjlo" => HostToken::Tjlo(host_dec(v, "tjlo=")?),
            "tjhi" => HostToken::Tjhi(host_dec(v, "tjhi=")?),
            "tjx" => {
                // hvx_explore.c parse_tj: entries <tick>:<delay> separated by ','; a missing ':'
                // or more than 32 entries returns -1. An empty list (tjx=) is accepted there and
                // means "no jitter"; it is refused here.
                let mut out = Vec::new();
                for e in v.split(',') {
                    let (a, b) = e
                        .split_once(':')
                        .ok_or_else(|| err(format!("tjx=: entry {e:?} is not <tick>:<delay>")))?;
                    out.push((host_dec(a, "tjx= tick")?, host_dec(b, "tjx= delay")?));
                }
                if out.len() > MAX_EXPLICIT {
                    return Err(err(format!(
                        "tjx=: {} entries, at most {MAX_EXPLICIT} (HVX_TJ_MAX_EXPLICIT)",
                        out.len()
                    )));
                }
                HostToken::Tjx(out)
            }
            "pp" => HostToken::Pp(host_dec(v, "pp=")?),
            "pptsc" => HostToken::Pptsc(host_dec(v, "pptsc=")?),
            "xt" => HostToken::Xt(host_hex(v, "xt=")?),
            "xtr" => HostToken::Xtr(host_dec(v, "xtr=")?),
            "xtpm" => HostToken::Xtpm(host_dec(v, "xtpm=")?),
            "xtlo" => HostToken::Xtlo(host_dec(v, "xtlo=")?),
            "xthi" => HostToken::Xthi(host_dec(v, "xthi=")?),
            "xtsc" => HostToken::Xtsc(host_dec(v, "xtsc=")?),
            "xtx" => {
                // parse_preempt: strtoull per entry, end == p returns -1; more than 32 returns -1.
                let mut out = Vec::new();
                for e in v.split(',') {
                    out.push(host_dec(e, "xtx= entry")?);
                }
                if out.len() > MAX_EXPLICIT {
                    return Err(err(format!(
                        "xtx=: {} entries, at most {MAX_EXPLICIT} (HVX_XT_MAX_EXPLICIT)",
                        out.len()
                    )));
                }
                HostToken::Xtx(out)
            }
            "cf" => {
                let n = host_dec(v, "cf=")?;
                if n == 0 {
                    // hvx_explore.c parse_sched: !sc->cf returns -1
                    return Err(err("cf=0: the FLUSH count starts at 1"));
                }
                HostToken::Cf(n)
            }
            "rb" => HostToken::Rb(host_dec(v, "rb=")?),
            "q" => {
                let n = host_dec(v, "q=")?;
                if n == 0 {
                    // parse_turns: !sc->q returns -1
                    return Err(err("q=0: a turn is at least one branch"));
                }
                HostToken::Q(n)
            }
            "qs" => {
                let n = host_hex(v, "qs=")?;
                if n == 0 {
                    // parse_turns: !sc->qs returns -1
                    return Err(err("qs=0: the turn seed is not 0"));
                }
                HostToken::Qs(n)
            }
            "qb" => {
                // parse_turns: <B>:<s>:<permille>, B and s not 0, permille at most 1000
                let f: Vec<&str> = v.split(':').collect();
                if f.len() != 3 {
                    return Err(err(format!("qb={v}: expected <B>:<s>:<permille>")));
                }
                let (len, q, permille) = (
                    host_dec(f[0], &w("burst length"))?,
                    host_dec(f[1], &w("turn length in a burst"))?,
                    host_dec(f[2], &w("permille"))?,
                );
                if len == 0 || q == 0 || permille > 1000 {
                    return Err(err(format!(
                        "qb={v}: B and s are at least 1 and permille is at most 1000"
                    )));
                }
                HostToken::Qb { len, q, permille }
            }
            "qbs" => {
                let n = host_hex(v, "qbs=")?;
                if n == 0 {
                    return Err(err("qbs=0: the burst seed is not 0"));
                }
                HostToken::Qbs(n)
            }
            "qm" => {
                let n = host_dec(v, "qm=")?;
                if n < 8 {
                    // parse_turns: sc->tc.margin < 8 returns -1
                    return Err(err(format!("qm={n}: the margin is at least 8")));
                }
                HostToken::Qm(n)
            }
            "pct" => HostToken::Pct(host_hex(v, "pct=")?),
            "pctn" => HostToken::Pctn(host_dec(v, "pctn=")?),
            "pctk" => HostToken::Pctk(host_dec(v, "pctk=")?),
            "br" => HostToken::Br(host_dec(v, "br=")?),
            "torn" => {
                let (p, any) = match v.split_once(':') {
                    Some((p, "any")) => (p, true),
                    Some((_, m)) => {
                        return Err(err(format!(
                            "torn={v}: the only mode after ':' is any (got {m:?})"
                        )))
                    }
                    None => (v, false),
                };
                let pct = host_dec(p, "torn=")?;
                if pct == 0 || pct > 100 {
                    return Err(err(format!("torn={v}: the percentage is 1 to 100")));
                }
                HostToken::Torn { pct, any }
            }
            "mis" => {
                let (k, d) = match v.split_once(':') {
                    Some((k, d)) => (k, Some(d)),
                    None => (v, None),
                };
                let k = host_dec(k, "mis=")?;
                if k == 0 {
                    return Err(err("mis=0: the writes are counted from 1"));
                }
                let delta = match d {
                    None => None,
                    Some(d) => {
                        let (neg, digits) = match d.strip_prefix('-') {
                            Some(x) => (true, x),
                            None => (false, d.strip_prefix('+').unwrap_or(d)),
                        };
                        let n = host_dec(digits, "mis= distance")?;
                        if n == 0 || n > MIS_MAX_DELTA {
                            return Err(err(format!(
                                "mis={v}: the distance is 1 to {MIS_MAX_DELTA} sectors, with a sign"
                            )));
                        }
                        Some(if neg { -(n as i64) } else { n as i64 })
                    }
                };
                HostToken::Mis { k, delta }
            }
            "lse" => {
                let (l, rot) = match v.split_once(':') {
                    Some((l, "rot")) => (l, true),
                    Some((_, m)) => {
                        return Err(err(format!(
                            "lse={v}: the only mode after ':' is rot (got {m:?})"
                        )))
                    }
                    None => (v, false),
                };
                let mut ranges = Vec::new();
                for e in l.split(',') {
                    let (a, n) = e
                        .split_once('+')
                        .ok_or_else(|| err(format!("lse=: entry {e:?} is not <sector>+<count>")))?;
                    let (a, n) = (host_dec(a, "lse= sector")?, host_dec(n, "lse= count")?);
                    if n == 0 || n > LSE_MAX_COUNT {
                        return Err(err(format!(
                            "lse=: entry {e:?}: the count is 1 to {LSE_MAX_COUNT} sectors"
                        )));
                    }
                    ranges.push((a, n));
                }
                if ranges.len() > LSE_MAX_RANGES {
                    return Err(err(format!(
                        "lse=: {} ranges, at most {LSE_MAX_RANGES}",
                        ranges.len()
                    )));
                }
                HostToken::Lse { ranges, rot }
            }
            "hang" => {
                let (k, ms) = match v.split_once('+') {
                    Some((k, m)) => (k, Some(m)),
                    None => (v, None),
                };
                let k = host_dec(k, "hang=")?;
                if k == 0 {
                    return Err(err("hang=0: the requests are counted from 1"));
                }
                let ms = match ms {
                    None => None,
                    Some(m) => {
                        let n = host_dec(m, "hang= delay")?;
                        if n == 0 || n > HANG_MAX_MS {
                            return Err(err(format!(
                                "hang={v}: the delay is 1 to {HANG_MAX_MS} ms"
                            )));
                        }
                        Some(n)
                    }
                };
                HostToken::Hang { k, ms }
            }
            "spin" => {
                if v != "rewind" {
                    return Err(err(format!("spin={v}: the only value is rewind")));
                }
                HostToken::SpinRewind
            }
            _ => {
                // the engines ignore a word they do not know; refusing it catches typos
                return Err(err(format!("unknown host token {word:?}")));
            }
        };
        Ok(t)
    }
}

impl fmt::Display for HostToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let k = self.key();
        match self {
            HostToken::Disk(d) => write!(f, "disk={d}"),
            HostToken::Dseed(v)
            | HostToken::Tj(v)
            | HostToken::Xt(v)
            | HostToken::Qs(v)
            | HostToken::Qbs(v)
            | HostToken::Pct(v) => write!(f, "{k}={v:x}"),
            HostToken::Tjpm(v)
            | HostToken::Tjmax(v)
            | HostToken::Tjlo(v)
            | HostToken::Tjhi(v)
            | HostToken::Pp(v)
            | HostToken::Pptsc(v)
            | HostToken::Xtr(v)
            | HostToken::Xtpm(v)
            | HostToken::Xtlo(v)
            | HostToken::Xthi(v)
            | HostToken::Xtsc(v)
            | HostToken::Cf(v)
            | HostToken::Rb(v)
            | HostToken::Q(v)
            | HostToken::Qm(v)
            | HostToken::Pctn(v)
            | HostToken::Pctk(v)
            | HostToken::Br(v) => write!(f, "{k}={v}"),
            HostToken::Tjx(l) => {
                f.write_str("tjx=")?;
                for (i, (a, b)) in l.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{a}:{b}")?;
                }
                Ok(())
            }
            HostToken::Xtx(l) => {
                f.write_str("xtx=")?;
                for (i, a) in l.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{a}")?;
                }
                Ok(())
            }
            HostToken::Cfn => f.write_str("cfn"),
            HostToken::Qb { len, q, permille } => write!(f, "qb={len}:{q}:{permille}"),
            HostToken::SpinRewind => f.write_str("spin=rewind"),
            HostToken::Torn { pct, any } => {
                write!(f, "torn={pct}{}", if *any { ":any" } else { "" })
            }
            HostToken::Mis { k, delta } => match delta {
                None => write!(f, "mis={k}"),
                Some(d) if *d > 0 => write!(f, "mis={k}:+{d}"),
                Some(d) => write!(f, "mis={k}:{d}"),
            },
            HostToken::Lse { ranges, rot } => {
                f.write_str("lse=")?;
                for (i, (a, n)) in ranges.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{a}+{n}")?;
                }
                if *rot {
                    f.write_str(":rot")?;
                }
                Ok(())
            }
            HostToken::Hang { k, ms } => match ms {
                None => write!(f, "hang={k}"),
                Some(m) => write!(f, "hang={k}+{m}"),
            },
        }
    }
}
