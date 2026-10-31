//! The freeze sweep (`--sweep freeze`, M76) and the adaptive freeze sweep (`--fc-adaptive`, M92;
//! the port of fc_adaptive.py).
//!
//! Every packet point of a run's traffic is one schedule `F=<svc><dir><peer>@<step>#<n>[:<match>]:<dur>`.
//! The points come from the run itself: for each (pair, direction, match) stream and each step s,
//! one `#0` trace run makes the guest print "freeze never fired: <N> of 0 matching events after
//! step <s>"; step s then holds N(s) - N(s+1) packets (+ a margin where step s+1 is not swept).
//! The adaptive sweep runs a cheap stage 1 (the manifest's freeze_match: streams, or every k-th
//! packet) and then every packet in a widening window around each stage-1 failure.

use crate::args::Args;
use crate::gen::Ex;
use crate::pool::{read_text, Pool, Res};
use crate::py::{dumps_line, py_str, repr, round1};
use crate::report::{assert_failures, records, verdict_kind};
use crate::results::Events;
use crate::sched::{render_fc, Fc, S};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

pub type Point = (String, S);

fn trace_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"freeze never fired: (\d+) of 0 matching events after step (\d+)").expect("p")
    })
}

/// The count of a #0 trace run's console for step `st` (the last such line).
fn trace_count(text: &str, st: i64) -> Option<i64> {
    let mut n = None;
    for c in trace_re().captures_iter(text) {
        if c[2].parse::<i64>().ok() == Some(st) {
            n = c[1].parse().ok();
        }
    }
    n
}

/// `--fc-steps` (8-9, 3,7) or every step.
pub fn steps_of(a: &Args, norders: i64) -> Result<Vec<i64>, String> {
    let steps: Vec<i64> = match &a.fc_steps {
        Some(t) => {
            let mut set = BTreeSet::new();
            for part in t.split(',') {
                if let Some((lo, hi)) = part.split_once('-') {
                    let lo: i64 = lo.trim().parse().map_err(|e| format!("--fc-steps: {e}"))?;
                    let hi: i64 = hi.trim().parse().map_err(|e| format!("--fc-steps: {e}"))?;
                    set.extend(lo..=hi);
                } else {
                    set.insert(
                        part.trim()
                            .parse::<i64>()
                            .map_err(|e| format!("--fc-steps: {e}"))?,
                    );
                }
            }
            set.into_iter().collect()
        }
        None => (0..norders).collect(),
    };
    if steps.is_empty() || steps[0] < 0 || *steps.last().unwrap_or(&0) >= norders {
        return Err(format!(
            "explore.py: --fc-steps must lie in 0..{}",
            norders - 1
        ));
    }
    Ok(steps)
}

fn dirs_of(a: &Args) -> Vec<char> {
    let d = a.fc_dirs.replace("resp", "<").replace("req", ">");
    ['<', '>'].into_iter().filter(|c| d.contains(*c)).collect()
}

fn want_pairs(a: &Args) -> Option<Vec<(String, String)>> {
    let t = a.fc_pairs.as_ref()?;
    Some(
        t.split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| match p.find(['>', ':']) {
                Some(i) => (p[..i].to_string(), p[i + 1..].to_string()),
                None => (p.to_string(), String::new()),
            })
            .collect(),
    )
}

/// explore.py `fc_sweep_with`: the fixed fault every point carries.
pub fn sweep_with(ex: &Ex, text: Option<&str>) -> Result<S, String> {
    let Some(t) = text.filter(|t| !t.is_empty()) else {
        return Ok(S::new("none", 0));
    };
    let t = if t.contains('|') {
        t.to_string()
    } else {
        format!("disk=none dseed=0 | run {t}")
    };
    let s = ex.parse(&t)?;
    if s.f.is_some() {
        return Err("explore.py: --sweep-with may not carry F= (the sweep places it)".into());
    }
    Ok(s)
}

type Stream = (String, String, char, Option<String>);

