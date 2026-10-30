//! Schedule generation, printing, parsing and shrink candidates: the port of explore.py's
//! generators (random, swarm M42, swarm-pos and process M56), the option streams (each from its
//! own splitmix64 stream, so turning one on leaves every other field of a seed's schedule as it
//! was), the sweeps, `render`/`parse`, the finding classes and the shrinker's candidates.
//!
//! [`Ex`] holds what explore.py keeps in module globals: the fault space (the stack's names, or a
//! manifest's after [`crate::space::Space::apply`]), the options and the engine's capabilities.

use crate::compose;
use crate::py::{fdiv, isdigit, py_hex, py_int};
use crate::rng::{mix64, SplitMix, GOLDEN};
use crate::sched::{netem_args, render_fc, render_h, render_preempt, render_tj, render_turns};
use crate::sched::{Fc, Kinds, Pause, Tj, H, KV, RV, S};
use crate::space::{PfTarget, Space};
use regex::Regex;
use std::sync::OnceLock;

pub const NORDERS: i64 = 30;
pub const TJ_MAX: i64 = 500000;
pub const TJ_EXPLICIT_MAX: usize = 32;
pub const BRANCH_BUDGET: i64 = 16;
pub const NF_SALT: u64 = 0x4E46A17C0FFEE046;
pub const BG_SALT: u64 = 0xB0661F7ED5EED0B6;
pub const CC_SALT: u64 = 0x51C0C0C0C0C0C051;
pub const MF_SALT: u64 = 0x3F7A11C0FFEE3F7A;
pub const X_OFF_MAX: i64 = 250;
pub const TURNS_SALT: u64 = 0x7A4E5C0FFEE7A4E5;
pub const BURSTS_SALT: u64 = 0xB0B5EED5C0FFEE77;
pub const Q_DEFAULT: i64 = 1 << 20;
pub const CLOCK_DELTAS: [i64; 10] = [
    -30 * 86400,
    -86400,
    -3600,
    -300,
    -30,
    30,
    300,
    3600,
    86400,
    365 * 86400,
];
pub const FC_SALT: u64 = 0xF2EE2EC0FFEEF2EE;
pub const FC_P: i64 = 50;
pub const FC_NMAX: i64 = 64;
pub const FC_DURS: [i64; 9] = [100, 300, 1000, 3000, 10000, 30000, 60000, 90000, 150000];
pub const FC_MATCH_NMAX: i64 = 32;
pub const PF_SALT: u64 = 0x4854505F46414B45;
pub const PF_P: [i64; 2] = [60, 25];
pub const PF_DELAYS: [i64; 4] = [300, 1000, 3000, 10000];
pub const PF_LATES: [i64; 3] = [1000, 5000, 20000];
pub const PF_LENS: [i64; 5] = [1, 1, 2, 3, 0];
pub const CW_SALT: u64 = 0xC0390538C0FFEE57;
pub const SWARM_SALT: u64 = 0x5A4A5A4A5A4A5A4A;
pub const POS_SALT: u64 = 0x9051710E9051710E;
pub const PROC_SALT: u64 = 0x7E5751A47E5751A4;
pub const TIMING_SALT: u64 = 0x7417D1CE7417D1CE;
pub const PREEMPT_SALT: u64 = 0x9EE397C0FFEE9EE3;
pub const GF_SALT: u64 = 0x475250435F464C54;
pub const HVM_TJ_MAX: i64 = 1000;

const CATEGORIES: [&str; 5] = ["kill", "net", "cut", "disk", "knob"];
const POSITIONS: [&str; 4] = ["early", "middle", "late", "end"];
const POS_CATEGORIES: [&str; 8] = [
    "kill", "pause", "net", "oneway", "clock", "cut", "disk", "knob",
];
const NETEM_KINDS: [&str; 5] = ["delay", "loss", "duplicate", "reorder", "corrupt"];
const GF_P: [i64; 2] = [60, 20];
const GF_KINDS: [&str; 9] = [
    "drop", "drop", "status", "acted", "delay", "reset", "conn", "cut", "hold",
];
const GF_STATUS: [&str; 5] = [
    "UNAVAILABLE",
    "DEADLINE_EXCEEDED",
    "RESOURCE_EXHAUSTED",
    "INTERNAL",
    "ABORTED",
];
const GF_DELAYS: [i64; 3] = [500, 3000, 30000];
const GF_NMSG: [i64; 5] = [0, 1, 1, 2, 5];
const GF_LENS: [i64; 5] = [1, 1, 2, 3, 0];
const GF_SEL: [&str; 4] = ["all", "all", "call", "share"];
const GRPC_KEYS: [&str; 6] = ["rpc", "call", "seed", "cut", "hold", "dir"];

/// The engine the schedules are for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Hvx,
    Hvm,
}

/// What explore.py keeps in module globals.
#[derive(Clone, Debug)]
pub struct Ex {
    pub norders: i64,
    pub svcs: Vec<String>,
    pub net_svcs: Vec<String>,
    pub pause_svcs: Vec<String>,
    pub knobs: Vec<(String, KV)>,
    pub knob_tokens: Vec<String>,
    pub space: Option<Space>,
    pub knob_rule_any: bool,
    pub timing: bool,
    pub tj_pm: Vec<i64>,
    pub new_faults: bool,
    pub preempt: bool,
    pub pct_points: Vec<i64>,
    pub pct_exits: Vec<i64>,
    pub pp_values: Vec<i64>,
    pub xt_rates: Vec<i64>,
    pub buggify: bool,
    pub concurrency: bool,
    pub more_faults: bool,
    pub cf_range: Option<(i64, i64)>,
    pub backend: Backend,
    /// hvm: the image has a data disk / tc (hvm_backend.configure).
    pub hvm_disk: bool,
    pub hvm_tc: bool,
    pub turns: bool,
    pub bursts: bool,
    pub freeze_call: bool,
    pub fc_nmax: i64,
    pub fc_pairs: Vec<(String, String)>,
    pub fc_match: Vec<(String, Option<char>)>,
    pub protocol_faults: bool,
    pub pf_targets: Vec<PfTarget>,
    pub grpc_on: bool,
    pub grpc_targets: Vec<(String, Vec<String>)>,
    pub compose: bool,
}

impl Default for Ex {
    fn default() -> Ex {
        let svcs: Vec<String> = ["mysql", "kafka", "redis"].map(String::from).to_vec();
        let mut pause = svcs.clone();
        pause.push("front".into());
        Ex {
            norders: NORDERS,
            net_svcs: svcs.clone(),
            svcs,
            pause_svcs: pause,
            knobs: vec![
                ("M".into(), KV::Int(2)),
                ("M".into(), KV::Int(0)),
                ("R".into(), KV::Str("everysec".into())),
                ("R".into(), KV::Str("no".into())),
            ],
            knob_tokens: vec!["M".into(), "R".into()],
            space: None,
            knob_rule_any: false,
            timing: false,
            tj_pm: vec![50, 100, 200],
            new_faults: false,
            preempt: false,
            pct_points: vec![10, 40],
            pct_exits: vec![3000, 10000],
            pp_values: vec![1 << 16, 1 << 14],
            xt_rates: vec![4, 16],
            buggify: false,
            concurrency: false,
            more_faults: false,
            cf_range: None,
            backend: Backend::Hvx,
            hvm_disk: false,
            hvm_tc: false,
            turns: false,
            bursts: false,
            freeze_call: false,
            fc_nmax: FC_NMAX,
            fc_pairs: Vec::new(),
            fc_match: Vec::new(),
            protocol_faults: false,
            pf_targets: Vec::new(),
            grpc_on: false,
            grpc_targets: Vec::new(),
            compose: true,
        }
    }
}

fn kinds_draw(r: &mut SplitMix) -> Kinds {
    let mut kinds: Kinds = Vec::new();
    for _ in 0..r.rng(1, 2) {
        let k = r.pick(&NETEM_KINDS);
        if !kinds.iter().any(|(x, _)| x == k) {
            // explore.py builds the whole {kind: value} dict before indexing it: five draws
            let vals = [
                r.rng(10, 300),
                r.rng(5, 60),
                r.rng(5, 50),
                r.rng(10, 50),
                r.rng(1, 10),
            ];
            let i = NETEM_KINDS.iter().position(|x| *x == k).unwrap_or(0);
            kinds.push((k.to_string(), vals[i]));
        }
    }
    kinds
}

fn one(k: &str, v: i64) -> Kinds {
    vec![(k.to_string(), v)]
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("a valid pattern"))
}

impl Ex {
    pub fn space_composed(&self) -> bool {
        self.space.as_ref().is_some_and(Space::composed)
    }

