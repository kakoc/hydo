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
}

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
        }
    }
}