/// explore.py `fc_sweep_streams`.
fn streams_of(ex: &Ex, a: &Args) -> Result<Vec<Stream>, String> {
    let mut pairs = ex.fc_pairs.clone();
    if let Some(want) = want_pairs(a) {
        let bad: Vec<&(String, String)> = want.iter().filter(|p| !pairs.contains(p)).collect();
        if !bad.is_empty() {
            let j = |v: Vec<&(String, String)>| {
                v.iter()
                    .map(|(x, y)| format!("{x}>{y}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            return Err(format!(
                "explore.py: --fc-pairs {}: not freeze pairs of this manifest ({})",
                j(bad),
                j(pairs.iter().collect())
            ));
        }
        pairs = want;
    }
    let dirs = dirs_of(a);
    let mut streams: Vec<(char, Option<String>)> = Vec::new();
    for item in a.fc_match.split(',') {
        let item = item.trim();
        if item == "all" {
            streams.extend(dirs.iter().map(|d| (*d, None)));
        } else if item == "manifest" {
            for (m, md) in &ex.fc_match {
                let ds: Vec<char> = md.map(|x| vec![x]).unwrap_or_else(|| dirs.clone());
                streams.extend(
                    ds.into_iter()
                        .filter(|d| dirs.contains(d))
                        .map(|d| (d, Some(m.clone()))),
                );
            }
        } else if !item.is_empty() {
            if item.contains(':') || item.contains(' ') {
                return Err(format!(
                    "explore.py: --fc-match {}: a match string may not contain ':' or a space",
                    repr(item)
                ));
            }
            let md = ex
                .fc_match
                .iter()
                .rev()
                .find(|(m, _)| m == item)
                .and_then(|x| x.1);
            let ds: Vec<char> = md.map(|x| vec![x]).unwrap_or_else(|| dirs.clone());
            streams.extend(
                ds.into_iter()
                    .filter(|d| dirs.contains(d))
                    .map(|d| (d, Some(item.to_string()))),
            );
        }
    }
    let mut out: Vec<Stream> = Vec::new();
    for (svc, peer) in &pairs {
        for (d, m) in &streams {
            let k = (svc.clone(), peer.clone(), *d, m.clone());
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    Ok(out)
}

/// `--fc-traces`: the trace runs of earlier sweeps, by schedule. A file is an exploration's
/// events.jsonl (its trace runs, read with the results reader) or, from before M115, an
/// fc-traces.jsonl.
fn known_traces(a: &Args) -> Result<HashMap<String, Map<String, Value>>, String> {
    let mut known = HashMap::new();
    for path in a.fc_traces.iter().flat_map(|t| t.split(',')) {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        if text.lines().next().is_some_and(|l| l.contains("\"v\":")) {
            let st = hydo_results::reader::stream(&text).map_err(|e| format!("{path}: {e}"))?;
            let dir = Path::new(path).parent().unwrap_or(Path::new(""));
            for r in st.runs() {
                let Some(t) = r.trace.as_ref().filter(|t| t.count.is_some()) else {
                    continue;
                };
                let rec = json!({
                    "tag": r.tag, "schedule": r.schedule, "pair": t.pair, "dir": t.dir,
                    "match": t.matched, "step": t.step, "count": t.count,
                    "verdict": r.verdict.text, "fp": r.fingerprints.fp, "secs": r.secs,
                    "console": r.artifacts.as_ref().and_then(|x| x.consoles.first())
                        .map(|c| dir.join(c).display().to_string()),
                });
                if let Some(o) = rec.as_object() {
                    known.insert(r.schedule.clone(), o.clone());
                }
            }
            continue;
        }
        for line in text.lines() {
            let rec: Value = serde_json::from_str(line).map_err(|e| format!("{path}: {e}"))?;
            if rec.get("count").is_some_and(|c| !c.is_null()) {
                if let Some(o) = rec.as_object() {
                    known.insert(py_str(&rec["schedule"]), o.clone());
                }
            }
        }
    }
    Ok(known)
}

/// A trace run (its record of the trace) as a run of the results format.
fn trace_run(out: &Path, rec: &Map<String, Value>, res: Option<&Res>) -> hydo_results::Run {
    let tag = py_str(&rec["tag"]);
    let text = py_str(&rec["schedule"]);
    let mut run = match res {
        Some(r) => crate::results::explorer_run(out, &tag, &text, r, hydo_results::role::TRACE),
        None => {
            let v = py_str(&rec["verdict"]);
            let mut r = hydo_results::Run::new(
                &text,
                hydo_results::Verdict {
                    kind: crate::results::kind_of(&v),
                    text: v,
                    ..Default::default()
                },
                &py_str(&rec["fp"]),
            );
            r.tag = Some(tag);
            r.role = Some(hydo_results::role::TRACE.into());
            r.secs = rec.get("secs").and_then(Value::as_f64);
            r
        }
    };
    run.trace = Some(hydo_results::Trace {
        pair: py_str(&rec["pair"]),
        dir: py_str(&rec["dir"]),
        matched: rec.get("match").filter(|m| !m.is_null()).map(py_str),
        step: rec["step"].as_i64().unwrap_or(0),
        count: rec.get("count").and_then(Value::as_i64),
        reused: rec.get("reused").and_then(Value::as_bool),
    });
    run
}

fn with_f(base: &S, f: Fc) -> S {
    let mut s = base.clone();
    s.f = Some(f);
    s
}

/// explore.py `freeze_sweep`: trace, then one point per packet.
pub fn freeze_sweep(
    ex: &Ex,
    pool: &Pool,
    a: &Args,
    out: &Path,
    ev: &Events,
) -> Result<Vec<Point>, String> {
    if ex.fc_pairs.is_empty() {
        return Err("explore.py: --sweep freeze needs an image whose manifest lists `freeze` in faults: (manifest_space.py gives the pairs)".into());
    }
    let steps = steps_of(a, ex.norders)?;
    let durs: Vec<i64> = a
        .fc_durs
        .split(',')
        .map(|v| v.trim().parse().map_err(|e| format!("--fc-durs: {e}")))
        .collect::<Result<_, _>>()?;
    let base = sweep_with(ex, a.sweep_with.as_deref())?;
    let streams = streams_of(ex, a)?;
    if streams.is_empty() {
        return Err(
            "explore.py: --sweep freeze: no streams (check --fc-pairs, --fc-dirs, --fc-match)"
                .into(),
        );
    }
    let mut tsteps: BTreeSet<i64> = steps.iter().copied().collect();
    tsteps.extend(steps.iter().map(|s| s + 1).filter(|s| *s < ex.norders));
    let known = known_traces(a)?;
    let (mut items, mut keys, mut trec) =
        (Vec::new(), HashMap::new(), Vec::<Map<String, Value>>::new());
    for (i, (svc, peer, d, m)) in streams.iter().enumerate() {
        for st in &tsteps {
            let t = with_f(
                &base,
                Fc {
                    svc: svc.clone(),
                    dir: *d,
                    peer: peer.clone(),
                    step: *st,
                    n: 0,
                    matched: m.clone(),
                    dur: 1,
                },
            );
            let tag = format!("fctrace-{i}-{st}");
            keys.insert(tag.clone(), (svc.clone(), peer.clone(), *d, m.clone(), *st));
            let text = ex.render(&t);
            if let Some(k) = known.get(&text) {
                let mut r = k.clone();
                r.insert("tag".into(), json!(tag));
                r.insert("reused".into(), json!(true));
                trec.push(r);
            } else {
                items.push((tag, text));
            }
        }
    }
    eprintln!(
        "freeze sweep: {} streams x {} steps = {} trace runs ({} reused from --fc-traces)",
        streams.len(),
        tsteps.len(),
        items.len() + trec.len(),
        trec.len()
    );
    let t0 = Instant::now();
    let res = if items.is_empty() {
        HashMap::new()
    } else {
        pool.run_all(&items, true)
    };
    let mut count: HashMap<TKey, Option<i64>> = HashMap::new();
    for r in &trec {
        let k = keys[&py_str(&r["tag"])].clone();
        count.insert(k, r["count"].as_i64());
    }
    let mut sorted: Vec<(&String, &(String, Res))> = res.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    for (tag, (text, r)) in sorted {
        let (svc, peer, d, m, st) = keys[tag].clone();
        let n = read_text(&out.join("consoles").join(format!("{tag}.log")))
            .and_then(|t| trace_count(&t, st));
        count.insert((svc.clone(), peer.clone(), d, m.clone(), st), n);
        let mut rec = Map::new();
        rec.insert("tag".into(), json!(tag));
        rec.insert("schedule".into(), json!(text));
        rec.insert("pair".into(), json!(format!("{svc}>{peer}")));
        rec.insert("dir".into(), json!(d.to_string()));
        rec.insert("match".into(), json!(m));
        rec.insert("step".into(), json!(st));
        rec.insert("count".into(), json!(n));
        rec.insert("verdict".into(), r["verdict"].clone());
        rec.insert("fp".into(), r["fp"].clone());
        rec.insert("_res".into(), Value::Object(r.clone()));
        trec.push(rec);
    }
    // the trace runs go into the stream (events.jsonl; `--fc-traces` reads them back)
    for rec in &trec {
        let res = rec.get("_res").and_then(Value::as_object);
        ev.write(&trace_run(out, rec, res));
    }
    let missing: Vec<String> = trec
        .iter()
        .filter(|r| r.get("count").is_none_or(Value::is_null))
        .map(|r| py_str(&r["tag"]))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "explore.py: --sweep freeze: no packet count in the console of {} (a trace run that never reached the end of fault injection -- e.g. a power cut in --sweep-with -- or an image without the freeze fault)",
            missing.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    let c = |k: TKey| count[&k].unwrap_or(0);
    let mut points = Vec::new();
    for (svc, peer, d, m) in &streams {
        for st in &steps {
            let key = |s: i64| (svc.clone(), peer.clone(), *d, m.clone(), s);
            let mut n_here = c(key(*st));
            if st + 1 < ex.norders {
                n_here = 0.max(n_here - c(key(st + 1)))
                    + if steps.contains(&(st + 1)) {
                        0
                    } else {
                        a.fc_margin
                    };
                n_here = n_here.min(c(key(*st)));
            }
            let mut n = 1;
            while n <= n_here {
                for dur in &durs {
                    let f = Fc {
                        svc: svc.clone(),
                        dir: *d,
                        peer: peer.clone(),
                        step: *st,
                        n,
                        matched: m.clone(),
                        dur: *dur,
                    };
                    points.push((render_fc(&f)[2..].to_string(), with_f(&base, f)));
                }
                n += a.sweep_every.max(1);
            }
        }
    }
    eprintln!(
        "freeze sweep: traces took {:.0} s; {} points (every {}-th packet, {} duration(s))",
        t0.elapsed().as_secs_f64(),
        points.len(),
        a.sweep_every,
        durs.len()
    );
    Ok(points)
}

// ---------------------------------------------------------------- the adaptive sweep
fn froze_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"hydo-fc: FROZE .* at (req|resp) #(\d+) \((req|resp)(\d+),").expect("p")
    })
}