    // ------------------------------------------------------------ generators
    pub fn generate(&self, seed: u64) -> S {
        let n_ = self.norders;
        let mut r = SplitMix::new(seed);
        let mut s = S::new("none", r.next());
        if r.below(100) < 40 {
            let at = r.rng(0, n_ - 4);
            let svc = r.pick(&self.svcs);
            s.k = Some((svc, at, r.rng(1, 4.min(n_ - at))));
        }
        if r.below(100) < 40 {
            let a = r.rng(0, n_ - 2);
            let b = r.rng(a + 1, n_.min(a + 15));
            let kinds = kinds_draw(&mut r);
            s.n = Some((r.pick(&self.net_svcs), a, b, kinds));
        }
        let x = r.below(100);
        s.disk = if x < 45 {
            "none".into()
        } else if x < 60 {
            format!("ioerr:{}", r.rng(1, 3))
        } else if x < 85 {
            "cut".into()
        } else {
            format!("lying:{}", r.rng(2, 12))
        };
        if s.disk == "cut" || s.disk_is("lying") {
            s.c = Some(r.rng(1, n_));
        }
        if r.below(100) < 20 {
            let (t, v) = r.pick(&self.knobs);
            s.set_knob(&t, v);
        }
        s
    }

    pub fn swarm_categories(seed: u64) -> (Vec<&'static str>, SplitMix) {
        let mut r = SplitMix::new(seed ^ SWARM_SALT);
        let mut on: Vec<&'static str> = CATEGORIES.iter().copied().filter(|_| r.below(2) != 0).collect();
        if on.is_empty() {
            on = vec![r.pick(&CATEGORIES)];
        }
        (on, r)
    }

    pub fn generate_swarm(&self, seed: u64) -> S {
        let n_ = self.norders;
        let (on, mut r) = Ex::swarm_categories(seed);
        let mut s = S::new("none", r.next());
        if on.contains(&"kill") {
            let at = r.rng(0, n_ - 4);
            let svc = r.pick(&self.svcs);
            s.k = Some((svc, at, r.rng(1, 4.min(n_ - at))));
        }
        if on.contains(&"net") {
            let a = r.rng(0, n_ - 2);
            let b = r.rng(a + 1, n_.min(a + 15));
            let kinds = kinds_draw(&mut r);
            s.n = Some((r.pick(&self.net_svcs), a, b, kinds));
        }
        if on.contains(&"disk") {
            let opts = [format!("ioerr:{}", r.rng(1, 3)), format!("lying:{}", r.rng(2, 12))];
            s.disk = r.pick(&opts);
            if s.disk_is("lying") {
                s.c = Some(r.rng(1, n_));
            }
        }
        if on.contains(&"cut") && s.disk == "none" {
            s.disk = "cut".into();
            s.c = Some(r.rng(1, n_));
        }
        if on.contains(&"knob") {
            let (t, v) = r.pick(&self.knobs);
            s.set_knob(&t, v);
        }
        s
    }

    fn pos_start(&self, r: &mut SplitMix, pos: &str) -> i64 {
        let n_ = self.norders;
        let (lo, hi) = match pos {
            "early" => (0, fdiv(n_, 3) - 1),
            "middle" => (fdiv(n_, 3), fdiv(2 * n_, 3) - 1),
            "late" => (fdiv(2 * n_, 3), n_ - 5),
            _ => (n_ - 4, n_ - 1),
        };
        r.rng(lo, hi)
    }

    fn pos_window(&self, r: &mut SplitMix, pos: &str, maxlen: i64) -> (i64, i64) {
        let a = self.pos_start(r, pos);
        let b = if pos == "end" {
            self.norders
        } else {
            r.rng(a + 1, self.norders.min(a + maxlen))
        };
        (a, b)
    }

    pub fn generate_swarm_pos(&self, seed: u64) -> S {
        let n_ = self.norders;
        let mut r = SplitMix::new(seed ^ POS_SALT);
        let mut on: Vec<&str> = POS_CATEGORIES.iter().copied().filter(|_| r.below(2) != 0).collect();
        if on.is_empty() {
            on = vec![r.pick(&POS_CATEGORIES)];
        }
        let pos: Vec<&str> = POS_CATEGORIES.iter().map(|_| r.pick(&POSITIONS)).collect();
        let p = |c: &str| pos[POS_CATEGORIES.iter().position(|x| *x == c).unwrap_or(0)];
        let mut s = S::new("none", r.next());
        if on.contains(&"kill") {
            let at = self.pos_start(&mut r, p("kill"));
            let svc = r.pick(&self.svcs);
            let down = if p("kill") == "end" {
                n_ - at
            } else {
                r.rng(1, 4.min(n_ - at))
            };
            s.k = Some((svc, at, down));
        }
        if on.contains(&"pause") {
            let at = self.pos_start(&mut r, p("pause"));
            if p("pause") == "end" || r.below(2) != 0 {
                let svc = r.pick(&self.pause_svcs);
                let n = if p("pause") == "end" { n_ - at } else { r.rng(1, 3) };
                s.p = Some(Pause::Plus(svc, at, n));
            } else {
                let svc = r.pick(&self.pause_svcs);
                let off = r.rng(0, 500);
                s.p = Some(Pause::Timed(svc, at, off, r.rng(500, 30000)));
            }
        }
        if on.contains(&"net") {
            let (a, b) = self.pos_window(&mut r, p("net"), 15);
            let kinds = kinds_draw(&mut r);
            s.n = Some((r.pick(&self.net_svcs), a, b, kinds));
        }
        if on.contains(&"oneway") && s.n.is_none() {
            let (a, b) = self.pos_window(&mut r, p("oneway"), 10);
            let kinds = if r.below(2) != 0 {
                one("loss", r.rng(30, 100))
            } else {
                one("delay", r.rng(100, 3000))
            };
            let svc = r.pick(&self.net_svcs);
            let d = r.pick(&["req", "resp"]).to_string();
            s.o = Some((svc, a, b, d, kinds));
        }
        if on.contains(&"clock") {
            let at = self.pos_start(&mut r, p("clock"));
            s.t = Some((at, r.pick(&CLOCK_DELTAS)));
        }
        if on.contains(&"disk") {
            let opts = [format!("ioerr:{}", r.rng(1, 3)), format!("lying:{}", r.rng(2, 12))];
            s.disk = r.pick(&opts);
            if s.disk_is("lying") {
                s.c = Some(self.pos_start(&mut r, p("disk")) + 1);
            }
        }
        if on.contains(&"cut") && s.disk == "none" {
            s.disk = "cut".into();
            s.c = Some(self.pos_start(&mut r, p("cut")) + 1);
        }
        if on.contains(&"knob") {
            let (t, v) = r.pick(&self.knobs);
            s.set_knob(&t, v);
        }
        s
    }

    pub fn generate_process(&self, seed: u64) -> S {
        let n_ = self.norders;
        let mut r = SplitMix::new(seed ^ PROC_SALT);
        let n = r.rng(2, 4);
        let mut order: Vec<&str> = Vec::new();
        while (order.len() as i64) < n {
            let k = r.pick(&["K", "P", "net", "T"]);
            if !order.contains(&k) {
                order.push(k);
            }
        }
        let mut s = S::new("none", r.next());
        let w = fdiv(n_, n);
        for (i, k) in order.iter().enumerate() {
            let i = i as i64;
            let (lo, hi) = (i * w, if i == n - 1 { n_ } else { (i + 1) * w });
            let a = r.rng(lo, lo.max(hi - 2));
            let b = if i == n - 1 && r.below(2) != 0 {
                n_
            } else {
                r.rng(a + 1, hi)
            };
            match *k {
                "K" => s.k = Some((r.pick(&self.svcs), a, b - a)),
                "P" => s.p = Some(Pause::Plus(r.pick(&self.pause_svcs), a, b - a)),
                "T" => s.t = Some((a, r.pick(&CLOCK_DELTAS))),
                _ => {
                    if r.below(2) != 0 {
                        let svc = r.pick(&self.net_svcs);
                        let kinds = if r.below(2) != 0 {
                            one("loss", r.rng(20, 100))
                        } else {
                            one("delay", r.rng(100, 1000))
                        };
                        s.n = Some((svc, a, b, kinds));
                    } else {
                        let svc = r.pick(&self.net_svcs);
                        let d = r.pick(&["req", "resp"]).to_string();
                        let kinds = if r.below(2) != 0 {
                            one("loss", r.rng(30, 100))
                        } else {
                            one("delay", r.rng(100, 3000))
                        };
                        s.o = Some((svc, a, b, d, kinds));
                    }
                }
            }
        }
        if r.below(5) == 0 {
            s.disk = "cut".into();
            s.c = Some(n_);
        }
        s
    }

