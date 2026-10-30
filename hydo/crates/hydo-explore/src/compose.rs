//! The test composer's explorer side (hydo/onboard/hydo_composer.py: `draw`, `candidates`,
//! `stable_w`, `describe`, `contents_of`, `fits`, `key_of`, and the W= forms they read and write).
//!
//! A composed plan's schedules carry `W=<seed>,p<at once>,q<per round>,r<round ms>[,n,t,x,o]` (the
//! seed form), `W=:<resolved>` (commands by id, M98) or `W=@<key>` (a saved resolved form). The
//! plan meta is the JSON hydo-build stages as /etc/hydo/compose.json (`commands` with `id` and
//! `kind`, `params`, `steps`).

use crate::py::{head, repr};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const GUEST_MAX: usize = 500;
pub const WORKLOAD_KINDS: [&str; 7] = [
    "first",
    "parallel_driver",
    "serial_driver",
    "singleton_driver",
    "anytime",
    "eventually",
    "finally",
];

/// The plan's commands as (id, kind), in index order.
pub fn commands(meta: &Value) -> Vec<(String, String)> {
    meta.get("commands")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|c| {
                    (
                        c.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                        c.get("kind").and_then(Value::as_str).unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn pint(meta: &Value, k: &str) -> i64 {
    meta.get("params")
        .and_then(|p| p.get(k))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

pub fn steps_of(meta: &Value) -> i64 {
    meta.get("steps").and_then(Value::as_i64).unwrap_or(0)
}

const M31: i64 = 2147483647;

struct Minstd {
    s: i64,
}

impl Minstd {
    fn new(seed: i64) -> Minstd {
        let mut m = Minstd {
            s: seed.rem_euclid(M31 - 1) + 1,
        };
        for _ in 0..8 {
            m.next();
        }
        m
    }
    fn next(&mut self) -> i64 {
        self.s = (self.s * 48271) % M31;
        self.s
    }
    fn below(&mut self, n: i64) -> i64 {
        self.next().rem_euclid(n)
    }
}

/// (entry, round, command index, entry seed)
pub type Entry = (i64, i64, usize, i64);

/// hydo_composer.plan: every entry of the composition before W's filters.
pub fn plan(seed: i64, q: i64, steps: i64, cmds: &[(String, String)]) -> Vec<Entry> {
    let idx = |k: &str| -> Vec<usize> {
        cmds.iter()
            .enumerate()
            .filter(|(_, c)| c.1 == k)
            .map(|(i, _)| i)
            .collect()
    };
    let (p, se, sg, a) = (
        idx("parallel_driver"),
        idx("serial_driver"),
        idx("singleton_driver"),
        idx("anytime"),
    );
    let mut rng = Minstd::new(seed);
    let mut out: Vec<Entry> = Vec::new();
    let single = !sg.is_empty() && ((p.is_empty() && se.is_empty()) || rng.below(4) == 0);
    let g = if single {
        sg[rng.below(sg.len() as i64) as usize]
    } else {
        usize::MAX
    };
    for r in 0..steps {
        if single {
            if r == 0 {
                let sd = rng.next();
                out.push((out.len() as i64, r, g, sd));
            }
        } else if !se.is_empty() && (p.is_empty() || rng.below(4) == 0) {
            let c = se[rng.below(se.len() as i64) as usize];
            let sd = rng.next();
            out.push((out.len() as i64, r, c, sd));
        } else if !p.is_empty() {
            let k = 1 + rng.below(q);
            for _ in 0..k {
                let c = p[rng.below(p.len() as i64) as usize];
                let sd = rng.next();
                out.push((out.len() as i64, r, c, sd));
            }
        }
        if !a.is_empty() && ((p.is_empty() && se.is_empty() && sg.is_empty()) || rng.below(3) == 0) {
            let c = a[rng.below(a.len() as i64) as usize];
            let sd = rng.next();
            out.push((out.len() as i64, r, c, sd));
        }
    }
    out
}

/// An `o` item: a command index (old form) or a command id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OItem {
    I(i64),
    S(String),
}

/// A seed-form W value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WSeed {
    pub seed: i64,
    pub p: i64,
    pub q: i64,
    pub r: i64,
    pub n: i64,
    pub t: i64,
    pub x: BTreeSet<i64>,
    pub o: BTreeSet<OItem>,
}

fn ilist(s: &str) -> Result<BTreeSet<i64>, String> {
    let mut out = BTreeSet::new();
    if s.is_empty() {
        return Ok(out);
    }
    for it in s.split('.') {
        if let Some((a, b)) = it.split_once('-') {
            let a: i64 = a.parse().map_err(|_| format!("bad range {it}"))?;
            let b: i64 = b.parse().map_err(|_| format!("bad range {it}"))?;
            out.extend(a..=b);
        } else if !it.is_empty() {
            out.insert(it.parse().map_err(|_| format!("bad number {it}"))?);
        }
    }
    Ok(out)
}

fn rlist(xs: &BTreeSet<i64>) -> String {
    let xs: Vec<i64> = xs.iter().copied().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < xs.len() {
        let mut j = i;
        while j + 1 < xs.len() && xs[j + 1] == xs[j] + 1 {
            j += 1;
        }
        out.push(if i == j {
            format!("{}", xs[i])
        } else {
            format!("{}-{}", xs[i], xs[j])
        });
        i = j + 1;
    }
    out.join(".")
}

/// hydo_composer.parse_w.
pub fn parse_w(w: &str, meta: &Value) -> Result<WSeed, String> {
    let mut d = WSeed {
        seed: 0,
        p: pint(meta, "parallel"),
        q: pint(meta, "per_round"),
        r: pint(meta, "round_ms"),
        n: pint(meta, "max_commands"),
        t: pint(meta, "budget_ms"),
        x: BTreeSet::new(),
        o: BTreeSet::new(),
    };
    if w.is_empty() || w == "-" {
        return Ok(d);
    }
    let f: Vec<&str> = w.split(',').collect();
    d.seed = (i64::from_str_radix(f[0], 16).map_err(|e| format!("W={w}: {e}"))?) & 0xFFFF_FFFF;
    for x in &f[1..] {
        if x.is_empty() {
            continue;
        }
        let k = x.chars().next().unwrap_or(' ');
        let v = &x[k.len_utf8()..];
        let num = || v.parse::<i64>().map_err(|e| format!("W={w}: {e}"));
        match k {
            'p' => d.p = num()?,
            'q' => d.q = num()?,
            'r' => d.r = num()?,
            'n' => d.n = num()?,
            't' => d.t = num()?,
            'o' if v.chars().any(|c| c.is_ascii_alphabetic()) => {
                d.o = v.split('+').map(|x| OItem::S(x.to_string())).collect()
            }
            'x' => d.x = ilist(v)?,
            'o' => d.o = ilist(v)?.into_iter().map(OItem::I).collect(),
            _ => return Err(format!("W={}: unknown field {}", w, repr(x))),
        }
    }
    Ok(d)
}

/// hydo_composer.render_w.
pub fn render_w(d: &WSeed) -> String {
    let mut s = format!("{:08x},p{},q{},r{}", d.seed, d.p, d.q, d.r);
    if d.n != 0 {
        s.push_str(&format!(",n{}", d.n));
    }
    if d.t != 0 {
        s.push_str(&format!(",t{}", d.t));
    }
    if !d.x.is_empty() {
        s.push_str(&format!(",x{}", rlist(&d.x)));
    }
    if !d.o.is_empty() {
        if d.o.iter().all(|c| matches!(c, OItem::I(_))) {
            let ints: BTreeSet<i64> = d
                .o
                .iter()
                .map(|c| match c {
                    OItem::I(i) => *i,
                    OItem::S(_) => 0,
                })
                .collect();
            s.push_str(&format!(",o{}", rlist(&ints)));
        } else {
            let mut ids: Vec<String> = d
                .o
                .iter()
                .map(|c| match c {
                    OItem::I(i) => i.to_string(),
                    OItem::S(x) => x.clone(),
                })
                .collect();
            ids.sort();
            s.push_str(&format!(",o{}", ids.join("+")));
        }
    }
    s
}

/// hydo_composer.kept: the plan minus x (entries) and o (commands), cut at n.
pub fn kept(d: &WSeed, steps: i64, cmds: &[(String, String)]) -> Vec<Entry> {
    let es: Vec<Entry> = plan(d.seed, d.q, steps, cmds)
        .into_iter()
        .filter(|e| {
            !d.x.contains(&e.0)
                && !d.o.contains(&OItem::I(e.2 as i64))
                && !d.o.contains(&OItem::S(cmds[e.2].0.clone()))
        })
        .collect();
    if d.n != 0 {
        let k = if d.n < 0 {
            (es.len() as i64 + d.n).max(0) as usize
        } else {
            (d.n as usize).min(es.len())
        };
        es[..k].to_vec()
    } else {
        es
    }
}

/// A resolved composition (`:p2,r200[,t300],c<id>+<id>,<entry>.<round>.<k>.<seed>,...`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub p: i64,
    pub r: i64,
    pub t: i64,
    pub entries: Vec<(i64, i64, String, i64)>,
}

fn kind_of(cid: &str) -> Option<&'static str> {
    let mut ks: Vec<&'static str> = WORKLOAD_KINDS.to_vec();
    ks.sort_by_key(|k| std::cmp::Reverse(k.len()));
    ks.into_iter().find(|k| cid.starts_with(&format!("{k}_")))
}