fn trace_line_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"hydo-fc-trace: (req|resp)(\d+) len=\d+(?: #\d+)? t=(\d+)").expect("p")
    })
}

fn dirword(d: char) -> &'static str {
    if d == '>' {
        "req"
    } else {
        "resp"
    }
}

/// The non-`always` property outcomes of a console, without the sweep's own fault.* records.
fn props_sig(text: &str) -> BTreeSet<String> {
    records(text)
        .into_iter()
        .filter(|r| {
            let t = r.get("t").and_then(Value::as_str).unwrap_or("");
            t != "always"
                && t != "always_or_unreachable"
                && !py_str(r.get("id").unwrap_or(&Value::Null)).starts_with("fault.")
        })
        .map(|r| {
            dumps_line(&json!([
                r.get("t").cloned().unwrap_or(Value::Null),
                r.get("id").cloned().unwrap_or(Value::Null),
                r.get("ok").cloned().unwrap_or(Value::Null)
            ]))
        })
        .collect()
}

type TKey = (String, String, char, Option<String>, i64);
/// A point's windows: (stream, centre n) and the record of how they were found.
type Centres = (Vec<(TKey, i64)>, Option<Value>);
type PKey = (String, char, String, i64, i64, Option<String>, i64);

struct Adaptive<'a> {
    ex: &'a Ex,
    pool: &'a Pool,
    a: &'a Args,
    out: &'a Path,
    base: S,
    durs: Vec<i64>,
    w: i64,
    traces: HashMap<TKey, Map<String, Value>>,
    lines_cache: HashMap<TKey, Vec<(String, i64, f64)>>,
    trecs: Vec<Map<String, Value>>,
    ntrace: usize,
    points: Vec<Point>,
    items: Vec<(String, String)>,
    res: HashMap<String, (String, Res)>,
    by_key: HashMap<PKey, usize>,
    stage: BTreeMap<usize, i64>,
    known: HashMap<String, Map<String, Value>>,
    centre_log: Vec<Value>,
    rounds: Vec<Value>,
    windows: Map<String, Value>,
}