    pub fn generator(&self, strategy: &str, seed: u64) -> S {
        match strategy {
            "swarm" => self.generate_swarm(seed),
            "swarm-pos" => self.generate_swarm_pos(seed),
            "process" => self.generate_process(seed),
            _ => self.generate(seed),
        }
    }

    // ------------------------------------------------------------ option streams
    pub fn add_timing(&self, mut s: S, seed: u64) -> S {
        if self.timing {
            let mut t = SplitMix::new(seed ^ TIMING_SALT);
            if t.below(100) < 50 {
                let sd = t.next();
                s.tj = Some(Tj::Seed {
                    seed: sd,
                    pm: t.pick(&self.tj_pm),
                    max: TJ_MAX,
                    lo: 0,
                    hi: 0,
                });
            }
        }
        s
    }

    pub fn add_preempt(&self, mut s: S, seed: u64) -> S {
        if self.preempt {
            let mut t = SplitMix::new(seed ^ PREEMPT_SALT);
            if t.below(100) < 50 {
                if self.backend == Backend::Hvm {
                    let a = t.next();
                    let b = t.pick(&self.pct_points);
                    s.pct = Some((a, b, t.pick(&self.pct_exits)));
                } else if t.below(2) != 0 {
                    s.pp = Some(t.pick(&self.pp_values));
                } else {
                    let a = t.next();
                    s.xt = Some((a, t.pick(&self.xt_rates), 500));
                }
            }
        }
        s
    }

    pub fn add_turns(&self, mut s: S, seed: u64) -> S {
        if self.turns {
            let mut t = SplitMix::new(seed ^ TURNS_SALT);
            let on = t.below(100) < 50;
            let x = t.below(100);
            let seeded = t.below(2) != 0;
            let qs = match t.next() {
                0 => 1,
                v => v,
            };
            let q = if x < 10 {
                t.rng(1, 100)
            } else if x < 20 {
                t.rng(101, 10000)
            } else {
                t.rng(Q_DEFAULT / 2, 2 * Q_DEFAULT)
            };
            if on {
                s.q = Some((q, if seeded { qs } else { 0 }));
            }
        }
        if self.bursts {
            let mut t = SplitMix::new(seed ^ BURSTS_SALT);
            let on = t.below(100) < 50;
            let blen = t.rng(2000, 200000);
            let bq = t.rng(20, 1000);
            let pm = t.pick(&[50, 100, 250, 500, 1000]);
            let bs = match t.next() {
                0 => 1,
                v => v,
            };
            if on {
                s.qb = Some((blen, bq, pm, bs));
            }
        }
        s
    }

    pub fn add_new_faults(&self, mut s: S, seed: u64, swarm: bool) -> S {
        if !self.new_faults {
            return s;
        }
        let n_ = self.norders;
        let mut t = SplitMix::new(seed ^ NF_SALT);
        let p_on = if swarm { 50 } else { 35 };
        if t.below(100) < p_on {
            let svc = t.pick(&self.pause_svcs);
            let at = t.rng(0, n_ - 2);
            if t.below(2) != 0 {
                s.p = Some(Pause::Plus(svc, at, t.rng(1, 3)));
            } else {
                let off = t.rng(0, 500);
                s.p = Some(Pause::Timed(svc, at, off, t.rng(500, 30000)));
            }
        }
        if t.below(100) < p_on {
            let at = t.rng(0, n_ - 1);
            s.t = Some((at, t.pick(&CLOCK_DELTAS)));
        }
        if t.below(100) < p_on {
            let svc = t.pick(&self.net_svcs);
            let a = t.rng(0, n_ - 2);
            let b = t.rng(a + 1, n_.min(a + 10));
            let d = t.pick(&["req", "resp"]).to_string();
            let kinds = if t.below(2) != 0 {
                one("loss", t.rng(30, 100))
            } else {
                one("delay", t.rng(100, 3000))
            };
            if s.n.is_none() {
                s.o = Some((svc, a, b, d, kinds));
            }
        }
        s
    }

    /// explore.py `fc_n`: log-uniform over 1 .. nmax.
    pub fn fc_n(&self, t: &mut SplitMix, nmax: Option<i64>) -> i64 {
        let nmax = nmax.filter(|n| *n != 0).unwrap_or(self.fc_nmax);
        fc_value(t.below(1000), nmax)
    }

    pub fn add_freeze_call(&self, mut s: S, seed: u64) -> S {
        if !self.freeze_call || self.fc_pairs.is_empty() {
            return s;
        }
        let mut t = SplitMix::new(seed ^ FC_SALT);
        let on = t.below(100) < FC_P;
        let pair = t.pick(&self.fc_pairs);
        let d = t.pick(&['>', '<']);
        let step = t.rng(0, self.norders - 1);
        let dur = t.pick(&FC_DURS);
        let want_match = t.below(2) != 0;
        let mi = t.below((self.fc_match.len() as i64).max(1));
        let n_all = self.fc_n(&mut t, None);
        let n_match = self.fc_n(&mut t, Some(FC_MATCH_NMAX));
        let use_match = !self.fc_match.is_empty() && want_match;
        if on {
            s.f = Some(if use_match {
                let (m, md) = self.fc_match[mi as usize].clone();
                Fc {
                    svc: pair.0,
                    dir: md.unwrap_or(d),
                    peer: pair.1,
                    step,
                    n: n_match,
                    matched: Some(m),
                    dur,
                }
            } else {
                Fc {
                    svc: pair.0,
                    dir: d,
                    peer: pair.1,
                    step,
                    n: n_all,
                    matched: None,
                    dur,
                }
            });
        }
        s
    }

    pub fn add_protocol_faults(&self, mut s: S, seed: u64) -> S {
        if !self.protocol_faults || self.pf_targets.is_empty() {
            return s;
        }
        let n_ = self.norders;
        let mut t = SplitMix::new(seed ^ PF_SALT);
        let mut hs = Vec::new();
        for pk in PF_P {
            let on = t.below(100) < pk;
            let tg = t.pick(&self.pf_targets);
            let kix = t.below(1 << 30);
            let a = t.below(n_);
            let ln = t.pick(&PF_LENS);
            let st = t.below(1 << 30);
            let ra = t.pick(&[None, Some(1), Some(2)]);
            let dl = t.pick(&PF_DELAYS);
            let late = t.pick(&PF_LATES);
            let p = t.pick(&[100, 100, 50, 25]);
            let eff = t.below(100) < 30;
            if !on {
                continue;
            }
            let mut kinds = tg.req.clone();
            kinds.extend(tg.hook.iter().cloned());
            let kind = kinds[(kix as usize) % kinds.len()].clone();
            let b = if ln == 0 { n_ } else { n_.min(a + ln) };
            let mut rules: Vec<(String, RV)> = match kind.as_str() {
                "status" => {
                    let codes: [i64; 4] = if tg.proto == "grpc" {
                        [14, 4, 8, 13]
                    } else {
                        [500, 502, 503, 429]
                    };
                    let code = codes[(st as usize) % codes.len()];
                    let mut r = vec![("status".to_string(), RV::Int(code))];
                    if let Some(ra) = ra.filter(|_| code == 429 || code == 503) {
                        r.push(("retry-after".into(), RV::Int(ra)));
                    }
                    if eff {
                        r.push(("effect".into(), RV::Str("yes".into())));
                    }
                    r
                }
                "delay" => vec![("delay".into(), RV::Int(dl))],
                "late" => vec![("late".into(), RV::Int(late))],
                k => vec![(k.to_string(), RV::None)],
            };
            if p < 100 {
                rules.push(("p".into(), RV::Int(p)));
            }
            hs.push(H {
                target: tg.name.clone(),
                a,
                b,
                rules,
            });
        }
        if !hs.is_empty() {
            s.h = hs;
        }
        s
    }

