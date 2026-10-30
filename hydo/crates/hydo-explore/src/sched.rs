//! The explorer's working form of a schedule: explore.py's schedule dict as a struct. Generators,
//! the shrinker and the fault-space filters change it; `Ex::render` prints it as the schedule
//! string and `Ex::parse` reads one back (both in gen.rs, because they depend on the fault space).
//!
//! The field values keep explore.py's types where the output depends on them: a knob value is a
//! number or a text (`M=2` parses as the number 2), a protocol-fault rule value is absent, a number
//! or a text (`delay=0300` parses as 300), a pause is counted (`+n`) or timed (`~off:dur`).

use std::fmt;

/// A knob value (`M=`, `R=`, a manifest knob): explore.py keeps `M` as an int when it is digits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KV {
    Int(i64),
    Str(String),
}

impl fmt::Display for KV {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KV::Int(i) => write!(f, "{i}"),
            KV::Str(s) => f.write_str(s),
        }
    }
}

/// `P=`: paused for n orders (`svc@at+n`) or, timed, `off` ms after order `at` for `dur` ms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pause {
    Plus(String, i64, i64),
    Timed(String, i64, i64, i64),
}

impl Pause {
    pub fn svc(&self) -> &str {
        match self {
            Pause::Plus(s, ..) | Pause::Timed(s, ..) => s,
        }
    }
}

/// Tick jitter: seeded (`tj= tjpm= tjmax= [tjlo= tjhi=]`) or an explicit list (`tjx=`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tj {
    Seed {
        seed: u64,
        pm: i64,
        max: i64,
        lo: i64,
        hi: i64,
    },
    X(Vec<(i64, i64)>),
}

/// A protocol-fault rule value: none (`drop`), a number (`status=503`) or a text (`effect=yes`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RV {
    None,
    Int(i64),
    Str(String),
}

impl RV {
    pub fn int(&self) -> Option<i64> {
        match self {
            RV::Int(i) => Some(*i),
            _ => None,
        }
    }
}

pub type Rule = (String, RV);

/// One `H=<target>@<from>-<to>:<rules>` token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct H {
    pub target: String,
    pub a: i64,
    pub b: i64,
    pub rules: Vec<Rule>,
}

/// `F=<svc><dir><peer>@<step>#<n>[:<match>]:<dur>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fc {
    pub svc: String,
    pub dir: char,
    pub peer: String,
    pub step: i64,
    pub n: i64,
    pub matched: Option<String>,
    pub dur: i64,
}

pub type Kinds = Vec<(String, i64)>;

/// explore.py's schedule dict. `None` / empty = the key is absent.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct S {
    pub disk: String,
    pub dseed: u64,
    /// K=svc@at+down
    pub k: Option<(String, i64, i64)>,
    /// N=svc@a-b:netem
    pub n: Option<(String, i64, i64, Kinds)>,
    /// C=at
    pub c: Option<i64>,
    /// Knob tokens (M, R, a manifest's; "-" = a placeholder draw), in insertion order.
    pub knobs: Vec<(String, KV)>,
    pub p: Option<Pause>,
    /// T=at:delta
    pub t: Option<(i64, i64)>,
    /// O=svc@a-b:dir:netem
    pub o: Option<(String, i64, i64, String, Kinds)>,
    /// Q=n[d]
    pub cq: Option<(i64, bool)>,
    /// X=svc@at~off
    pub x: Option<(String, i64, i64)>,
    pub b: Option<String>,
    pub f: Option<Fc>,
    pub h: Vec<H>,
    pub w: Option<String>,
    pub tj: Option<Tj>,
    pub pp: Option<i64>,
    pub xt: Option<(u64, i64, i64)>,
    pub pct: Option<(u64, i64, i64)>,
    pub cf: Option<i64>,
    /// q= qs= (vCPU turns)
    pub q: Option<(i64, u64)>,
    /// qb=len:q:pm qbs=seed
    pub qb: Option<(i64, i64, i64, u64)>,
    pub rb: Option<i64>,
}