/// What `--fc-adaptive` hands back to the main flow.
pub struct AdaptiveOut {
    pub points: Vec<Point>,
    pub items: Vec<(String, String)>,
    pub res: HashMap<String, (String, Res)>,
    pub info: Map<String, Value>,
    /// The centres, windows, rounds and stages (exploration.json `freeze_sweep`).
    pub doc: Map<String, Value>,
}

impl<'a> Adaptive<'a> {
    fn need_traces(&mut self, keys: &[TKey]) -> Result<(), String> {
        let mut todo = Vec::new();
        let mut tags: Vec<(String, TKey)> = Vec::new();
        for key in keys {
            if self.traces.contains_key(key) || tags.iter().any(|(_, k)| k == key) {
                continue;
            }
            let (svc, peer, d, m, st) = key.clone();
            let t = with_f(
                &self.base,
                Fc {
                    svc,
                    dir: d,
                    peer,
                    step: st,
                    n: 0,
                    matched: m,
                    dur: 1,
                },
            );
            let text = self.ex.render(&t);
            if let Some(k) = self.known.get(&text) {
                if k.get("console")
                    .is_some_and(|c| Path::new(&py_str(c)).exists())
                {
                    let mut rec = k.clone();
                    rec.insert("reused".into(), json!(true));
                    self.traces.insert(key.clone(), rec.clone());
                    self.trecs.push(rec);
                    continue;
                }
            }
            let tag = format!("fctrace-{}", self.ntrace);
            self.ntrace += 1;
            tags.push((tag.clone(), key.clone()));
            todo.push((tag, text));
        }
        if todo.is_empty() {
            return Ok(());
        }
        let res = self.pool.run_all(&todo, true);
        let mut sorted: Vec<(&String, &(String, Res))> = res.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        for (tag, (text, r)) in sorted {
            let key = tags
                .iter()
                .find(|(t, _)| t == tag)
                .map(|x| x.1.clone())
                .expect("a trace tag");
            let (svc, peer, d, m, st) = key.clone();
            let path = self.out.join("consoles").join(format!("{tag}.log"));
            let n = read_text(&path).and_then(|t| trace_count(&t, st)).ok_or_else(|| {
                format!("explore.py: --fc-adaptive: no packet count in the console of {tag} ({text})")
            })?;
            let mut rec = Map::new();
            rec.insert("tag".into(), json!(tag));
            rec.insert("schedule".into(), json!(text));
            rec.insert("pair".into(), json!(format!("{svc}>{peer}")));
            rec.insert("dir".into(), json!(d.to_string()));
            rec.insert("match".into(), json!(m));
            rec.insert("step".into(), json!(st));
            rec.insert("count".into(), json!(n));
            rec.insert("verdict".into(), r["verdict"].clone());
            rec.insert("fp".into(), r["fp"].clone());
            rec.insert("secs".into(), r.get("secs").cloned().unwrap_or(Value::Null));
            rec.insert("console".into(), json!(path.display().to_string()));
            self.traces.insert(key, rec.clone());
            rec.insert("_res".into(), Value::Object(r.clone()));
            self.trecs.push(rec);
        }
        Ok(())
    }

    fn count(&self, key: &TKey) -> i64 {
        self.traces[key]["count"].as_i64().unwrap_or(0)
    }