fn name_ok(x: &str) -> bool {
    // hydo_composer.NAME_RE = ^[A-Za-z0-9_.\-]+$ (match with $: a trailing newline is allowed)
    let x = x.strip_suffix('\n').unwrap_or(x);
    !x.is_empty()
        && x.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// hydo_composer.parse_resolved.
pub fn parse_resolved(v: &str) -> Result<Resolved, String> {
    let Some(body) = v.strip_prefix(':') else {
        return Err(format!(
            "a resolved composition starts with ':' (got {})",
            repr(&head(v, 20))
        ));
    };
    let (mut p, mut r, mut t) = (None, None, 0i64);
    let mut entries = Vec::new();
    let mut ids: Option<Vec<String>> = None;
    for f in body.split(',') {
        let first = f.chars().next();
        if matches!(first, Some('p' | 'r' | 't')) && all_digits(&f[1..]) {
            let val: i64 = f[1..].parse().map_err(|e| format!("{e}"))?;
            match first {
                Some('p') => p = Some(val),
                Some('r') => r = Some(val),
                _ => t = val,
            }
        } else if f.starts_with('c') && ids.is_none() {
            let l: Vec<String> = f[1..].split('+').map(str::to_string).collect();
            if !l.iter().all(|x| name_ok(x) && kind_of(x).is_some()) {
                return Err(format!(
                    "W={}: c lists command ids <kind>_<name>",
                    head(v, 40)
                ));
            }
            ids = Some(l);
        } else if f.split('.').count() == 4 && f.split('.').all(all_digits) {
            let parts: Vec<i64> = f.split('.').map(|x| x.parse().unwrap_or(0)).collect();
            let k = parts[2];
            match &ids {
                Some(l) if (k as usize) < l.len() => {
                    entries.push((parts[0], parts[1], l[k as usize].clone(), parts[3]))
                }
                _ => {
                    return Err(format!(
                        "W={}: entry {} names command {} of a table of {}",
                        head(v, 40),
                        f,
                        k,
                        ids.as_ref().map(|l| l.len()).unwrap_or(0)
                    ))
                }
            }
        } else {
            return Err(format!("W={}: unknown field {}", head(v, 40), repr(f)));
        }
    }
    let (Some(p), Some(r)) = (p.filter(|x| *x != 0), r.filter(|x| *x != 0)) else {
        return Err(format!(
            "W={}: a resolved composition has p, r and at least one entry",
            head(v, 40)
        ));
    };
    if entries.is_empty() {
        return Err(format!(
            "W={}: a resolved composition has p, r and at least one entry",
            head(v, 40)
        ));
    }
    let nums: BTreeSet<i64> = entries.iter().map(|e| e.0).collect();
    if nums.len() != entries.len() || entries.windows(2).any(|w| w[0].1 > w[1].1) {
        return Err(format!(
            "W={}: entry numbers are unique and rounds never go back",
            head(v, 40)
        ));
    }
    Ok(Resolved { p, r, t, entries })
}

/// hydo_composer.render_resolved.
pub fn render_resolved(d: &Resolved) -> String {
    let mut ids: Vec<String> = Vec::new();
    for e in &d.entries {
        if !ids.contains(&e.2) {
            ids.push(e.2.clone());
        }
    }
    let mut out = format!(":p{},r{}", d.p, d.r);
    if d.t != 0 {
        out.push_str(&format!(",t{}", d.t));
    }
    out.push_str(&format!(",c{}", ids.join("+")));
    for (e, r, cid, sd) in &d.entries {
        let k = ids.iter().position(|x| x == cid).unwrap_or(0);
        out.push_str(&format!(",{e}.{r}.{k}.{sd}"));
    }
    out
}

/// hydo_composer.w_of: the W= value of a schedule's guest part.
pub fn w_of(schedule: &str) -> Option<String> {
    let g = schedule.split_once('|').map(|x| x.1).unwrap_or(schedule);
    g.split_whitespace()
        .find_map(|t| t.strip_prefix("W=").map(str::to_string))
}

/// hydo_composer.fits: the guest part fits the engines' schedule buffer.
pub fn fits(schedule: &str) -> bool {
    let g = schedule.split_once('|').map(|x| x.1).unwrap_or(schedule);
    g.trim().chars().count() <= GUEST_MAX
}

/// hydo_composer.key_of: the key of a resolved composition (W=@<key>).
pub fn key_of(resolved: &str) -> String {
    let h = Sha256::digest(resolved.as_bytes());
    h.iter().map(|b| format!("{b:02x}")).collect::<String>()[..12].to_string()
}

fn resolve_seed(w: &str, meta: &Value, steps: i64) -> Result<Resolved, String> {
    let cmds = commands(meta);
    let d = parse_w(w, meta)?;
    Ok(Resolved {
        p: d.p,
        r: d.r,
        t: d.t,
        entries: kept(&d, steps, &cmds)
            .into_iter()
            .map(|(e, r, c, s)| (e, r, cmds[c].0.clone(), s))
            .collect(),
    })
}

/// hydo_composer.stable_w: W in the resolved form, from the seed or resolved form.
pub fn stable_w(w: &str, meta: &Value) -> Result<String, String> {
    if w.starts_with(':') {
        return Ok(w.to_string());
    }
    if w.starts_with('@') {
        return Err(format!("W={w}: the saved composition is not known here"));
    }
    Ok(render_resolved(&resolve_seed(w, meta, steps_of(meta))?))
}

/// hydo_composer.contents_of: {id: content hash} of the commands a composition uses (sorted ids).
pub fn contents_of(w: &str, meta: &Value) -> Result<serde_json::Map<String, Value>, String> {
    let used: BTreeSet<String> = parse_resolved(&stable_w(w, meta)?)?
        .entries
        .into_iter()
        .map(|e| e.2)
        .collect();
    let mut out = serde_json::Map::new();
    let cmds = meta.get("commands").and_then(Value::as_array);
    for cid in used {
        let content = cmds.and_then(|a| {
            a.iter()
                .find(|c| c.get("id").and_then(Value::as_str) == Some(cid.as_str()))
                .and_then(|c| c.get("content"))
        });
        if let Some(c) = content.filter(|c| hydo_manifest::py::truthy(c)) {
            out.insert(cid, c.clone());
        }
    }
    Ok(out)
}

/// hydo_composer.entries_of: (entry, round, command index, seed) any form of W runs here.
fn entries_of(w: &str, meta: &Value) -> Result<Vec<Entry>, String> {
    let steps = steps_of(meta);
    let cmds = commands(meta);
    if w.starts_with(':') || w.starts_with('@') {
        let d = parse_resolved(&stable_w(w, meta)?)?;
        return d
            .entries
            .into_iter()
            .map(|(e, r, cid, s)| {
                cmds.iter()
                    .position(|c| c.0 == cid)
                    .map(|i| (e, r, i, s))
                    .ok_or_else(|| format!("the saved composition uses command {cid}"))
            })
            .collect();
    }
    Ok(kept(&parse_w(w, meta)?, steps, &cmds))
}

/// hydo_composer.describe: one line per round, for reports.
pub fn describe(w: &str, meta: &Value) -> Result<String, String> {
    let cmds = commands(meta);
    let mut rounds: std::collections::BTreeMap<i64, Vec<String>> = Default::default();
    for (_, r, c, _) in entries_of(w, meta)? {
        rounds.entry(r).or_default().push(cmds[c].0.clone());
    }
    Ok(rounds
        .iter()
        .map(|(r, ids)| format!("r{}: {}", r, ids.join(", ")))
        .collect::<Vec<_>>()
        .join(" | "))
}

/// hydo_composer.draw: a W value from the explorer's own random stream.
pub fn draw(next: &mut dyn FnMut() -> u64, meta: &Value) -> String {
    let (par, per, rms) = (
        pint(meta, "parallel"),
        pint(meta, "per_round"),
        pint(meta, "round_ms"),
    );
    let seed = (next() & 0xFFFF_FFFF) as i64;
    let p = 1 + (next() % par as u64) as i64;
    let q = 1 + (next() % per as u64) as i64;
    let opts = [(rms / 2).max(10), rms, (rms * 2).min(60000)];
    let r = opts[(next() % 3) as usize];
    render_w(&WSeed {
        seed,
        p,
        q,
        r,
        n: pint(meta, "max_commands"),
        t: pint(meta, "budget_ms"),
        x: BTreeSet::new(),
        o: BTreeSet::new(),
    })
}

/// hydo_composer.candidates: simpler compositions than W, most aggressive first.
pub fn candidates(w: &str, meta: &Value) -> Vec<String> {
    if w.starts_with(':') {
        return resolved_candidates(w);
    }
    if w.starts_with('@') {
        return Vec::new();
    }
    let Ok(d) = parse_w(w, meta) else {
        return Vec::new();
    };
    let cmds = commands(meta);
    let ks = kept(&d, steps_of(meta), &cmds);
    let o_ids: BTreeSet<OItem> = d
        .o
        .iter()
        .filter_map(|c| match c {
            OItem::I(i) => cmds
                .get(*i as usize)
                .filter(|_| *i >= 0)
                .map(|c| OItem::S(c.0.clone())),
            OItem::S(s) => Some(OItem::S(s.clone())),
        })
        .collect();
    let mut out: Vec<String> = Vec::new();
    let base = WSeed {
        o: o_ids.clone(),
        ..d.clone()
    };
    let mut var = |f: &dyn Fn(&mut WSeed)| {
        let mut t = base.clone();
        f(&mut t);
        out.push(render_w(&t));
    };
    let mut used: Vec<usize> = ks.iter().map(|e| e.2).collect();
    used.sort();
    used.dedup();
    if used.len() > 1 {
        for c in &used {
            let id = cmds[*c].0.clone();
            var(&|t: &mut WSeed| {
                t.o.insert(OItem::S(id.clone()));
            });
        }
    }
    if ks.len() > 1 {
        let n = (ks.len() as i64 / 2).max(1);
        var(&|t: &mut WSeed| t.n = n);
    }
    if d.p > 1 {
        var(&|t: &mut WSeed| t.p = 1);
        if d.p > 2 {
            let p = d.p - 1;
            var(&|t: &mut WSeed| t.p = p);
        }
    }
    if ks.len() > 3 {
        let half = ks.len() / 2;
        let xs: Vec<i64> = ks[..half].iter().map(|e| e.0).collect();
        var(&|t: &mut WSeed| t.x.extend(xs.iter().copied()));
    }
    if ks.len() > 1 && ks.len() <= 8 {
        for e in &ks {
            let x = e.0;
            var(&|t: &mut WSeed| {
                t.x.insert(x);
            });
        }
    }
    dedup_except(out, w)
}

fn dedup_except(out: Vec<String>, w: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut res = Vec::new();
    for c in out {
        if c != w && seen.insert(c.clone()) {
            res.push(c);
        }
    }
    res
}

fn resolved_candidates(w: &str) -> Vec<String> {
    let Ok(d) = parse_resolved(w) else {
        return Vec::new();
    };
    let es = d.entries.clone();
    let mut out = Vec::new();
    let mut var = |entries: Option<Vec<(i64, i64, String, i64)>>, p: Option<i64>| {
        let mut t = d.clone();
        if let Some(p) = p {
            t.p = p;
        }
        t.entries = entries.unwrap_or_else(|| es.clone());
        if !t.entries.is_empty() {
            out.push(render_resolved(&t));
        }
    };
    let mut used: Vec<String> = Vec::new();
    for e in &es {
        if !used.contains(&e.2) {
            used.push(e.2.clone());
        }
    }
    if used.len() > 1 {
        for cid in &used {
            var(
                Some(es.iter().filter(|e| e.2 != *cid).cloned().collect()),
                None,
            );
        }
    }
    if es.len() > 1 {
        let k = (es.len() / 2).max(1);
        var(Some(es[..k].to_vec()), None);
    }
    if d.p > 1 {
        var(None, Some(1));
        if d.p > 2 {
            var(None, Some(d.p - 1));
        }
    }
    if es.len() > 3 {
        var(Some(es[es.len() / 2..].to_vec()), None);
    }
    if es.len() > 1 && es.len() <= 8 {
        for i in 0..es.len() {
            let mut e2 = es.clone();
            e2.remove(i);
            var(Some(e2), None);
        }
    }
    dedup_except(out, w)
}