    pub fn add_grpc_faults(&self, mut s: S, seed: u64) -> S {
        if !self.grpc_on || self.grpc_targets.is_empty() {
            return s;
        }
        let n_ = self.norders;
        let mut t = SplitMix::new(seed ^ GF_SALT);
        let mut hs = Vec::new();
        for gp in GF_P {
            let on = t.below(100) < gp;
            let tg = t.pick(&self.grpc_targets);
            let kind = t.pick(&GF_KINDS);
            let a = t.below(n_);
            let ln = t.pick(&GF_LENS);
            let st = t.pick(&GF_STATUS);
            let dl = t.pick(&GF_DELAYS);
            let nm = t.pick(&GF_NMSG);
            let dreq = t.below(100) < 30;
            let sel = t.pick(&GF_SEL);
            let coff = t.below(3);
            let p = t.pick(&[50, 25]);
            let sd = t.below(1000);
            let rix = t.below(1 << 30);
            if !on {
                continue;
            }
            let b = if ln == 0 { n_ } else { n_.min(a + ln) };
            let rs = |k: &str, v: RV| (k.to_string(), v);
            let dir = || {
                if dreq {
                    vec![rs("dir", RV::Str("req".into()))]
                } else {
                    vec![]
                }
            };
            let mut rules: Vec<(String, RV)> = match kind {
                "drop" => vec![rs("drop", RV::None)],
                "status" => vec![rs("status", RV::Str(st.into()))],
                "acted" => vec![
                    rs("status", RV::Str(st.into())),
                    rs("effect", RV::Str("yes".into())),
                ],
                "delay" => vec![rs("delay", RV::Int(dl))],
                "reset" => vec![rs("reset", RV::None)],
                "conn" => vec![rs("reset", RV::Str("conn".into()))],
                "cut" => [vec![rs("cut", RV::Int(nm))], dir()].concat(),
                _ => [vec![rs("hold", RV::Int(nm))], dir()].concat(),
            };
            let (name, paths) = tg;
            if !paths.is_empty() {
                let path = &paths[(rix as usize) % paths.len()];
                let m = path.rsplit('/').next().unwrap_or(path);
                rules.push(rs("rpc", RV::Str(m.to_string())));
            }
            if sel == "call" {
                rules.push(rs("call", RV::Int(a + 1 + coff)));
            } else if sel == "share" {
                rules.push(rs("p", RV::Int(p)));
                rules.push(rs("seed", RV::Int(sd)));
            }
            hs.push(H {
                target: name,
                a,
                b,
                rules,
            });
        }
        if !hs.is_empty() {
            s.h.extend(hs);
        }
        s
    }

    pub fn add_compose(&self, mut s: S, seed: u64) -> S {
        if self.compose && self.space_composed() {
            let meta = self
                .space
                .as_ref()
                .and_then(|sp| sp.compose.as_ref())
                .expect("a composed plan");
            let mut r = SplitMix::new(seed ^ CW_SALT);
            s.w = Some(compose::draw(&mut || r.next(), meta));
        }
        s
    }

    pub fn add_buggify(&self, mut s: S, seed: u64) -> S {
        if self.buggify {
            s.b = Some(format!("{:x}", SplitMix::new(seed ^ BG_SALT).next()));
        }
        s
    }

    pub fn add_concurrency(&self, mut s: S, seed: u64) -> S {
        if self.concurrency {
            let mut t = SplitMix::new(seed ^ CC_SALT);
            if t.below(100) < 60 {
                let n = t.pick(&[2, 4, 8]);
                s.cq = Some((n, t.below(100) < 30));
            }
        }
        s
    }

    pub fn add_more_faults(&self, mut s: S, seed: u64, strategy: &str) -> S {
        if !self.more_faults {
            return s;
        }
        let mut t = SplitMix::new(seed ^ MF_SALT);
        let px = match strategy {
            "random" => 30,
            "swarm" | "swarm-pos" => 50,
            "process" => 40,
            _ => 30,
        };
        let pcf = match strategy {
            "random" => 40,
            "swarm" | "swarm-pos" => 50,
            "process" => 60,
            _ => 40,
        };
        let x_on = t.below(100) < px;
        let x_svc = t.next();
        let x_at = t.rng(0, self.norders - 1);
        let x_off = t.rng(0, X_OFF_MAX);
        let c_on = t.below(100) < pcf;
        let c_k = t.next();
        let c_ok = c_on
            && self.cf_range.is_some()
            && s.k.is_none()
            && (s.disk == "none" || s.disk == "cut")
            && self.space.as_ref().is_none_or(|sp| sp.has_fault("cut"));
        if x_on && self.space.is_none() {
            let svcs: Vec<&String> = self
                .pause_svcs
                .iter()
                .filter(|v| s.k.as_ref().is_none_or(|k| **v != k.0) && (!c_ok || *v == "front"))
                .collect();
            if !svcs.is_empty() {
                s.x = Some((
                    svcs[(x_svc % svcs.len() as u64) as usize].clone(),
                    x_at,
                    x_off,
                ));
            }
        }
        if c_ok {
            let (lo, hi) = self.cf_range.expect("checked");
            s.disk = "cut".into();
            s.c = None;
            s.cf = Some(lo + crate::py::pmod(c_k as i128, (hi - lo + 1) as i128) as i64);
        }
        s
    }

    /// The composition explore.py main() applies to a generated schedule.
    pub fn compose_seed(&self, strategy: &str, seed: u64, durable_only: bool) -> S {
        let mut s = self.generator(strategy, seed);
        if durable_only {
            s = self.durable_only(s);
        }
        if strategy != "swarm-pos" && strategy != "process" {
            s = self.add_new_faults(s, seed, strategy == "swarm");
        }
        s = self.add_more_faults(s, seed, strategy);
        s = self.add_freeze_call(s, seed);
        s = self.add_protocol_faults(s, seed);
        s = self.add_grpc_faults(s, seed);
        self.add_point_streams(s, seed)
    }

    /// The streams every sweep point and generated schedule gets (explore.py main()).
    pub fn add_point_streams(&self, s: S, seed: u64) -> S {
        let s = self.add_timing(s, seed);
        let s = self.add_concurrency(s, seed);
        let s = self.add_preempt(s, seed);
        let s = self.add_buggify(s, seed);
        let s = self.add_turns(s, seed);
        self.add_compose(s, seed)
    }

    pub fn durable_only(&self, mut s: S) -> S {
        if s.disk_is("lying") {
            s.disk = "cut".into();
        } else if s.disk_is("ioerr") {
            s.disk = "none".into();
        }
        for k in self.knob_tokens.clone() {
            s.pop_knob(&k);
        }
        s
    }

    // ------------------------------------------------------------ engines
    /// hvm_backend.hvm_supported: the part of a schedule the Mac engine can run (`render` has
    /// dropped `cf=` before, which hvmx cannot apply either).
    pub fn hvm_supported(&self, s: &S) -> S {
        let mut s = s.clone();
        if !self.hvm_disk {
            s.disk = "none".into();
            s.c = None;
        }
        if !self.hvm_tc {
            s.n = None;
            s.o = None;
        }
        s.rb = None;
        s.pp = None;
        s.xt = None;
        s.q = None;
        s.qb = None;
        if let Some(Tj::Seed { max, .. }) = &mut s.tj {
            if *max > HVM_TJ_MAX {
                *max = HVM_TJ_MAX;
            }
        }
        s
    }

    // ------------------------------------------------------------ printing and parsing
    pub fn render(&self, s: &S) -> String {
        let mut s = match &self.space {
            Some(sp) => sp.restrict(s),
            None => s.clone(),
        };
        if self.backend == Backend::Hvm {
            // hvmx cannot apply cf= (a power cut at the k-th FLUSH): a schedule for it never
            // carries one (explore.py kept it, so Mac reports claimed cuts that never happened)
            s.cf = None;
        }
        if s.cf.is_some() && (s.disk != "cut" || s.c.is_some()) {
            if s.disk != "cut" {
                s.cf = None;
            } else {
                s.c = None;
            }
        }
        if self.backend == Backend::Hvm {
            s = self.hvm_supported(&s);
        }
        let mut g = vec!["run".to_string()];
        if let Some((svc, at, down)) = &s.k {
            g.push(format!("K={svc}@{at}+{down}"));
        }
        if let Some((svc, a, b, kinds)) = &s.n {
            g.push(format!("N={svc}@{a}-{b}:{}", netem_args(kinds)));
        }
        if let Some(c) = s.c {
            g.push(format!("C={c}"));
        }
        if let Some(v) = s.knob("M") {
            g.push(format!("M={v}"));
        }
        if let Some(v) = s.knob("R") {
            g.push(format!("R={v}"));
        }
        for k in &self.knob_tokens {
            if k != "M" && k != "R" {
                if let Some(v) = s.knob(k) {
                    g.push(format!("{k}={v}"));
                }
            }
        }
        match &s.p {
            Some(Pause::Plus(svc, at, n)) => g.push(format!("P={svc}@{at}+{n}")),
            Some(Pause::Timed(svc, at, off, dur)) => g.push(format!("P={svc}@{at}~{off}:{dur}")),
            None => {}
        }
        if let Some((at, d)) = s.t {
            g.push(format!("T={at}:{d}"));
        }
        if let Some((svc, a, b, d, kinds)) = &s.o {
            g.push(format!("O={svc}@{a}-{b}:{d}:{}", netem_args(kinds)));
        }
        if let Some((n, dup)) = s.cq {
            g.push(format!("Q={n}{}", if dup { "d" } else { "" }));
        }
        if let Some((svc, at, off)) = &s.x {
            g.push(format!("X={svc}@{at}~{off}"));
        }
        if let Some(b) = &s.b {
            g.push(format!("B={b}"));
        }
        if let Some(f) = &s.f {
            g.push(render_fc(f));
        }
        for h in &s.h {
            g.push(render_h(h));
        }
        if let Some(w) = &s.w {
            g.push(format!("W={w}"));
        }
        format!(
            "disk={} dseed={:x}{}{}{} | {}",
            s.disk,
            s.dseed,
            s.tj.as_ref().map(render_tj).unwrap_or_default(),
            render_preempt(&s)
                + &s.cf.map(|c| format!(" cf={c}")).unwrap_or_default()
                + &render_turns(&s),
            s.rb.map(|r| format!(" rb={r}")).unwrap_or_default(),
            g.join(" ")
        )
    }