    fn lines(&mut self, key: &TKey) -> Vec<(String, i64, f64)> {
        if let Some(l) = self.lines_cache.get(key) {
            return l.clone();
        }
        let path = self.traces[key]
            .get("console")
            .map(py_str)
            .unwrap_or_default();
        let text = read_text(Path::new(&path)).unwrap_or_default();
        let l: Vec<(String, i64, f64)> = trace_line_re()
            .captures_iter(&text)
            .map(|c| {
                (
                    c[1].to_string(),
                    c[2].parse().unwrap_or(0),
                    c[3].parse().unwrap_or(0.0),
                )
            })
            .collect();
        self.lines_cache.insert(key.clone(), l.clone());
        l
    }

    fn add_point(&mut self, f: Fc, stage: i64) -> Option<(String, String)> {
        let key: PKey = (
            f.svc.clone(),
            f.dir,
            f.peer.clone(),
            f.step,
            f.n,
            f.matched.clone(),
            f.dur,
        );
        if self.by_key.contains_key(&key) {
            return None;
        }
        let i = self.points.len() + 1;
        let s = with_f(&self.base, f.clone());
        self.points
            .push((render_fc(&f)[2..].to_string(), s.clone()));
        self.by_key.insert(key, i);
        self.stage.insert(i, stage);
        let text = self.ex.render(&self.ex.add_point_streams(s, i as u64));
        let it = (format!("seed-{i}"), text);
        self.items.push(it.clone());
        Some(it)
    }

    fn run(&mut self, items: &[(String, String)]) {
        if !items.is_empty() {
            let r = self.pool.run_all(items, false);
            self.res.extend(r);
        }
    }

    fn failed(&self, i: usize) -> bool {
        let tag = format!("seed-{i}");
        let (_, r) = &self.res[&tag];
        verdict_kind(&py_str(&r["verdict"])) != "ok"
            || !assert_failures(self.out, &tag, r).is_empty()
    }

    fn console(&self, i: usize) -> String {
        read_text(&self.out.join("consoles").join(format!("seed-{i}.log"))).unwrap_or_default()
    }

    /// Guest uptime of the k-th `dw` packet of trace tkey (extrapolated past the visible lines);
    /// the flag says whether it is a value read from the trace (an int there) or computed (a float).
    fn own_t(&mut self, tkey: &TKey, dw: &str, k: i64) -> Option<(f64, bool)> {
        let l: Vec<(i64, f64)> = self
            .lines(tkey)
            .into_iter()
            .filter(|x| x.0 == dw)
            .map(|x| (x.1, x.2))
            .collect();
        if let Some((_, t)) = l.iter().find(|(kk, _)| *kk == k) {
            return Some((*t, true));
        }
        if l.is_empty() {
            return None;
        }
        if k < l[0].0 {
            return Some((l[0].1, true));
        }
        let tail = &l[l.len().saturating_sub(20)..];
        let (f, la) = (tail[0], tail[tail.len() - 1]);
        let rate = (la.1 - f.1) / ((la.0 - f.0).max(1) as f64);
        Some((la.1 + (k - la.0) as f64 * rate, false))
    }

    fn opposite(&mut self, tkey: &TKey, dw: &str, k: i64, ow: &str) -> i64 {
        let l = self.lines(tkey);
        let mut last: HashMap<&str, i64> = HashMap::from([("req", 0), ("resp", 0)]);
        for (w, kk, _) in &l {
            if w == dw && *kk == k {
                return 1.max(last[ow]);
            }
            last.insert(if w == "req" { "req" } else { "resp" }, *kk);
        }
        if l.is_empty() {
            k
        } else {
            1.max(k + last[ow] - last[dw])
        }
    }

    fn at_time(&mut self, tkey: &TKey, dw: &str, t: Option<f64>, n_max: i64) -> Option<i64> {
        let l: Vec<(i64, f64)> = self
            .lines(tkey)
            .into_iter()
            .filter(|x| x.0 == dw)
            .map(|x| (x.1, x.2))
            .collect();
        let t = t?;
        if l.is_empty() {
            return None;
        }
        let last = l[l.len() - 1];
        if t > last.1 && l.len() >= 2 {
            let tail = &l[l.len().saturating_sub(20)..];
            let f = tail[0];
            let rate = (last.0 - f.0) as f64 / (last.1 - f.1).max(1.0);
            let v = (last.0 as f64 + (t - last.1) * rate).round_ties_even() as i64;
            return Some(1.max(n_max.min(v)));
        }
        let mut best = l[0];
        for x in &l {
            let (dx, db) = ((x.1 - t).abs(), (best.1 - t).abs());
            if dx < db || (dx == db && x.0 < best.0) {
                best = *x;
            }
        }
        Some(best.0)
    }

    fn allowed_pairs(&self) -> Vec<(String, String)> {
        let pairs = self.ex.fc_pairs.clone();
        match want_pairs(self.a) {
            Some(want) => pairs.into_iter().filter(|p| want.contains(p)).collect(),
            None => pairs,
        }
    }

