//! What one schedule arms in the module: the values the engine passes to `/dev/hvx` (disk
//! faults, tick jitter, preemption, extra ticks, vCPU turns, the run budget, the flush cut) and
//! the guest text it types, computed from hydo-schedule's typed [`Schedule`].
//!
//! The engine parses no schedule text (M142): every caller hands it a [`Schedule`] (in process:
//! hydo-engine; the program `hydo-hvx`: its request reader parses each line with
//! [`Schedule::parse`]). [`Sched::from_schedule`] maps the typed tokens to the fields the C
//! engine's `parse_sched` filled, with its defaults (`tjpm` 10, `tjmax` 500000, `xtr` 4, `xtpm`
//! 500, a burst seed of 1 when `qb=` has no `qbs=`); `tests/parse_vs_c.rs` checks that on every
//! schedule hydo-schedule accepts the fields are the C parser's (recorded in
//! `tests/golden/parse_sched_c.txt`) and lists the inputs the C engine ran and hydo-schedule
//! refuses. Host tokens for the Mac engine only (`pct*`, `spin=`) arm nothing here, as before.

use crate::abi::*;
use hydo_schedule::{Disk, GuestToken, HostToken, Schedule};

#[derive(Clone, Copy)]
pub struct Sched {
    /// The guest part as typed on the console (`run` and the tokens), at most 511 bytes.
    pub guest: [u8; 512],
    pub guest_len: usize,
    pub classes: u32,
    pub prob: u32,
    pub dseed: u64,
    pub cut: bool,
    pub tj_on: bool,
    pub tj: hvx_tjitter,
    pub rb: u64,
    pub pp: u64,
    pub pptsc: u64,
    pub xt_on: bool,
    pub xt: hvx_xtick,
    pub cf_on: bool,
    pub cf: u64,
    pub q_on: bool,
    pub q: u64,
    pub qs: u64,
    pub tc_on: bool,
    pub tc: hvx_turnctl,
}

impl Sched {
    pub fn empty() -> Sched {
        Sched {
            guest: [0; 512],
            guest_len: 0,
            classes: 0,
            prob: 0,
            dseed: 0,
            cut: false,
            tj_on: false,
            tj: hvx_tjitter::default(),
            rb: 0,
            pp: 0,
            pptsc: 0,
            xt_on: false,
            xt: hvx_xtick::default(),
            cf_on: false,
            cf: 0,
            q_on: false,
            q: 0,
            qs: 0,
            tc_on: false,
            tc: hvx_turnctl::default(),
        }
    }

    pub fn guest(&self) -> &[u8] {
        &self.guest[..self.guest_len]
    }