    /// explore.py `parse(text)`. Err: the text explore.py would stop on with an exception.
    pub fn parse(&self, text: &str) -> Result<S, String> {
        static R: [OnceLock<Regex>; 16] = [const { OnceLock::new() }; 16];
        let (host, guest) = text
            .split_once('|')
            .ok_or_else(|| format!("a schedule needs a '|': {text}"))?;
        let disk = re(&R[0], r"disk=(\S+)")
            .captures(host)
            .ok_or("no disk=")?[1]
            .to_string();
        let dseed = re(&R[1], r"dseed=([0-9a-fA-F]+)")
            .captures(host)
            .and_then(|c| py_hex(&c[1]))
            .ok_or("no dseed=")?;
        let mut s = S::new(&disk, dseed);
        let int = |x: &str| x.parse::<i64>().map_err(|e| format!("{x}: {e}"));
        let hex = |x: &str| py_hex(x).ok_or_else(|| format!("{x}: not hex"));
        if let Some(c) = re(&R[2], r"\brb=(\d+)").captures(host) {
            s.rb = Some(int(&c[1])?);
        }
        if let Some(c) = re(&R[3], r"\bpp=(\d+)").captures(host) {
            s.pp = Some(int(&c[1])?);
        }
        if let Some(c) = re(&R[4], r"\bcf=(\d+)").captures(host) {
            s.cf = Some(int(&c[1])?);
        }
        let mq = re(&R[5], r"(?:^|\s)q=(\d+)").captures(host);
        let mqs = re(&R[6], r"(?:^|\s)qs=([0-9a-fA-F]+)").captures(host);
        if mq.is_some() || mqs.is_some() {
            s.q = Some((
                match &mq {
                    Some(c) => int(&c[1])?,
                    None => 0,
                },
                match &mqs {
                    Some(c) => hex(&c[1])?,
                    None => 0,
                },
            ));
        }
        if let Some(c) = re(&R[7], r"(?:^|\s)qb=(\d+):(\d+):(\d+)(?: qbs=([0-9a-fA-F]+))?").captures(host) {
            s.qb = Some((
                int(&c[1])?,
                int(&c[2])?,
                int(&c[3])?,
                hex(c.get(4).map(|m| m.as_str()).unwrap_or("1"))?,
            ));
        }
        if let Some(c) = re(&R[8], r"\bpct=([0-9a-fA-F]+) pctn=(\d+) pctk=(\d+)").captures(host) {
            s.pct = Some((hex(&c[1])?, int(&c[2])?, int(&c[3])?));
        }
        if let Some(c) = re(&R[9], r"\bxt=([0-9a-fA-F]+) xtr=(\d+) xtpm=(\d+)").captures(host) {
            s.xt = Some((hex(&c[1])?, int(&c[2])?, int(&c[3])?));
        }
        if let Some(c) = re(&R[10], r"tjx=(\S+)").captures(host) {
            let mut kd = Vec::new();
            for x in c[1].split(',') {
                let v: Vec<&str> = x.split(':').collect();
                if v.len() != 2 {
                    return Err(format!("bad tjx entry {x}"));
                }
                kd.push((int(v[0])?, int(v[1])?));
            }
            s.tj = Some(Tj::X(kd));
        } else if re(&R[11], r"\btj=").is_match(host) {
            let g = |k: &str, d: i64| -> Result<i64, String> {
                match Regex::new(&format!(r"{k}=(\d+)"))
                    .expect("pattern")
                    .captures(host)
                {
                    Some(c) => int(&c[1]),
                    None => Ok(d),
                }
            };
            let seed = re(&R[12], r"\btj=([0-9a-fA-F]+)")
                .captures(host)
                .ok_or("bad tj=")?;
            s.tj = Some(Tj::Seed {
                seed: hex(&seed[1])?,
                pm: g("tjpm", 10)?,
                max: g("tjmax", TJ_MAX)?,
                lo: g("tjlo", 0)?,
                hi: g("tjhi", 0)?,
            });
        }
        let pint = |x: &str| py_int(x).ok_or_else(|| format!("invalid literal for int(): {x:?}"));
        let digits_of = |x: &str| -> Result<i64, String> {
            let d: String = x.chars().filter(|c| c.is_ascii_digit()).collect();
            pint(&d)
        };
        let netem = |args: &str| -> Result<Kinds, String> {
            let toks: Vec<&str> = args.split(',').collect();
            let mut kinds = Vec::new();
            let mut i = 0;
            while i < toks.len() {
                let v = toks.get(i + 1).ok_or("netem: a kind without a value")?;
                kinds.push((toks[i].to_string(), digits_of(v)?));
                i += 2;
            }
            Ok(kinds)
        };
        for t in guest.split_whitespace().skip(1) {
            let (k, v) = t
                .split_once('=')
                .ok_or_else(|| format!("not enough values to unpack: {t}"))?;
            match k {
                "K" => {
                    let (svc, rest) = split2(v, '@')?;
                    let (at, down) = split2(rest, '+')?;
                    s.k = Some((svc.to_string(), pint(at)?, pint(down)?));
                }
                "N" => {
                    let (svc, rest) = v.split_once('@').ok_or("N=: no @")?;
                    let (rng, args) = rest.split_once(':').ok_or("N=: no :")?;
                    let (a, b) = split2(rng, '-')?;
                    s.n = Some((svc.to_string(), pint(a)?, pint(b)?, netem(args)?));
                }
                "C" => s.c = Some(pint(v)?),
                "M" => s.set_knob(
                    "M",
                    if isdigit(v) {
                        KV::Int(pint(v)?)
                    } else {
                        KV::Str(v.to_string())
                    },
                ),
                "R" => s.set_knob("R", KV::Str(v.to_string())),
                "P" => {
                    let (svc, rest) = split2(v, '@')?;
                    if rest.contains('~') {
                        let (at, r) = split2(rest, '~')?;
                        let (off, dur) = split2(r, ':')?;
                        s.p = Some(Pause::Timed(svc.into(), pint(at)?, pint(off)?, pint(dur)?));
                    } else {
                        let (at, n) = split2(rest, '+')?;
                        s.p = Some(Pause::Plus(svc.into(), pint(at)?, pint(n)?));
                    }
                }
                "T" => {
                    let (at, d) = split2(v, ':')?;
                    s.t = Some((pint(at)?, pint(d)?));
                }
                "O" => {
                    let (svc, rest) = v.split_once('@').ok_or("O=: no @")?;
                    let mut it = rest.splitn(3, ':');
                    let (rng, d, args) = match (it.next(), it.next(), it.next()) {
                        (Some(a), Some(b), Some(c)) => (a, b, c),
                        _ => return Err(format!("bad O= token {v}")),
                    };
                    let (a, b) = split2(rng, '-')?;
                    s.o = Some((svc.into(), pint(a)?, pint(b)?, d.into(), netem(args)?));
                }
                "Q" => s.cq = Some((pint(v.trim_end_matches('d'))?, v.ends_with('d'))),
                "X" => {
                    let (svc, rest) = split2(v, '@')?;
                    let (at, off) = split2(rest, '~')?;
                    s.x = Some((svc.into(), pint(at)?, pint(off)?));
                }
                "B" => s.b = Some(v.to_string()),
                "F" => s.f = Some(parse_fc(v)?),
                "H" => s.h.push(parse_h(v)?),
                "W" => s.w = Some(v.to_string()),
                k if self.knob_tokens.iter().any(|t| t == k) => {
                    s.set_knob(k, KV::Str(v.to_string()))
                }
                _ => {}
            }
        }
        Ok(s)
    }