    fn centre_windows(&mut self, i: usize, why: &str) -> Result<Centres, String> {
        let (label, s) = self.points[i - 1].clone();
        let f = s.f.clone().expect("a freeze point");
        let cons = self.console(i);
        let Some(mm) = froze_re().captures(&cons) else {
            eprintln!("fc-adaptive: point {label}: the freeze never fired, no centre");
            return Ok((Vec::new(), None));
        };
        let k: i64 = mm[4].parse().unwrap_or(0);
        let dw = mm[3].to_string();
        let d = if dw == "req" { '>' } else { '<' };
        let mut pairs = vec![(f.svc.clone(), f.peer.clone())];
        if self.a.fc_links == "svc" {
            pairs.extend(
                self.allowed_pairs()
                    .into_iter()
                    .filter(|p| p.0 == f.svc && *p != (f.svc.clone(), f.peer.clone())),
            );
        }
        let dirs = dirs_of(self.a);
        let keys: Vec<TKey> = pairs
            .iter()
            .flat_map(|p| {
                dirs.iter()
                    .map(move |x| (p.0.clone(), p.1.clone(), *x, None, f.step))
            })
            .collect();
        let own: TKey = (f.svc.clone(), f.peer.clone(), d, None, f.step);
        let mut need = keys.clone();
        need.push(own.clone());
        self.need_traces(&need)?;
        let t = self.own_t(&own, &dw, k);
        let mut out: Vec<(TKey, i64)> = Vec::new();
        for key in &keys {
            let (q, qd) = (key.1.clone(), key.2);
            let ow = dirword(qd);
            let n = self.count(key);
            if n == 0 {
                continue;
            }
            let c = if q == f.peer && qd == d {
                k
            } else if q == f.peer {
                self.opposite(&own, &dw, k, ow)
            } else {
                match self.at_time(key, ow, t.map(|x| x.0), n) {
                    Some(c) => c,
                    None => 1.max(
                        (k as f64 * n as f64 / self.count(&own).max(1) as f64).round_ties_even()
                            as i64,
                    ),
                }
            };
            match out.iter_mut().find(|(kk, _)| kk == key) {
                Some(e) => e.1 = 1.max(n.min(c)),
                None => out.push((key.clone(), 1.max(n.min(c)))),
            }
        }
        let mut centres = Map::new();
        for (kk, c) in &out {
            centres.insert(format!("{}{}{}@{}", kk.0, kk.2, kk.1, kk.4), json!(c));
        }
        let info = json!({"point": label, "why": why, "froze": format!("{dw}{k}"),
                          "t": t.map(|(x, exact)| if exact { json!(x as i64) } else { crate::py::fl(x) }),
                          "centres": centres});
        Ok((out, Some(info)))
    }

    fn stage1(&mut self) -> Result<Vec<usize>, String> {
        let (a, ex) = (self.a, self.ex);
        let steps = steps_of(a, ex.norders)?;
        let mut modes: Vec<String> = a.fc_stage1.split(',').map(str::to_string).collect();
        if modes == ["auto"] {
            modes = vec![if ex.fc_match.is_empty() {
                "stride"
            } else {
                "manifest"
            }
            .to_string()];
        }
        let dirs = dirs_of(a);
        let mut streams: Vec<(String, String, char, Option<String>, i64)> = Vec::new();
        for (svc, peer) in self.allowed_pairs() {
            for mode in &modes {
                match mode.as_str() {
                    "manifest" => {
                        for (m, md) in &ex.fc_match {
                            let ds: Vec<char> = md.map(|x| vec![x]).unwrap_or_else(|| dirs.clone());
                            for x in ds.into_iter().filter(|x| dirs.contains(x)) {
                                streams.push((svc.clone(), peer.clone(), x, Some(m.clone()), 1));
                            }
                        }
                    }
                    "stride" => {
                        for x in &dirs {
                            streams.push((svc.clone(), peer.clone(), *x, None, a.fc_stride));
                        }
                    }
                    other => {
                        return Err(format!(
                            "explore.py: --fc-stage1 {}: expected auto, manifest, stride",
                            repr(other)
                        ))
                    }
                }
            }
        }
        if streams.is_empty() {
            return Err("explore.py: --fc-adaptive: no stage-1 streams (check --fc-pairs, --fc-dirs, --fc-stage1; stage 1 'manifest' needs freeze_match: in the manifest)".into());
        }
        let mut tsteps: BTreeSet<i64> = steps.iter().copied().collect();
        tsteps.extend(steps.iter().map(|s| s + 1).filter(|s| *s < ex.norders));
        let keys: Vec<TKey> = streams
            .iter()
            .flat_map(|(svc, peer, d, m, _)| {
                tsteps
                    .iter()
                    .map(move |st| (svc.clone(), peer.clone(), *d, m.clone(), *st))
            })
            .collect();
        eprintln!(
            "fc-adaptive: stage 1 ({}): {} streams x {} steps, {} trace runs",
            modes.join(","),
            streams.len(),
            tsteps.len(),
            keys.len()
        );
        self.need_traces(&keys)?;
        let mut items = Vec::new();
        for (svc, peer, d, m, every) in &streams {
            for st in &steps {
                let key = (svc.clone(), peer.clone(), *d, m.clone(), *st);
                let mut n_here = self.count(&key);
                if st + 1 < ex.norders {
                    let next = (svc.clone(), peer.clone(), *d, m.clone(), st + 1);
                    n_here = 0.max(n_here - self.count(&next))
                        + if steps.contains(&(st + 1)) {
                            0
                        } else {
                            a.fc_margin
                        };
                    n_here = n_here.min(self.count(&key));
                }
                let mut n = 1;
                while n <= n_here {
                    for dur in self.durs.clone() {
                        let f = Fc {
                            svc: svc.clone(),
                            dir: *d,
                            peer: peer.clone(),
                            step: *st,
                            n,
                            matched: m.clone(),
                            dur,
                        };
                        if let Some(it) = self.add_point(f, 1) {
                            items.push(it);
                        }
                    }
                    n += (*every).max(1);
                }
            }
        }
        eprintln!("fc-adaptive: stage 1: {} points", items.len());
        self.run(&items);
        Ok(items
            .iter()
            .map(|(t, _)| t[5..].parse().unwrap_or(0))
            .collect())
    }