    /// The module's values for `s`. hydo-schedule has already refused what the C engine refused
    /// (and more: see `tests/parse_vs_c.rs`), so this cannot fail.
    pub fn from_schedule(s: &Schedule) -> Sched {
        let mut sc = Sched::empty();
        let g = s.guest_text();
        let g = g.as_bytes();
        // hydo_schedule::GUEST_MAX_BYTES (511) is the C buffer's limit
        let n = g.len().min(511);
        sc.guest[..n].copy_from_slice(&g[..n]);
        sc.guest_len = n;
        match s.disk() {
            Disk::None => {}
            Disk::Cut => {
                sc.classes = HVX_FAULT_DROP_UNFLUSHED;
                sc.prob = 100;
            }
            Disk::IoErr(p) => {
                sc.classes = HVX_FAULT_ERROR;
                sc.prob = p as u32;
            }
            Disk::Lying(p) => {
                sc.classes = HVX_FAULT_DROP;
                sc.prob = p as u32;
            }
        }
        sc.dseed = s.dseed();
        let num = |k: &str| match s.host_token(k) {
            Some(
                HostToken::Dseed(v)
                | HostToken::Tj(v)
                | HostToken::Tjpm(v)
                | HostToken::Tjmax(v)
                | HostToken::Tjlo(v)
                | HostToken::Tjhi(v)
                | HostToken::Pp(v)
                | HostToken::Pptsc(v)
                | HostToken::Xt(v)
                | HostToken::Xtr(v)
                | HostToken::Xtpm(v)
                | HostToken::Xtlo(v)
                | HostToken::Xthi(v)
                | HostToken::Xtsc(v)
                | HostToken::Cf(v)
                | HostToken::Rb(v)
                | HostToken::Q(v)
                | HostToken::Qs(v)
                | HostToken::Qbs(v)
                | HostToken::Qm(v)
                | HostToken::Pct(v)
                | HostToken::Pctn(v)
                | HostToken::Pctk(v),
            ) => Some(*v),
            _ => None,
        };
        // tick jitter: an explicit list, else the seeded form with its defaults
        if let Some(HostToken::Tjx(l)) = s.host_token("tjx") {
            for (i, (tick, delay)) in l.iter().take(HVX_TJ_MAX_EXPLICIT).enumerate() {
                sc.tj.explicit_[i] = hvx_tj_explicit {
                    tick: *tick,
                    delay: *delay,
                };
                sc.tj.n_explicit += 1;
            }
            sc.tj_on = sc.tj.n_explicit > 0;
        } else if let Some(seed) = num("tj") {
            sc.tj.seed = seed;
            sc.tj.permille = num("tjpm").map_or(10, |v| v as u32);
            sc.tj.max_delay = num("tjmax").unwrap_or(500000);
            sc.tj.tick_lo = num("tjlo").unwrap_or(0);
            sc.tj.tick_hi = num("tjhi").unwrap_or(0);
            sc.tj_on = sc.tj.permille != 0 && sc.tj.max_delay != 0;
        }
        // preemption and extra ticks
        sc.pp = num("pp").unwrap_or(0);
        sc.pptsc = num("pptsc").unwrap_or(0);
        sc.xt.tsc = num("xtsc").unwrap_or(0);
        if let Some(HostToken::Xtx(l)) = s.host_token("xtx") {
            for (i, b) in l.iter().take(HVX_XT_MAX_EXPLICIT).enumerate() {
                sc.xt.explicit_[i] = *b;
                sc.xt.n_explicit += 1;
            }
            sc.xt_on = sc.xt.n_explicit > 0;
        } else if let Some(seed) = num("xt") {
            sc.xt.seed = seed;
            sc.xt.rate = num("xtr").map_or(4, |v| v as u32);
            sc.xt.permille = num("xtpm").map_or(500, |v| v as u32);
            sc.xt.lo = num("xtlo").unwrap_or(0);
            sc.xt.hi = num("xthi").unwrap_or(0);
            sc.xt_on = sc.xt.rate != 0 && sc.xt.permille != 0;
        }
        // vCPU turns
        if let Some(q) = num("q") {
            sc.q = q;
            sc.q_on = true;
        }
        if let Some(qs) = num("qs") {
            sc.qs = qs;
            sc.q_on = true;
        }
        if let Some(HostToken::Qb { len, q, permille }) = s.host_token("qb") {
            sc.tc.burst_len = *len;
            sc.tc.burst_q = *q;
            sc.tc.burst_permille = *permille as u32;
            sc.tc.burst_seed = num("qbs").unwrap_or(1);
            sc.tc_on = true;
            sc.q_on = true;
        }
        if let Some(m) = num("qm") {
            sc.tc.margin = m;
            sc.tc_on = true;
            sc.q_on = true;
        }
        sc.rb = num("rb").unwrap_or(0);
        // the power cut: a guest C= token, or the k-th FLUSH (cf=); cfn counts FLUSHes only
        sc.cut = s.guest.iter().any(|t| matches!(t, GuestToken::Cut(_)));
        if let Some(cf) = num("cf") {
            sc.cf = cf;
            sc.cf_on = true;
            sc.cut = true;
        } else if s.host_token("cfn").is_some() {
            sc.cf_on = true;
        }
        sc
    }

    /// What the module would refuse when this schedule is armed, said before any VM runs it
    /// (None: nothing). The rules are hvx.c's: `HVX_SET_PMU` refuses a tick period of at most
    /// [`PMU_PERIOD_MIN`] branches; `HVX_SET_TJITTER` refuses a delay of a tick period or more
    /// (the period is `pp=`'s when the schedule has one, else the module's [`PMU_PERIOD`]), so
    /// `tjmax=500000` with `pp=65536` is refused; `HVX_SET_XTICK` refuses more than
    /// [`HVX_XT_MAX_RATE`] extra ticks per period. Until Milestone 208 such a schedule met the
    /// refusal after the restore and ended `ERROR harness tick jitter (needs the PMU clock)`,
    /// which the explorer counted as a failure. What depends on the machine (whether the PMU
    /// clock is on) is still found when the engine arms the schedule.
    pub fn refusal(&self) -> Option<String> {
        if self.pp != 0 && self.pp <= PMU_PERIOD_MIN {
            return Some(format!(
                "pp={}: hvx needs a tick period above {PMU_PERIOD_MIN} branches",
                self.pp
            ));
        }
        let (period, from) = if self.pp != 0 {
            (self.pp, format!("pp={}", self.pp))
        } else {
            (PMU_PERIOD, "the default".to_string())
        };
        if self.tj_on {
            if self.tj.n_explicit == 0 && self.tj.max_delay >= period {
                return Some(format!(
                    "tick jitter tjmax={} with a tick period of {period} branches ({from}): hvx \
                     refuses a delay of a tick period or more; use a tjmax below {period}",
                    self.tj.max_delay
                ));
            }
            let n = (self.tj.n_explicit as usize).min(HVX_TJ_MAX_EXPLICIT);
            if let Some(e) = self.tj.explicit_[..n].iter().find(|e| e.delay >= period) {
                return Some(format!(
                    "tick jitter tjx={}:{} with a tick period of {period} branches ({from}): hvx \
                     refuses a delay of a tick period or more",
                    e.tick, e.delay
                ));
            }
        }
        if self.xt_on && self.xt.n_explicit == 0 && self.xt.rate > HVX_XT_MAX_RATE {
            return Some(format!(
                "xtr={}: hvx takes at most {HVX_XT_MAX_RATE} extra ticks per tick period",
                self.xt.rate
            ));
        }
        None
    }
}