    // ------------------------------------------------------------ classes
    pub fn finding_class(&self, s: &S) -> &'static str {
        if s.disk_is("lying") {
            return "lying-disk";
        }
        if s.disk_is("ioerr") {
            return "io-errors";
        }
        let relaxed = match &self.space {
            Some(sp) => sp.unsafe_knob(s),
            None => {
                let m = match s.knob("M") {
                    None => false,
                    Some(KV::Int(i)) => *i != 1,
                    Some(KV::Str(_)) => true,
                };
                let r = match s.knob("R") {
                    None => false,
                    Some(v) => v.to_string() != "always" || matches!(v, KV::Int(_)),
                };
                m || r
            }
        };
        if relaxed && (self.knob_rule_any || s.disk == "cut") {
            return "unsafe-knob";
        }
        "durable-defaults"
    }

    // ------------------------------------------------------------ shrinking
    /// explore.py `candidates(s, tjk)`: simpler variants of s, most aggressive first.
    pub fn candidates(&self, s: &S, tjk: Option<(i64, i64)>) -> Vec<S> {
        let mut out: Vec<S> = Vec::new();
        let mut var = |f: &dyn Fn(&mut S)| {
            let mut t = s.clone();
            f(&mut t);
            out.push(t);
        };
        if s.k.is_some() {
            var(&|t| t.k = None);
        }
        if s.n.is_some() {
            var(&|t| t.n = None);
        }
        if s.p.is_some() {
            var(&|t| t.p = None);
        }
        if s.t.is_some() {
            var(&|t| t.t = None);
        }
        if s.o.is_some() {
            var(&|t| t.o = None);
        }
        for k in &self.knob_tokens {
            if s.knob(k).is_some() {
                var(&|t| t.pop_knob(k));
            }
        }
        if s.disk != "none" {
            var(&|t| t.disk = "none".into());
        }
        if s.c.is_some() && s.disk == "none" {
            var(&|t| t.c = None);
        }
        if let Some((svc, a, b, kinds)) = &s.n {
            if kinds.len() > 1 {
                for i in 0..kinds.len() {
                    let mut k2 = kinds.clone();
                    k2.remove(i);
                    let n = (svc.clone(), *a, *b, k2);
                    var(&|t| t.n = Some(n.clone()));
                }
            }
            for (i, (k, v)) in kinds.iter().enumerate() {
                if *v > 1 {
                    let mut k2 = kinds.clone();
                    k2[i] = (k.clone(), 1.max(fdiv(*v, 2)));
                    let n = (svc.clone(), *a, *b, k2);
                    var(&|t| t.n = Some(n.clone()));
                }
            }
            if b - a > 1 {
                let n = (svc.clone(), *a, a + 1.max(fdiv(b - a, 2)), kinds.clone());
                var(&|t| t.n = Some(n.clone()));
            }
        }
        if let Some((svc, at, down)) = &s.k {
            if *down > 1 {
                let k = (svc.clone(), *at, 1);
                var(&|t| t.k = Some(k.clone()));
            }
        }
        if s.disk_is("lying") || s.disk_is("ioerr") {
            let (kind, p) = s.disk.split_once(':').unwrap_or((&s.disk, "0"));
            let p = py_int(p).unwrap_or(0);
            if p > 1 {
                let d = format!("{}:{}", kind, 1.max(fdiv(p, 2)));
                var(&|t| t.disk = d.clone());
            }
        }
        self.nf_candidates(s, &mut var);
        if let Some((svc, at, off)) = &s.x {
            var(&|t| t.x = None);
            if *off > 0 {
                let x = (svc.clone(), *at, fdiv(*off, 2));
                var(&|t| t.x = Some(x.clone()));
            }
        }
        if let Some((n, dup)) = s.cq {
            var(&|t| t.cq = None);
            if dup {
                var(&|t| t.cq = Some((n, false)));
            }
            if n > 2 {
                var(&|t| t.cq = Some((fdiv(n, 2), dup)));
            }
        }
        if let Some(cf) = s.cf {
            var(&|t| t.cf = None);
            if cf > 1 {
                var(&|t| t.cf = Some(fdiv(cf, 2)));
            }
        }
        if s.b.is_some() {
            var(&|t| t.b = None);
        }
        if let Some(f) = &s.f {
            var(&|t| t.f = None);
            if f.dur > 100 {
                let g = Fc {
                    dur: 100.max(fdiv(f.dur, 2)),
                    ..f.clone()
                };
                var(&|t| t.f = Some(g.clone()));
            }
            if f.n > 1 {
                let g = Fc {
                    n: fdiv(f.n, 2),
                    ..f.clone()
                };
                var(&|t| t.f = Some(g.clone()));
            }
        }
        if !s.h.is_empty() {
            pf_candidates(s, &mut var);
            grpc_candidates(s, &mut var);
        }
        if s.pp.is_some() {
            var(&|t| t.pp = None);
        }
        if let Some((a, r, pm)) = s.xt {
            var(&|t| t.xt = None);
            if r > 1 {
                var(&|t| t.xt = Some((a, fdiv(r, 2), pm)));
            }
        }
        if let Some((a, n, k)) = s.pct {
            var(&|t| t.pct = None);
            if n > 1 {
                var(&|t| t.pct = Some((a, fdiv(n, 2), k)));
            }
        }
        if let Some((q, qs)) = s.q {
            var(&|t| t.q = None);
            if qs != 0 {
                var(&|t| t.q = Some((q, 0)));
            }
            if q != 0 && q < Q_DEFAULT {
                var(&|t| t.q = Some((Q_DEFAULT.min(q * 8), qs)));
            }
        }
        if let Some((blen, bq, pm, bs)) = s.qb {
            var(&|t| t.qb = None);
            if pm > 1 {
                var(&|t| t.qb = Some((blen, bq, 1.max(fdiv(pm, 2)), bs)));
            }
            if blen > 2 * bq {
                var(&|t| t.qb = Some((fdiv(blen, 2), bq, pm, bs)));
            }
        }
        tj_candidates(s, tjk, &mut var);
        if let (Some(w), Some(meta)) = (
            &s.w,
            self.space.as_ref().and_then(|sp| sp.compose.as_ref()),
        ) {
            if hydo_manifest::py::truthy(meta) {
                for w2 in compose::candidates(w, meta) {
                    var(&|t| t.w = Some(w2.clone()));
                }
            }
        }
        out
    }

    fn nf_candidates(&self, s: &S, var: &mut dyn FnMut(&dyn Fn(&mut S))) {
        match &s.p {
            Some(Pause::Plus(svc, at, n)) if *n > 1 => {
                let p = Pause::Plus(svc.clone(), *at, 1);
                var(&|t| t.p = Some(p.clone()));
            }
            Some(Pause::Timed(svc, at, off, dur)) => {
                if *dur > 500 {
                    let p = Pause::Timed(svc.clone(), *at, *off, 500.max(fdiv(*dur, 2)));
                    var(&|t| t.p = Some(p.clone()));
                }
                if *off > 0 {
                    let p = Pause::Timed(svc.clone(), *at, 0, *dur);
                    var(&|t| t.p = Some(p.clone()));
                }
            }
            _ => {}
        }
        if let Some((at, d)) = s.t {
            if d.abs() > 30 {
                let half = (d as f64 / 2.0) as i64;
                var(&|t| t.t = Some((at, half)));
            }
        }
        if let Some((svc, a, b, d, kinds)) = &s.o {
            if b - a > 1 {
                let o = (svc.clone(), *a, a + 1.max(fdiv(b - a, 2)), d.clone(), kinds.clone());
                var(&|t| t.o = Some(o.clone()));
            }
            let (k, v) = &kinds[0];
            if *v > 1 {
                let o = (svc.clone(), *a, *b, d.clone(), one(k, 1.max(fdiv(*v, 2))));
                var(&|t| t.o = Some(o.clone()));
            }
        }
    }

    // ------------------------------------------------------------ sweeps
    /// explore.py `sweep(name)`: (label, schedule) for every point of a small fault space.
    pub fn sweep(&self, name: &str) -> Result<Vec<(String, S)>, String> {
        let n_ = self.norders;
        let mut out: Vec<(String, S)> = Vec::new();
        let base = || S::new("none", 0);
        let cut = || S::new("cut", 0);
        let sev: Vec<Kinds> = vec![
            one("loss", 20),
            one("loss", 40),
            one("loss", 60),
            one("delay", 100),
            one("delay", 1000),
        ];
        let ow: Vec<Kinds> = vec![one("loss", 100), one("loss", 50), one("delay", 1000)];
        let durs = |at: i64| -> Vec<i64> {
            let mut v = vec![1, n_ - at];
            v.sort();
            v.dedup();
            v
        };
        match name {
            "kill" => {
                for svc in &self.svcs {
                    for at in 0..n_ {
                        let mut s = base();
                        s.k = Some((svc.clone(), at, 1));
                        out.push((format!("{svc}@{at}"), s));
                    }
                }
            }
            "cut" => {
                for c in 1..=n_ {
                    let mut s = cut();
                    s.c = Some(c);
                    out.push((format!("C={c}"), s));
                }
            }
            "net" => {
                for svc in &self.net_svcs {
                    for kinds in &sev {
                        let mut s = base();
                        s.n = Some((svc.clone(), 0, n_, kinds.clone()));
                        out.push((format!("{svc}:{}", netem_args(kinds)), s));
                    }
                }
            }
            "killcut" => {
                for svc in &self.svcs {
                    for at in (0..n_ - 1).step_by(5) {
                        for d in [1, 2, 4, 8, 13] {
                            if at + d <= n_ {
                                let mut s = cut();
                                s.k = Some((svc.clone(), at, 1));
                                s.c = Some(at + d);
                                out.push((format!("{svc}@{at} C={}", at + d), s));
                            }
                        }
                    }
                }
            }
            "pause" => {
                for svc in &self.pause_svcs {
                    for at in (0..n_).step_by(5) {
                        let mut s = base();
                        s.p = Some(Pause::Plus(svc.clone(), at, 2));
                        out.push((format!("{svc}@{at}+2"), s));
                        let mut s = base();
                        s.p = Some(Pause::Timed(svc.clone(), at, 200, 5000));
                        out.push((format!("{svc}@{at}~200:5000"), s));
                    }
                }
            }
            "clock" => {
                for at in [0, 10, 20] {
                    for d in [-86400, -3600, -60, 60, 3600, 86400] {
                        let mut s = base();
                        s.t = Some((at, d));
                        out.push((format!("T={at}:{d}"), s));
                    }
                }
            }
            "oneway" => {
                for svc in &self.net_svcs {
                    for d in ["req", "resp"] {
                        for kinds in &ow {
                            let mut s = base();
                            s.o = Some((svc.clone(), 5, 15, d.into(), kinds.clone()));
                            out.push((format!("{svc}:{d}:{}", netem_args(kinds)), s));
                        }
                    }
                }
            }
            "frontkill" => {
                for q in [None, Some((4, false))] {
                    for at in (0..n_).step_by(3) {
                        for off in [20, 80] {
                            let mut s = base();
                            s.x = Some(("front".into(), at, off));
                            s.cq = q;
                            out.push((
                                format!("front@{at}~{off}{}", if q.is_some() { " Q=4" } else { "" }),
                                s,
                            ));
                        }
                    }
                }
            }
            "tail" => {
                let last = n_ - 3..n_;
                for svc in &self.svcs {
                    for at in last.clone() {
                        for down in durs(at) {
                            let mut s = base();
                            s.k = Some((svc.clone(), at, down));
                            out.push((format!("K={svc}@{at}+{down}"), s));
                        }
                    }
                }
                for svc in &self.pause_svcs {
                    for at in last.clone() {
                        for n in 1..=n_ - at {
                            let mut s = base();
                            s.p = Some(Pause::Plus(svc.clone(), at, n));
                            out.push((format!("P={svc}@{at}+{n}"), s));
                        }
                        for off in [0, 200] {
                            let mut s = base();
                            s.p = Some(Pause::Timed(svc.clone(), at, off, 5000));
                            out.push((format!("P={svc}@{at}~{off}:5000"), s));
                        }
                    }
                }
                for svc in &self.net_svcs {
                    for at in last.clone() {
                        for d in ["req", "resp"] {
                            for kinds in &ow {
                                let mut s = base();
                                s.o = Some((svc.clone(), at, n_, d.into(), kinds.clone()));
                                out.push((
                                    format!("O={svc}@{at}-{n_}:{d}:{}", netem_args(kinds)),
                                    s,
                                ));
                            }
                        }
                        for kinds in &sev {
                            let mut s = base();
                            s.n = Some((svc.clone(), at, n_, kinds.clone()));
                            out.push((format!("N={svc}@{at}-{n_}:{}", netem_args(kinds)), s));
                        }
                    }
                }
                for c in n_ - 2..=n_ {
                    let mut s = cut();
                    s.c = Some(c);
                    out.push((format!("C={c}"), s));
                }
            }
            "depth1" | "depth1-old" => {
                let new = name == "depth1";
                for svc in &self.svcs {
                    for at in 0..n_ {
                        for d in durs(at) {
                            let mut s = base();
                            s.k = Some((svc.clone(), at, d));
                            out.push((format!("K={svc}@{at}+{d}"), s));
                        }
                    }
                }
                for svc in &self.net_svcs {
                    for at in 0..n_ {
                        for d in durs(at) {
                            let mut s = base();
                            s.n = Some((svc.clone(), at, at + d, one("loss", 60)));
                            out.push((format!("N={svc}@{at}-{}:loss,60%", at + d), s));
                        }
                    }
                }
                for c in 1..=n_ {
                    let mut s = cut();
                    s.c = Some(c);
                    out.push((format!("C={c}"), s));
                }
                if new {
                    for svc in &self.pause_svcs {
                        for at in 0..n_ {
                            for d in durs(at) {
                                let mut s = base();
                                s.p = Some(Pause::Plus(svc.clone(), at, d));
                                out.push((format!("P={svc}@{at}+{d}"), s));
                            }
                            for dur in [500, 30000] {
                                let mut s = base();
                                s.p = Some(Pause::Timed(svc.clone(), at, 0, dur));
                                out.push((format!("P={svc}@{at}~0:{dur}"), s));
                            }
                        }
                    }
                    for svc in &self.net_svcs {
                        for at in 0..n_ {
                            for d in durs(at) {
                                for dr in ["req", "resp"] {
                                    let mut s = base();
                                    s.o = Some((svc.clone(), at, at + d, dr.into(), one("loss", 100)));
                                    out.push((format!("O={svc}@{at}-{}:{dr}:loss,100%", at + d), s));
                                }
                            }
                        }
                    }
                    for at in 0..n_ {
                        for delta in [-86400, 365 * 86400] {
                            let mut s = base();
                            s.t = Some((at, delta));
                            out.push((format!("T={at}:{delta}"), s));
                        }
                    }
                }
            }
            "protocol" => {
                if self.pf_targets.is_empty() {
                    return Err("--sweep protocol needs a manifest with fakes: or proxies:".into());
                }
                for tg in &self.pf_targets {
                    let code: i64 = if tg.proto == "grpc" { 14 } else { 500 };
                    let mut kinds = tg.req.clone();
                    kinds.extend(tg.hook.iter().cloned());
                    for kind in kinds {
                        let st = || ("status".to_string(), RV::Int(code));
                        let variants: Vec<Vec<(String, RV)>> = match kind.as_str() {
                            "status" => vec![
                                vec![st()],
                                vec![st(), ("effect".into(), RV::Str("yes".into()))],
                            ],
                            "delay" => vec![vec![("delay".into(), RV::Int(3000))]],
                            "late" => vec![vec![("late".into(), RV::Int(5000))]],
                            k => vec![vec![(k.to_string(), RV::None)]],
                        };
                        for rules in variants {
                            for at in 0..n_ {
                                let h = H {
                                    target: tg.name.clone(),
                                    a: at,
                                    b: at + 1,
                                    rules: rules.clone(),
                                };
                                let mut s = base();
                                let label = render_h(&h)[2..].to_string();
                                s.h = vec![h];
                                out.push((label, s));
                            }
                        }
                    }
                }
            }
            _ => {
                return Err(format!(
                    "unknown sweep {} (kill, cut, net, killcut, pause, clock, oneway, frontkill, tail, depth1, depth1-old, protocol)",
                    crate::py::repr(name)
                ))
            }
        }
        Ok(out)
    }
}