    fn centres_of(&mut self, seeds: &[usize]) -> Vec<(usize, &'static str)> {
        let want: BTreeSet<String> = self.a.fc_centres.split(',').map(str::to_string).collect();
        let mut out = Vec::new();
        for &i in seeds {
            if self.failed(i) {
                out.push((i, "fail"));
                continue;
            }
            if !want.contains("props") && !want.contains("state") {
                continue;
            }
            let f = self.points[i - 1].1.f.clone().expect("a freeze point");
            let Some(tr) = self.traces.get(&(
                f.svc.clone(),
                f.peer.clone(),
                f.dir,
                f.matched.clone(),
                f.step,
            )) else {
                continue;
            };
            let base = read_text(Path::new(
                &tr.get("console").map(py_str).unwrap_or_default(),
            ))
            .unwrap_or_default();
            let tr_verdict = tr["verdict"].clone();
            let mine = self.console(i);
            if want.contains("props") && props_sig(&mine) != props_sig(&base) {
                out.push((i, "props"));
            } else if want.contains("state")
                && self.res[&format!("seed-{i}")].1["verdict"] != tr_verdict
            {
                out.push((i, "state"));
            }
        }
        out
    }

    fn stage2(&mut self, seeds1: &[usize]) -> Result<(), String> {
        let cents = self.centres_of(seeds1);
        eprintln!(
            "fc-adaptive: stage 2: {} centres ({})",
            cents.len(),
            cents
                .iter()
                .map(|(i, w)| format!("{}[{}]", self.points[i - 1].0, w))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut windows: Vec<(TKey, Vec<[i64; 2]>)> = Vec::new();
        for (i, why) in cents {
            let (cs, info) = self.centre_windows(i, why)?;
            if let Some(info) = info {
                self.centre_log.push(info);
            }
            for (key, c) in cs {
                let n = self.count(&key);
                let iv = [1.max(c - self.w), n.min(c + self.w)];
                match windows.iter_mut().find(|(k, _)| *k == key) {
                    Some(e) => e.1.push(iv),
                    None => windows.push((key, vec![iv])),
                }
            }
        }
        let mut rnd = 0;
        loop {
            for (_, ivs) in windows.iter_mut() {
                *ivs = merge(ivs);
            }
            let mut order: Vec<usize> = (0..windows.len()).collect();
            order.sort_by_key(|i| tkey_str(&windows[*i].0));
            let mut items = Vec::new();
            for wi in order {
                let (key, ivs) = windows[wi].clone();
                let (svc, peer, d, _, st) = key;
                for [lo, hi] in ivs {
                    for n in lo..=hi {
                        for dur in self.durs.clone() {
                            let f = Fc {
                                svc: svc.clone(),
                                dir: d,
                                peer: peer.clone(),
                                step: st,
                                n,
                                matched: None,
                                dur,
                            };
                            if let Some(it) = self.add_point(f, 2) {
                                items.push(it);
                            }
                        }
                    }
                }
            }
            if items.is_empty() {
                break;
            }
            rnd += 1;
            let t0 = Instant::now();
            self.run(&items);
            let nf = items
                .iter()
                .filter(|(t, _)| self.failed(t[5..].parse().unwrap_or(0)))
                .count();
            self.rounds
                .push(json!({"round": rnd, "points": items.len(), "failures": nf,
                                    "secs": crate::py::fl(round1(t0.elapsed().as_secs_f64()))}));
            eprintln!(
                "fc-adaptive: stage 2 round {rnd}: {} points, {nf} failures",
                items.len()
            );
            let mut changed = false;
            #[allow(clippy::needless_range_loop)]
            for wi in 0..windows.len() {
                let (svc, peer, d, _, st) = windows[wi].0.clone();
                let n_all = self.count(&windows[wi].0);
                for j in 0..windows[wi].1.len() {
                    let iv = windows[wi].1[j];
                    let fails: Vec<i64> = (iv[0]..=iv[1])
                        .filter(|n| {
                            self.durs.iter().any(|dur| {
                                let k = (svc.clone(), d, peer.clone(), st, *n, None, *dur);
                                self.failed(self.by_key[&k])
                            })
                        })
                        .collect();
                    let (Some(first), Some(last)) = (fails.first(), fails.last()) else {
                        continue;
                    };
                    let (lo, hi) = (1.max(first - self.w), n_all.min(last + self.w));
                    if lo < iv[0] || hi > iv[1] {
                        windows[wi].1[j] = [lo.min(iv[0]), hi.max(iv[1])];
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for (k, v) in &windows {
            self.windows.insert(
                format!("{}{}{}@{}", k.0, k.2, k.1, k.4),
                json!(v.iter().map(|iv| vec![iv[0], iv[1]]).collect::<Vec<_>>()),
            );
        }
        Ok(())
    }
}

/// `str(key)` of explore.py's stream key tuple, for its sort order.
fn tkey_str(k: &TKey) -> String {
    format!(
        "({}, {}, {}, {}, {})",
        repr(&k.0),
        repr(&k.1),
        repr(&k.2.to_string()),
        k.3.as_ref()
            .map(|m| repr(m))
            .unwrap_or_else(|| "None".into()),
        k.4
    )
}

fn merge(ivs: &[[i64; 2]]) -> Vec<[i64; 2]> {
    let mut v = ivs.to_vec();
    v.sort();
    let mut out: Vec<[i64; 2]> = Vec::new();
    for [lo, hi] in v {
        match out.last_mut() {
            Some(l) if lo <= l[1] + 1 => l[1] = l[1].max(hi),
            _ => out.push([lo, hi]),
        }
    }
    out
}

/// fc_adaptive.run: `--sweep freeze --fc-adaptive`.
pub fn adaptive(
    ex: &Ex,
    pool: &Pool,
    a: &Args,
    out: &Path,
    ev: &Events,
) -> Result<AdaptiveOut, String> {
    if ex.fc_pairs.is_empty() {
        return Err("explore.py: --sweep freeze needs an image whose manifest lists `freeze` in faults: (manifest_space.py gives the pairs)".into());
    }
    if a.sweep_dry {
        return Err("explore.py: --fc-adaptive runs its points as it goes; --sweep-dry is for the full sweep".into());
    }
    let mut ad = Adaptive {
        ex,
        pool,
        a,
        out,
        base: sweep_with(ex, a.sweep_with.as_deref())?,
        durs: a
            .fc_durs
            .split(',')
            .map(|v| v.trim().parse().map_err(|e| format!("--fc-durs: {e}")))
            .collect::<Result<_, _>>()?,
        w: a.fc_window,
        traces: HashMap::new(),
        lines_cache: HashMap::new(),
        trecs: Vec::new(),
        ntrace: 0,
        points: Vec::new(),
        items: Vec::new(),
        res: HashMap::new(),
        by_key: HashMap::new(),
        stage: BTreeMap::new(),
        known: known_traces(a)?,
        centre_log: Vec::new(),
        rounds: Vec::new(),
        windows: Map::new(),
    };
    let t0 = Instant::now();
    let s1 = ad.stage1()?;
    let t1 = Instant::now();
    ad.stage2(&s1)?;
    let t2 = Instant::now();
    // the trace runs go into the stream (events.jsonl; `--fc-traces` reads them back)
    for rec in &ad.trecs {
        let res = rec.get("_res").and_then(Value::as_object);
        ev.write(&trace_run(out, rec, res));
    }
    let n1 = ad.stage.values().filter(|v| **v == 1).count();
    let f1 = ad
        .stage
        .iter()
        .filter(|(i, v)| **v == 1 && ad.failed(**i))
        .count();
    let f2 = ad
        .stage
        .iter()
        .filter(|(i, v)| **v == 2 && ad.failed(**i))
        .count();
    let mut info = Map::new();
    info.insert("stage1_points".into(), json!(n1));
    info.insert("stage2_points".into(), json!(ad.points.len() - n1));
    info.insert("trace_runs".into(), json!(ad.ntrace));
    info.insert("stage1_failures".into(), json!(f1));
    info.insert("stage2_failures".into(), json!(f2));
    info.insert(
        "stage1_wall_s".into(),
        crate::py::fl(round1((t1 - t0).as_secs_f64())),
    );
    info.insert(
        "stage2_wall_s".into(),
        crate::py::fl(round1((t2 - t1).as_secs_f64())),
    );
    info.insert("fc_window".into(), json!(ad.w));
    info.insert("fc_links".into(), json!(a.fc_links));
    info.insert("fc_centres".into(), json!(a.fc_centres));
    let mut stage = Map::new();
    for (i, v) in &ad.stage {
        stage.insert(format!("seed-{i}"), json!(v));
    }
    // the rest of the record goes into exploration.json (`freeze_sweep`, after its summary)
    let mut doc = Map::new();
    doc.insert("centres".into(), Value::Array(ad.centre_log.clone()));
    doc.insert("windows".into(), Value::Object(ad.windows.clone()));
    doc.insert("rounds".into(), Value::Array(ad.rounds.clone()));
    doc.insert("stage".into(), Value::Object(stage));
    eprintln!(
        "fc-adaptive: done: {}",
        dumps_line(&Value::Object(info.clone()))
    );
    Ok(AdaptiveOut {
        points: ad.points,
        items: ad.items,
        res: ad.res,
        info,
        doc,
    })
}