/// The module's tick period of the PMU clock when the schedule sets none (hvx.c
/// `HVX_PMU_PERIOD`), in guest branches.
pub const PMU_PERIOD: u64 = 1 << 20;
/// `HVX_SET_PMU` refuses a period of at most this many branches (hvx.c `2 * HVX_PMU_MARGIN`).
pub const PMU_PERIOD_MIN: u64 = 2 * 256;
/// `HVX_SET_XTICK`'s largest seeded rate (hvx_abi.h `HVX_XT_MAX_RATE`).
pub const HVX_XT_MAX_RATE: u32 = 64;

/// [`Sched::refusal`] of a typed schedule: what hvx refuses in it, before any VM.
pub fn refusal(s: &Schedule) -> Option<String> {
    Sched::from_schedule(s).refusal()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Sched {
        Sched::from_schedule(&Schedule::parse(s).unwrap())
    }

    #[test]
    fn host_tokens() {
        let s = p("disk=cut dseed=4d5cb825dfaab05b tj=7 tjpm=20 rb=16 | run K=redis@11+1 C=24\n");
        assert_eq!(s.classes, HVX_FAULT_DROP_UNFLUSHED);
        assert_eq!(s.prob, 100);
        assert_eq!(s.dseed, 0x4d5cb825dfaab05b);
        assert!(s.tj_on && s.tj.seed == 7 && s.tj.permille == 20 && s.tj.max_delay == 500000);
        assert_eq!(s.rb, 16);
        assert!(s.cut);
        assert_eq!(s.guest(), b"run K=redis@11+1 C=24");
        let c = p("disk=cut cf=3 | run");
        assert!(c.cf_on && c.cut && c.cf == 3);
        let q = p("disk=none q=5000 qs=ab qb=100:7:250 | run");
        assert!(q.q_on && q.q == 5000 && q.qs == 0xab);
        assert!(q.tc_on && q.tc.burst_len == 100 && q.tc.burst_q == 7 && q.tc.burst_seed == 1);
        let x = p("disk=none tjx=5:100,9:7 xtx=1000,2000 | run");
        assert_eq!(x.tj.n_explicit, 2);
        assert_eq!(x.tj.explicit_[1], hvx_tj_explicit { tick: 9, delay: 7 });
        assert_eq!(x.xt.n_explicit, 2);
        // what the C refused, hydo-schedule refuses
        for bad in [
            "disk=bogus | run",
            "no bar",
            "disk=none cf=3 | run",
            "disk=none qm=4 | run",
            "disk=none qbs=3 | run",
            "disk=none tjx=5-100 | run",
        ] {
            assert!(Schedule::parse(bad).is_err(), "{bad}");
        }
    }

    fn refused(s: &str) -> Option<String> {
        refusal(&Schedule::parse(s).unwrap())
    }

    #[test]
    fn refusals_follow_the_module() {
        // the Qdrant exploration's combination (M208): tjmax=500000 is above pp=65536
        let r = refused("disk=none tj=5 tjpm=10 tjmax=500000 pp=65536 | run").expect("refused");
        assert!(r.contains("tjmax=500000") && r.contains("pp=65536"), "{r}");
        assert!(refused("disk=none tj=5 tjpm=10 tjmax=32768 pp=65536 | run").is_none());
        assert!(refused("disk=none tj=5 tjpm=10 tjmax=500000 | run").is_none());
        assert!(refused("disk=none tj=5 tjmax=1048576 | run").is_some());
        // jitter off (permille 0) arms nothing
        assert!(refused("disk=none tj=5 tjpm=0 tjmax=500000 pp=65536 | run").is_none());
        assert!(refused("disk=none tjx=5:70000 pp=65536 | run").is_some());
        assert!(refused("disk=none tjx=5:7000 pp=65536 | run").is_none());
        assert!(refused("disk=none pp=512 | run").is_some());
        assert!(refused("disk=none pp=513 | run").is_none());
        assert!(refused("disk=none xt=3 xtr=65 | run").is_some());
        assert!(refused("disk=none xt=3 xtr=64 pp=16384 tj=1 tjmax=8192 | run").is_none());
    }
}