/// `max(1, min(nmax, int(exp(k / 1000.0 * log(nmax)))))`: n of an F= token from a draw k in
/// 0..1000. The value depends on the last bit of libm's exp and log (k = 500, nmax = 64 gives
/// exp = 7.999999999999998, so 7), so tests/identity.rs compares the whole table with Python's.
pub fn fc_value(k: i64, nmax: i64) -> i64 {
    let e = k as f64 / 1000.0 * (nmax as f64).ln();
    1.max(nmax.min(e.exp() as i64))
}

fn split2(v: &str, sep: char) -> Result<(&str, &str), String> {
    let parts: Vec<&str> = v.split(sep).collect();
    if parts.len() != 2 {
        return Err(format!(
            "{} values to unpack in {v:?}",
            if parts.len() < 2 { "not enough" } else { "too many" }
        ));
    }
    Ok((parts[0], parts[1]))
}

/// explore.py `parse_fc`.
pub fn parse_fc(v: &str) -> Result<Fc, String> {
    static FC: OnceLock<Regex> = OnceLock::new();
    let c = re(
        &FC,
        r"^([a-z0-9_]+)([<>])([a-z0-9_]+)@(\d+)(?:#(\d+))?(?::([^:]*))?:(\d+)$",
    )
    .captures(v)
    .ok_or_else(|| format!("bad F= token {}", crate::py::repr(v)))?;
    let int = |x: &str| x.parse::<i64>().map_err(|e| format!("{x}: {e}"));
    Ok(Fc {
        svc: c[1].to_string(),
        dir: c[2].chars().next().unwrap_or('>'),
        peer: c[3].to_string(),
        step: int(&c[4])?,
        n: match c.get(5) {
            Some(m) if !m.as_str().is_empty() => int(m.as_str())?,
            _ => 1,
        },
        matched: c
            .get(6)
            .map(|m| m.as_str().to_string())
            .filter(|m| !m.is_empty()),
        dur: int(&c[7])?,
    })
}