impl S {
    pub fn new(disk: &str, dseed: u64) -> S {
        S {
            disk: disk.to_string(),
            dseed,
            ..S::default()
        }
    }

    pub fn knob(&self, tok: &str) -> Option<&KV> {
        self.knobs.iter().find(|(t, _)| t == tok).map(|(_, v)| v)
    }

    /// `s[tok] = v` (in place when present, else appended).
    pub fn set_knob(&mut self, tok: &str, v: KV) {
        if let Some(e) = self.knobs.iter_mut().find(|(t, _)| t == tok) {
            e.1 = v;
        } else {
            self.knobs.push((tok.to_string(), v));
        }
    }

    pub fn pop_knob(&mut self, tok: &str) {
        self.knobs.retain(|(t, _)| t != tok);
    }

    pub fn disk_is(&self, prefix: &str) -> bool {
        self.disk.starts_with(prefix)
    }
}

/// explore.py `netem_args(kinds)`.
pub fn netem_args(kinds: &[(String, i64)]) -> String {
    let mut out: Vec<String> = Vec::new();
    let has_delay = kinds.iter().any(|(k, _)| k == "delay");
    for (k, v) in kinds {
        if k == "delay" {
            out.push("delay".into());
            out.push(format!("{v}ms"));
        } else if k == "reorder" {
            if !has_delay {
                out.push("delay".into());
                out.push("20ms".into());
            }
            out.push("reorder".into());
            out.push(format!("{v}%"));
        } else {
            out.push(k.clone());
            out.push(format!("{v}%"));
        }
    }
    out.join(",")
}

/// explore.py `render_fc(f)`.
pub fn render_fc(f: &Fc) -> String {
    format!(
        "F={}{}{}@{}#{}{}:{}",
        f.svc,
        f.dir,
        f.peer,
        f.step,
        f.n,
        match &f.matched {
            Some(m) if !m.is_empty() => format!(":{m}"),
            _ => String::new(),
        },
        f.dur
    )
}

/// explore.py `render_h(h)`.
pub fn render_h(h: &H) -> String {
    let rules: Vec<String> = h
        .rules
        .iter()
        .map(|(k, v)| match v {
            RV::None => k.clone(),
            RV::Int(i) => format!("{k}={i}"),
            RV::Str(s) => format!("{k}={s}"),
        })
        .collect();
    format!("H={}@{}-{}:{}", h.target, h.a, h.b, rules.join(","))
}

/// explore.py `render_tj(tj)`.
pub fn render_tj(tj: &Tj) -> String {
    match tj {
        Tj::X(kd) => format!(
            " tjx={}",
            kd.iter()
                .map(|(k, d)| format!("{k}:{d}"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Tj::Seed {
            seed,
            pm,
            max,
            lo,
            hi,
        } => {
            let mut t = format!(" tj={seed:x} tjpm={pm} tjmax={max}");
            if *lo != 0 || *hi != 0 {
                t.push_str(&format!(" tjlo={lo} tjhi={hi}"));
            }
            t
        }
    }
}

/// explore.py `render_preempt(s)`.
pub fn render_preempt(s: &S) -> String {
    let mut out = s.pp.map(|p| format!(" pp={p}")).unwrap_or_default();
    if let Some((a, b, c)) = s.xt {
        out.push_str(&format!(" xt={a:x} xtr={b} xtpm={c}"));
    }
    if let Some((a, b, c)) = s.pct {
        out.push_str(&format!(" pct={a:x} pctn={b} pctk={c}"));
    }
    out
}

/// explore.py `render_turns(s)`.
pub fn render_turns(s: &S) -> String {
    let mut out = String::new();
    if let Some((q, qs)) = s.q {
        if q != 0 {
            out.push_str(&format!(" q={q}"));
        }
        if qs != 0 {
            out.push_str(&format!(" qs={qs:x}"));
        }
    }
    if let Some((a, b, c, d)) = s.qb {
        out.push_str(&format!(" qb={a}:{b}:{c} qbs={d:x}"));
    }
    out
}