/// explore.py `parse_h`.
pub fn parse_h(v: &str) -> Result<H, String> {
    static HR: OnceLock<Regex> = OnceLock::new();
    let c = re(
        &HR,
        r"^([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)?)@(\d+)-(\d+):(\S+)$",
    )
    .captures(v)
    .ok_or_else(|| format!("bad H= token {}", crate::py::repr(v)))?;
    let mut rules = Vec::new();
    for kv in c[4].split(',') {
        let (k, x) = match kv.split_once('=') {
            Some((k, x)) => (
                k,
                if isdigit(x) {
                    RV::Int(x.parse().map_err(|e| format!("{x}: {e}"))?)
                } else {
                    RV::Str(x.to_string())
                },
            ),
            None => (kv, RV::None),
        };
        rules.push((k.to_string(), x));
    }
    Ok(H {
        target: c[1].to_string(),
        a: c[2].parse().map_err(|e| format!("{e}"))?,
        b: c[3].parse().map_err(|e| format!("{e}"))?,
        rules,
    })
}

/// explore.py `tj_delays`: the (tick, delay) pairs a seeded jitter applies (hvx.c hvx_tj_delay).
pub fn tj_delays(seed: u64, pm: i64, mx: i64, lo: i64, hi: i64, k_first: i64, k_last: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let start = 2.max(k_first).max(lo);
    let mut k = start;
    while k <= k_last {
        if hi != 0 && k >= hi {
            break;
        }
        let h = mix64(seed.wrapping_add((k as u64).wrapping_mul(GOLDEN)));
        if ((h % 1000) as i64) < pm {
            out.push((k, 1 + ((h >> 16) % mx as u64) as i64));
        }
        k += 1;
    }
    out
}

fn tj_candidates(s: &S, tjk: Option<(i64, i64)>, var: &mut dyn FnMut(&dyn Fn(&mut S))) {
    let Some(tj) = &s.tj else { return };
    var(&|t| t.tj = None);
    match tj {
        Tj::Seed {
            seed,
            pm,
            max,
            lo,
            hi,
        } => {
            if let Some((a, b)) = tjk {
                let pairs = tj_delays(*seed, *pm, *max, *lo, *hi, a + 1, b);
                if !pairs.is_empty() && pairs.len() <= TJ_EXPLICIT_MAX {
                    var(&|t| t.tj = Some(Tj::X(pairs.clone())));
                } else if !pairs.is_empty() {
                    let mid = pairs[pairs.len() / 2].0;
                    let first = Tj::Seed {
                        seed: *seed,
                        pm: *pm,
                        max: *max,
                        lo: *lo,
                        hi: mid,
                    };
                    let second = Tj::Seed {
                        seed: *seed,
                        pm: *pm,
                        max: *max,
                        lo: mid,
                        hi: *hi,
                    };
                    var(&|t| t.tj = Some(first.clone()));
                    var(&|t| t.tj = Some(second.clone()));
                }
            }
        }
        Tj::X(kd) if kd.len() > 1 => {
            if kd.len() > 3 {
                let h = kd.len() / 2;
                var(&|t| t.tj = Some(Tj::X(kd[..h].to_vec())));
                var(&|t| t.tj = Some(Tj::X(kd[h..].to_vec())));
            }
            for i in 0..kd.len() {
                let mut k2 = kd.clone();
                k2.remove(i);
                var(&|t| t.tj = Some(Tj::X(k2.clone())));
            }
        }
        Tj::X(_) => {}
    }
}

/// explore.py `pf_candidates`: drop a token, narrow its window, drop a modifier, halve a delay.
fn pf_candidates(s: &S, var: &mut dyn FnMut(&dyn Fn(&mut S))) {
    let hs = &s.h;
    for (i, h) in hs.iter().enumerate() {
        let mut rest = hs.clone();
        rest.remove(i);
        let put = |var: &mut dyn FnMut(&dyn Fn(&mut S)), h2: H| {
            let mut l = rest.clone();
            l.insert(i, h2);
            var(&|t| t.h = l.clone());
        };
        var(&|t| t.h = rest.clone());
        if h.b - h.a > 1 {
            put(var, H { b: h.a + 1, ..h.clone() });
            if h.b - h.a > 2 {
                put(var, H { b: h.a + fdiv(h.b - h.a, 2), ..h.clone() });
            }
        }
        for m in ["p", "retry-after", "effect"] {
            if h.rules.iter().any(|(k, _)| k == m) {
                let rules = h.rules.iter().filter(|r| r.0 != m).cloned().collect();
                put(var, H { rules, ..h.clone() });
            }
        }
        for (k, v) in &h.rules {
            if (k == "delay" || k == "late") && v.int().is_some_and(|x| x > 100) {
                let x = v.int().unwrap_or(0);
                let rules = h
                    .rules
                    .iter()
                    .map(|(a, b)| {
                        if a == k {
                            (a.clone(), RV::Int(100.max(fdiv(x, 2))))
                        } else {
                            (a.clone(), b.clone())
                        }
                    })
                    .collect();
                put(var, H { rules, ..h.clone() });
            }
        }
    }
}

/// grpc_faults.candidates: shrink steps for the gRPC-only keys of every H= token.
fn grpc_candidates(s: &S, var: &mut dyn FnMut(&dyn Fn(&mut S))) {
    let hs = &s.h;
    for (i, h) in hs.iter().enumerate() {
        let rules = &h.rules;
        if !rules.iter().any(|(k, v)| {
            GRPC_KEYS.contains(&k.as_str()) || (k == "reset" && *v == RV::Str("conn".into()))
        }) {
            continue;
        }
        let put = |var: &mut dyn FnMut(&dyn Fn(&mut S)), r: Vec<(String, RV)>| {
            let mut l = hs.clone();
            l[i] = H {
                rules: r,
                ..h.clone()
            };
            var(&|t| t.h = l.clone());
        };
        for k in ["call", "rpc"] {
            if rules.iter().any(|(x, _)| x == k) {
                put(var, rules.iter().filter(|r| r.0 != k).cloned().collect());
            }
        }
        if rules.iter().any(|(x, _)| x == "seed") {
            put(
                var,
                rules
                    .iter()
                    .filter(|r| r.0 != "p" && r.0 != "seed")
                    .cloned()
                    .collect(),
            );
        }
        let conn = ("reset".to_string(), RV::Str("conn".into()));
        if rules.contains(&conn) {
            put(
                var,
                rules
                    .iter()
                    .map(|r| {
                        if *r == conn {
                            ("reset".to_string(), RV::None)
                        } else {
                            r.clone()
                        }
                    })
                    .collect(),
            );
        }
        let dreq = ("dir".to_string(), RV::Str("req".into()));
        if rules.contains(&dreq) {
            put(var, rules.iter().filter(|r| **r != dreq).cloned().collect());
        }
        for k in ["cut", "hold"] {
            for (x, v) in rules {
                if x == k && v.int().is_some_and(|n| n > 0) {
                    let n = v.int().unwrap_or(0);
                    put(
                        var,
                        rules
                            .iter()
                            .map(|(x2, v2)| {
                                if x2 == k {
                                    (x2.clone(), RV::Int(fdiv(n, 2)))
                                } else {
                                    (x2.clone(), v2.clone())
                                }
                            })
                            .collect(),
                    );
                }
            }
        }
    }
}
