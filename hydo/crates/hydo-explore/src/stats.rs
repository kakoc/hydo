//! An exploration's statistics (Milestone 336): `stats.json` beside `exploration.json`, and its
//! words for `hydo stats` and `hydo report`.
//!
//! Four parts, each a host-side record of the runs, so nothing here changes a run, a fingerprint
//! or a verdict:
//!
//! - the curve of new behaviour: after each first run in seed order, how many distinct
//!   fingerprints, verdicts, property outcomes, error kinds and error paths the runs showed so
//!   far, and one judgement of whether the last runs still showed anything new;
//! - the cost: the engine's seconds by the run's role, the engine's clock, the host CPU, the
//!   largest resident memory, and each worker's time busy, starting its engine, waiting and idle;
//! - the coverage of the fault space: which fault kinds the schedules drew, on which targets and
//!   at which workload steps, against the manifest's `faults:` and services;
//! - an estimate of how many first runs fit in a time budget with N workers or N shards on a
//!   machine like this one.
//!
//! `hydo explore --merge` makes the statistics of the whole range from its parts' files
//! ([`merge`]): the first runs in seed order with what each showed first, so the curve is the
//! one an exploration of the whole range gives.

use crate::pool::Ledger;
use crate::space::{Drawn, Space};
use hydo_results::{
    trend, Engine, StatsCost, StatsCoverage, StatsCurve, StatsDoc, StatsEstimate, StatsFit,
    StatsKind, StatsNever, StatsPoint, StatsRun, StatsSpread, StatsWorker,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// The measures of the curve, in their order.
pub const FINGERPRINTS: &str = "fingerprints";
pub const VERDICTS: &str = "verdicts";
pub const PROPERTIES: &str = "properties";
pub const ERROR_KINDS: &str = "error_kinds";
pub const ERROR_PATHS: &str = "error_paths";

/// The fewest first runs the judgement looks at.
pub const MIN_WINDOW: u64 = 10;

/// The most evenly spaced points of the curve (besides the powers of two and the last).
const POINTS: u64 = 64;

/// One first run as the explorer saw it, everything it showed (not only what was new).
#[derive(Clone, Debug, Default)]
pub struct First {
    pub seed: i64,
    pub schedule: String,
    /// The verdict line, after the explorer's assertion check.
    pub verdict: String,
    pub fp: String,
    pub refused: bool,
    pub cached: bool,
    pub vm_s: f64,
    pub job_s: Option<f64>,
    pub clock: Option<u64>,
    pub properties: Vec<String>,
    pub error_kinds: Vec<String>,
    pub error_paths: Vec<u32>,
}

/// What [`build`] needs besides the first runs.
#[derive(Clone, Debug, Default)]
pub struct Input {
    pub hydo_version: Option<String>,
    pub epoch: Option<u64>,
    pub image: String,
    pub engine: Engine,
    pub strategy: String,
    pub seeds: Option<String>,
    /// The image is built with `errors:` (the runs record error kinds), and the engine watched
    /// the error paths.
    pub errlog: bool,
    pub paths: bool,
    /// The engine seconds of the other runs, by role (`rerun`, `shrink`, `error-shrink`): (runs,
    /// seconds).
    pub other_runs: BTreeMap<String, (u64, f64)>,
    pub wall_s: f64,
    /// The time the first runs' batch took.
    pub sweep_wall_s: f64,
    pub clock_unit: Option<String>,
    pub usage: Option<Usage>,
    pub guest_ram_mib: Option<u64>,
    /// About how many workers this machine fits for this guest now.
    pub machine_workers: Option<u64>,
}

/// The host's process accounting at the exploration's end (`getrusage`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub cpu_s: f64,
    pub self_peak_mib: u64,
    pub children_peak_mib: u64,
}

/// The explorer's own process and the engine processes it waited for: CPU seconds (user and
/// system) and the largest resident memory of each. None when the accounting cannot be read.
pub fn usage() -> Option<Usage> {
    fn one(who: libc::c_int) -> Option<libc::rusage> {
        // SAFETY: getrusage writes one struct rusage into the zeroed value it is given.
        let mut r: libc::rusage = unsafe { std::mem::zeroed() };
        (unsafe { libc::getrusage(who, &mut r) } == 0).then_some(r)
    }
    let s = one(libc::RUSAGE_SELF)?;
    let c = one(libc::RUSAGE_CHILDREN)?;
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    // ru_maxrss: bytes on macOS, KiB on Linux
    let mib = |x: libc::c_long| -> u64 {
        let x = x.max(0) as u64;
        if cfg!(target_os = "macos") {
            x >> 20
        } else {
            x >> 10
        }
    };
    Some(Usage {
        cpu_s: secs(s.ru_utime) + secs(s.ru_stime) + secs(c.ru_utime) + secs(c.ru_stime),
        self_peak_mib: mib(s.ru_maxrss),
        children_peak_mib: mib(c.ru_maxrss),
    })
}

/// How far the engine's clock moved in a run's control transcripts (`<tag>.control.jsonl` and
/// `<tag>.control.recovery.jsonl` in `dir`): the last stop's `at` less the first's, summed over
/// the conversations. None when the run left no transcript with a stop.
pub fn clock_of(dir: &std::path::Path, tag: &str) -> Option<u64> {
    let mut total = None;
    for f in [
        format!("{tag}.control.jsonl"),
        format!("{tag}.control.recovery.jsonl"),
    ] {
        let Ok(text) = std::fs::read_to_string(dir.join(f)) else {
            continue;
        };
        let (mut first, mut last) = (None, None);
        for l in text.lines().filter(|l| l.contains("\"t\":\"stop\"")) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else {
                continue;
            };
            if let Some(at) = v.get("at").and_then(serde_json::Value::as_u64) {
                first.get_or_insert(at);
                last = Some(at);
            }
        }
        if let (Some(a), Some(b)) = (first, last) {
            total = Some(total.unwrap_or(0) + b.saturating_sub(a));
        }
    }
    total
}

/// The outcome of each property in one run's merged consoles: `<id>: failed` for an `always`,
/// `always_or_unreachable` or `eventually` property with a failed evaluation and for a reached
/// `unreachable`, `<id>: held` for one whose evaluations all held, `<id>: reached` for a
/// `sometimes` or `reachable` property with a passing evaluation. Sorted, each once.
pub fn property_outcomes(text: &str) -> Vec<String> {
    let mut by: BTreeMap<String, &'static str> = BTreeMap::new();
    for rec in crate::report::records(text) {
        let t = rec
            .get("t")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let Some(id) = rec.get("id").map(crate::py::py_str) else {
            continue;
        };
        let ok = rec.get("ok").and_then(serde_json::Value::as_bool);
        let now = match (t, ok) {
            ("always" | "always_or_unreachable" | "eventually", Some(false))
            | ("unreachable", _) => "failed",
            ("always" | "always_or_unreachable" | "eventually", Some(true)) => "held",
            ("sometimes" | "reachable", Some(true)) => "reached",
            _ => continue,
        };
        let e = by.entry(id).or_insert(now);
        if now == "failed" || (*e == "held" && now == "reached") {
            *e = now;
        }
    }
    by.into_iter().map(|(id, o)| format!("{id}: {o}")).collect()
}

/// A verdict's key in the curve: its kind and signature.
pub fn verdict_key(v: &str) -> String {
    format!(
        "{}: {}",
        crate::report::verdict_kind(v),
        crate::report::sig_key(v)
    )
}

/// The first runs with what each showed first, in seed order.
pub fn mark_new(firsts: &[First]) -> Vec<StatsRun> {
    let mut seen: [HashSet<String>; 4] = Default::default();
    let mut paths: HashSet<u32> = HashSet::new();
    let mut out = Vec::new();
    for f in firsts {
        let mut r = StatsRun {
            seed: f.seed,
            vm_s: round3(f.vm_s),
            job_s: f.job_s.map(round3),
            clock: f.clock,
            refused: f.refused,
            ..StatsRun::default()
        };
        if !f.refused {
            let mut fresh = |i: usize, xs: &[String]| -> Vec<String> {
                let mut n = Vec::new();
                for x in xs {
                    if seen[i].insert(x.clone()) {
                        n.push(x.clone());
                    }
                }
                n
            };
            r.fingerprint = fresh(0, std::slice::from_ref(&f.fp)).pop();
            r.verdicts = fresh(1, &[verdict_key(&f.verdict)]);
            r.properties = fresh(2, &f.properties);
            r.error_kinds = fresh(3, &f.error_kinds);
            r.error_paths = f
                .error_paths
                .iter()
                .copied()
                .filter(|p| paths.insert(*p))
                .collect();
        }
        out.push(r);
    }
    out
}

/// Runs whose new keys were found per part, in seed order: each key kept only at its first run
/// (what [`mark_new`] gives for one exploration of the whole range).
fn renew(runs: &mut [StatsRun]) {
    let mut seen: [HashSet<String>; 4] = Default::default();
    let mut paths: HashSet<u32> = HashSet::new();
    for r in runs {
        if let Some(fp) = r.fingerprint.take() {
            if seen[0].insert(fp.clone()) {
                r.fingerprint = Some(fp);
            }
        }
        r.verdicts.retain(|x| seen[1].insert(x.clone()));
        r.properties.retain(|x| seen[2].insert(x.clone()));
        r.error_kinds.retain(|x| seen[3].insert(x.clone()));
        r.error_paths.retain(|p| paths.insert(*p));
    }
}

fn new_of(r: &StatsRun, m: &str) -> usize {
    match m {
        FINGERPRINTS => r.fingerprint.is_some() as usize,
        VERDICTS => r.verdicts.len(),
        PROPERTIES => r.properties.len(),
        ERROR_KINDS => r.error_kinds.len(),
        ERROR_PATHS => r.error_paths.len(),
        _ => 0,
    }
}

/// A measure in words (plural), for the judgement.
pub fn measure_words(m: &str) -> &'static str {
    match m {
        FINGERPRINTS => "run fingerprints",
        VERDICTS => "verdicts",
        PROPERTIES => "property outcomes",
        ERROR_KINDS => "error kinds",
        ERROR_PATHS => "error paths",
        _ => "things",
    }
}

fn one_word(m: &str) -> &'static str {
    match m {
        FINGERPRINTS => "run fingerprint",
        VERDICTS => "verdict",
        PROPERTIES => "property outcome",
        ERROR_KINDS => "error kind",
        ERROR_PATHS => "error path",
        _ => "thing",
    }
}

/// The judgement's window for `total` first runs: a quarter of them, at least [`MIN_WINDOW`].
pub fn window_of(total: u64) -> u64 {
    MIN_WINDOW.max(total.div_ceil(4))
}

/// The curve of the runs (each with its new keys marked) over the measures recorded.
pub fn curve(runs: &[StatsRun], measures: &[String]) -> StatsCurve {
    let total = runs.len() as u64;
    let step = total.div_ceil(POINTS).max(1);
    let mut counts: BTreeMap<String, u64> = measures.iter().map(|m| (m.clone(), 0)).collect();
    let mut grew_at: BTreeMap<String, Vec<u64>> = measures
        .iter()
        .filter(|m| *m != FINGERPRINTS)
        .map(|m| (m.clone(), Vec::new()))
        .collect();
    let mut points = Vec::new();
    for (i, r) in runs.iter().enumerate() {
        let n = i as u64 + 1;
        for m in measures {
            let k = new_of(r, m) as u64;
            *counts.get_mut(m).expect("a measure") += k;
            if k > 0 {
                if let Some(g) = grew_at.get_mut(m) {
                    g.push(n);
                }
            }
        }
        if n.is_power_of_two() || n.is_multiple_of(step) || n == total {
            points.push(StatsPoint {
                runs: n,
                counts: counts.clone(),
            });
        }
    }
    let window = window_of(total);
    let last: Option<(u64, &str)> = grew_at
        .iter()
        .filter_map(|(m, g)| g.last().map(|n| (*n, m.as_str())))
        .max();
    let judged: Vec<&str> = measures
        .iter()
        .filter(|m| *m != FINGERPRINTS)
        .map(String::as_str)
        .collect();
    let (tr, judgement) = if total < MIN_WINDOW {
        (
            trend::TOO_FEW,
            format!(
                "Too few runs to judge: {total} run{}; the judgement needs at least {MIN_WINDOW}.",
                if total == 1 { "" } else { "s" }
            ),
        )
    } else if last.is_some_and(|(n, _)| n > total - window) {
        let recent: Vec<String> = grew_at
            .iter()
            .filter_map(|(m, g)| {
                let k = g.iter().filter(|n| **n > total - window).count();
                (k > 0).then(|| {
                    format!(
                        "{k} new {}",
                        if k == 1 {
                            one_word(m)
                        } else {
                            measure_words(m)
                        }
                    )
                })
            })
            .collect();
        let (n, _) = last.expect("a growth");
        (
            trend::GROWING,
            format!(
                "Still finding new behaviour: the last {window} runs showed {}, the last \
                 at run {n} of {total}.",
                hydo_schedule::describe::and_list(&recent)
            ),
        )
    } else {
        let none = hydo_schedule::describe::and_list(
            &judged
                .iter()
                .map(|m| one_word(m).to_string())
                .collect::<Vec<_>>(),
        )
        .replace(" and ", " or ");
        (
            trend::FLAT,
            match last {
                Some((n, _)) => format!(
                    "Flat over the last {window} runs: no new {none} since run {n} of \
                     {total}."
                ),
                None => format!("Flat: every run showed the first run's {none}."),
            },
        )
    };
    StatsCurve {
        measures: measures.to_vec(),
        points,
        grew_at,
        window,
        trend: tr.to_string(),
        judgement,
    }
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn spread(xs: &[f64]) -> StatsSpread {
    if xs.is_empty() {
        return StatsSpread::default();
    }
    let mut s = xs.to_vec();
    s.sort_by(f64::total_cmp);
    let at = |q: f64| s[((s.len() - 1) as f64 * q).round() as usize];
    StatsSpread {
        mean: round3(s.iter().sum::<f64>() / s.len() as f64),
        median: round3(at(0.5)),
        p90: round3(at(0.9)),
        max: round3(*s.last().expect("one")),
    }
}

/// A job's role by its tag.
pub fn role_of_tag(tag: &str) -> &'static str {
    if tag.starts_with("seed-") {
        "first"
    } else if tag.starts_with("repro") {
        "rerun"
    } else if tag.starts_with("errshrink") || tag.starts_with("errmin") {
        "error-shrink"
    } else if tag.starts_with("shrink") || tag.starts_with("min") {
        "shrink"
    } else {
        "other"
    }
}

/// The workers' time from the pool's ledger.
pub fn workers_of(l: &Ledger) -> Vec<StatsWorker> {
    l.workers
        .iter()
        .enumerate()
        .map(|(i, w)| StatsWorker {
            worker: i as u64,
            part: None,
            jobs: w.jobs,
            restarts: w.restarts,
            busy_s: round1(w.busy_s),
            starting_s: round1(w.starting_s),
            waiting_s: round1(w.waiting_s),
            idle_s: round1((w.life_s - w.busy_s - w.starting_s - w.waiting_s).max(0.0)),
        })
        .collect()
}

/// The share of the busy seconds that went to re-runs and shrinks, from the jobs' seconds.
pub fn shrink_share(jobs: &BTreeMap<String, f64>) -> f64 {
    let all: f64 = jobs.values().sum();
    if all <= 0.0 {
        return 0.0;
    }
    let s: f64 = jobs
        .iter()
        .filter(|(r, _)| matches!(r.as_str(), "rerun" | "shrink" | "error-shrink"))
        .map(|x| x.1)
        .sum();
    round3(s / all)
}

/// The first runs that fit in `budget_s` with `workers` workers on each of `shards` machines like
/// this one ([`StatsEstimate`]'s rule). The count of `seeds_from: history` in `hydo ci run` is
/// this rule with one worker's measured seconds per seed and no start or shrink share.
pub fn fit(budget_s: f64, e: &StatsEstimate, workers: u64, shards: u64) -> u64 {
    fit_count(
        (budget_s - e.starting_s).max(0.0) * workers.max(1) as f64 * (1.0 - e.shrink_share),
        e.job_s,
    ) * shards.max(1)
}

/// How many runs of `per_s` seconds each fit in `left_s` seconds; 0 when `per_s` is not
/// positive.
pub fn fit_count(left_s: f64, per_s: f64) -> u64 {
    if per_s > 0.0 && left_s > 0.0 {
        (left_s / per_s).floor() as u64
    } else {
        0
    }
}

/// The budgets [`build`] works out: an hour and eight hours on the exploration's workers, an
/// hour on 4 and 20 shards, and an hour on the workers this machine fits.
fn fits(e: &StatsEstimate) -> Vec<StatsFit> {
    let mut v: Vec<(f64, u64, u64)> = vec![
        (3600.0, e.workers, 1),
        (8.0 * 3600.0, e.workers, 1),
        (3600.0, e.workers, 4),
        (3600.0, e.workers, 20),
    ];
    if let Some(m) = e.machine_workers.filter(|m| *m != e.workers) {
        v.insert(1, (3600.0, m, 1));
    }
    v.into_iter()
        .map(|(b, w, s)| StatsFit {
            budget_s: b,
            workers: w,
            shards: s,
            runs: fit(b, e, w, s),
        })
        .collect()
}

/// A fault kind in a few words.
pub fn kind_words(k: &str) -> &'static str {
    match k {
        "kill" => "crashes",
        "pause" => "pauses",
        "network" => "network faults",
        "oneway" => "one-way network faults",
        "cut" => "power cuts",
        "clock" => "clock jumps",
        "disk" => "disk faults",
        "freeze" => "freezes at a packet",
        "protocol" => "faults of the outside services",
        "corrupt" => "damaged files",
        "powerloss" => "power losses of one service",
        "upgrade" => "upgrades",
        "block" => "faults of the disk's blocks",
        "timing" => "timing changes",
        "setting" => "settings",
        "reaction" => "reactions to events",
        _ => "other faults",
    }
}

/// The fault tokens of a schedule: (kind, target, step) each.
pub fn faults_of(schedule: &str) -> Vec<(String, Option<String>, Option<u64>)> {
    let (host, guest) = schedule.split_once('|').unwrap_or((schedule, ""));
    let mut out = Vec::new();
    let lead = |s: &str| -> Option<u64> {
        let d: String = s.chars().take_while(char::is_ascii_digit).collect();
        d.parse().ok()
    };
    for w in host.split_whitespace() {
        let (k, v) = w.split_once('=').unwrap_or((w, ""));
        let kind = match k {
            "disk" if v != "none" => "disk",
            "cf" => "cut",
            "torn" | "mis" | "lse" | "hang" => "block",
            _ if k.starts_with("tj")
                || k.starts_with("xt")
                || k.starts_with("pct")
                || matches!(k, "pp" | "q" | "qb" | "br") =>
            {
                "timing"
            }
            _ => continue,
        };
        out.push((kind.to_string(), None, None));
    }
    for w in guest.split_whitespace().skip(1) {
        let Some((k, v)) = w.split_once('=') else {
            continue;
        };
        let mut c = k.chars();
        let (Some(letter), None) = (c.next(), c.next()) else {
            continue;
        };
        let at_target = |v: &str| -> (Option<String>, Option<u64>) {
            match v.split_once('@') {
                Some((t, r)) => (Some(t.to_string()), lead(r)),
                None => (None, None),
            }
        };
        let (kind, target, step) = match letter {
            'K' | 'X' => ("kill", at_target(v).0, at_target(v).1),
            'P' => ("pause", at_target(v).0, at_target(v).1),
            'N' => ("network", at_target(v).0, at_target(v).1),
            'O' => ("oneway", at_target(v).0, at_target(v).1),
            'F' => ("freeze", at_target(v).0, at_target(v).1),
            'H' => ("protocol", at_target(v).0, at_target(v).1),
            'G' => ("corrupt", at_target(v).0, at_target(v).1),
            'L' => ("powerloss", at_target(v).0, at_target(v).1),
            'U' => ("upgrade", at_target(v).0, at_target(v).1),
            'C' => ("cut", None, lead(v)),
            'T' => ("clock", None, lead(v)),
            'A' => {
                let (ev, action) = v.split_once(':').unwrap_or((v, ""));
                let step = ev.split_once('@').and_then(|x| lead(x.1));
                let svc = |p: &str| action.strip_prefix(p).map(str::to_string);
                if action.starts_with("cut") {
                    ("cut", None, step)
                } else if let Some(s) = svc("kill-now-").or_else(|| svc("kill-")) {
                    ("kill", Some(s), step)
                } else if action.starts_with("kill") {
                    ("kill", None, step)
                } else if action.starts_with("stop") {
                    ("pause", None, step)
                } else if action.starts_with("clock") {
                    ("clock", None, step)
                } else {
                    ("reaction", None, step)
                }
            }
            'Q' | 'B' | 'W' | 'I' => continue,
            _ => ("setting", Some(w.to_string()), None),
        };
        out.push((kind.to_string(), target, step));
    }
    out
}

/// The `hydo explore` option that draws an allowed kind the seeds did not draw; None when the
/// explorer's options already draw it.
fn option_for(k: &str, d: &Drawn) -> Option<&'static str> {
    let all = d.new_faults || matches!(d.strategy.as_str(), "swarm-pos" | "process");
    match k {
        "pause" | "clock" | "oneway" if !all => Some("--new-faults"),
        "corrupt" | "powerloss" if !d.storage => Some("--storage-faults"),
        "protocol" if !d.protocol => Some("--protocol-faults"),
        "freeze" => Some("--freeze-call"),
        _ => None,
    }
}

/// Which faults the schedules drew, against the manifest's fault space.
pub fn coverage(schedules: &[&str], space: Option<&Space>, d: &Drawn) -> StatsCoverage {
    let allowed: Vec<String> = space.map(|s| s.faults.clone()).unwrap_or_default();
    let services: Vec<String> = space.map(|s| s.svcs.clone()).unwrap_or_default();
    let mut kinds: BTreeMap<String, StatsKind> = BTreeMap::new();
    let mut steps: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let mut without = 0;
    for s in schedules {
        let fs = faults_of(s);
        if fs.is_empty() {
            without += 1;
        }
        let mut once: BTreeSet<&str> = BTreeSet::new();
        let mut targets_once: BTreeSet<(String, String)> = BTreeSet::new();
        for (k, t, st) in &fs {
            let e = kinds.entry(k.clone()).or_insert_with(|| StatsKind {
                kind: k.clone(),
                words: kind_words(k).into(),
                allowed: allowed.contains(k),
                ..StatsKind::default()
            });
            if once.insert(k) {
                e.runs += 1;
            }
            if let Some(t) = t {
                if targets_once.insert((k.clone(), t.clone())) {
                    *e.targets.entry(t.clone()).or_insert(0) += 1;
                }
            }
            if let Some(st) = st {
                steps.entry(k.clone()).or_default().insert(*st);
            }
        }
    }
    let mut kinds: Vec<StatsKind> = kinds
        .into_values()
        .map(|mut k| {
            k.steps = steps
                .get(&k.kind)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            k
        })
        .collect();
    kinds.sort_by(|a, b| b.runs.cmp(&a.runs).then(a.kind.cmp(&b.kind)));
    let drawn: BTreeSet<&str> = kinds.iter().map(|k| k.kind.as_str()).collect();
    let never = allowed
        .iter()
        .filter(|k| !drawn.contains(k.as_str()))
        .map(|k| StatsNever {
            kind: k.clone(),
            words: kind_words(k).into(),
            option: option_for(k, d).map(str::to_string),
        })
        .collect();
    let named = |svc: &str| -> bool {
        kinds.iter().filter(|k| k.kind != "setting").any(|k| {
            k.targets.keys().any(|t| {
                t.split([',', '/', '>', '-'])
                    .any(|p| p == svc || p.split('#').next() == Some(svc))
                    || space.is_some_and(|sp| {
                        sp.groups.iter().any(|g| {
                            t.split('.').next() == Some(g.name.as_str())
                                && g.members.iter().any(|m| m == svc)
                        })
                    })
            })
        })
    };
    let services_never = services.iter().filter(|s| !named(s)).cloned().collect();
    StatsCoverage {
        allowed,
        services,
        steps: space.and_then(|s| u64::try_from(s.steps).ok()),
        without_faults: without,
        kinds,
        never,
        services_never,
    }
}

/// The statistics of one exploration.
pub fn build(
    inp: Input,
    firsts: &[First],
    ledger: &Ledger,
    space: Option<&Space>,
    d: &Drawn,
) -> StatsDoc {
    let mut measures = vec![
        FINGERPRINTS.to_string(),
        VERDICTS.to_string(),
        PROPERTIES.to_string(),
    ];
    if inp.errlog {
        measures.push(ERROR_KINDS.into());
    }
    if inp.paths {
        measures.push(ERROR_PATHS.into());
    }
    let runs = mark_new(firsts);
    let curve = curve(&runs, &measures);
    // the cost
    let mut nruns: BTreeMap<String, u64> = BTreeMap::new();
    let mut vm: BTreeMap<String, f64> = BTreeMap::new();
    nruns.insert("first".into(), firsts.len() as u64);
    vm.insert("first".into(), firsts.iter().map(|f| f.vm_s).sum());
    for (r, (n, s)) in &inp.other_runs {
        *nruns.entry(r.clone()).or_insert(0) += n;
        *vm.entry(r.clone()).or_insert(0.0) += s;
    }
    let mut job_by_role: BTreeMap<String, f64> = BTreeMap::new();
    for (tag, s) in &ledger.jobs {
        let r = role_of_tag(tag);
        *job_by_role.entry(r.into()).or_insert(0.0) += s;
        if r == "other" {
            *nruns.entry("other".into()).or_insert(0) += 1;
        }
    }
    let workers = workers_of(ledger);
    let clocks: Vec<u64> = firsts.iter().filter_map(|f| f.clock).collect();
    let clock = (!clocks.is_empty()).then(|| clocks.iter().sum::<u64>());
    let sum = |f: fn(&StatsWorker) -> f64| round1(workers.iter().map(f).sum());
    let jobs: Vec<f64> = firsts.iter().filter_map(|f| f.job_s).collect();
    let cost = StatsCost {
        wall_s: round1(inp.wall_s),
        runs: nruns,
        vm_s: round1(vm.values().sum()),
        vm_s_by_role: vm.into_iter().map(|(k, v)| (k, round1(v))).collect(),
        shrink_share: shrink_share(&job_by_role),
        first_run_s: spread(
            &firsts
                .iter()
                .filter(|f| !f.cached && !f.refused)
                .map(|f| f.vm_s)
                .collect::<Vec<_>>(),
        ),
        clock,
        clock_per_run: clock.map(|c| c / clocks.len().max(1) as u64),
        clock_unit: clock.and(inp.clock_unit.clone()),
        host_cpu_s: inp.usage.map(|u| round1(u.cpu_s)),
        guest_ram_mib: inp.guest_ram_mib,
        engine_peak_mib: inp.usage.map(|u| u.children_peak_mib).filter(|m| *m > 0),
        explorer_peak_mib: inp.usage.map(|u| u.self_peak_mib),
        cached: firsts.iter().filter(|f| f.cached).count() as u64,
        busy_s: sum(|w| w.busy_s),
        starting_s: sum(|w| w.starting_s),
        waiting_s: sum(|w| w.waiting_s),
        idle_s: sum(|w| w.idle_s),
        workers,
    };
    let nw = cost.workers.len().max(1) as u64;
    let mut estimate = StatsEstimate {
        job_s: round3(if jobs.is_empty() {
            cost.first_run_s.mean
        } else {
            jobs.iter().sum::<f64>() / jobs.len() as f64
        }),
        starting_s: round1(
            cost.workers
                .iter()
                .map(|w| w.starting_s / (1 + w.restarts) as f64)
                .sum::<f64>()
                / nw as f64,
        ),
        shrink_share: cost.shrink_share,
        workers: nw,
        machine_workers: inp.machine_workers,
        measured_per_hour: (inp.sweep_wall_s > 0.0 && !firsts.is_empty())
            .then(|| round1(firsts.len() as f64 / inp.sweep_wall_s * 3600.0)),
        fits: Vec::new(),
    };
    estimate.fits = fits(&estimate);
    let schedules: Vec<&str> = firsts.iter().map(|f| f.schedule.as_str()).collect();
    StatsDoc {
        hydo_version: inp.hydo_version,
        epoch: inp.epoch,
        image: inp.image,
        engine: inp.engine,
        strategy: inp.strategy,
        seeds: inp.seeds,
        schedules: firsts.len() as u64,
        shards: None,
        curve,
        cost,
        coverage: coverage(&schedules, space, d),
        estimate,
        runs,
        ..StatsDoc::default()
    }
}

/// The statistics of a merge (`hydo explore --merge`): the parts' first runs in seed order with
/// each key kept at its first run, the costs added up (the wall time is the longest part's, as
/// the parts ran side by side), the coverage added up, and the estimate from the parts' measures.
pub fn merge(parts: &[StatsDoc], wall_s: f64) -> Option<StatsDoc> {
    let first = parts.first()?;
    let mut ps: Vec<&StatsDoc> = parts.iter().collect();
    ps.sort_by_key(|p| p.runs.first().map(|r| r.seed).unwrap_or(i64::MAX));
    let mut measures: Vec<String> = Vec::new();
    for m in [FINGERPRINTS, VERDICTS, PROPERTIES, ERROR_KINDS, ERROR_PATHS] {
        if ps.iter().any(|p| p.curve.measures.iter().any(|x| x == m)) {
            measures.push(m.into());
        }
    }
    let mut runs: Vec<StatsRun> = ps.iter().flat_map(|p| p.runs.iter().cloned()).collect();
    runs.sort_by_key(|r| r.seed);
    renew(&mut runs);
    let curve = curve(&runs, &measures);
    // the cost
    let mut c = StatsCost {
        wall_s: round1(wall_s),
        ..StatsCost::default()
    };
    let (mut cpu, mut any_cpu) = (0.0, false);
    let mut clock_runs = 0u64;
    for (i, p) in ps.iter().enumerate() {
        let pc = &p.cost;
        for (k, v) in &pc.runs {
            *c.runs.entry(k.clone()).or_insert(0) += v;
        }
        for (k, v) in &pc.vm_s_by_role {
            *c.vm_s_by_role.entry(k.clone()).or_insert(0.0) += v;
        }
        c.vm_s += pc.vm_s;
        if let Some(x) = pc.clock {
            c.clock = Some(c.clock.unwrap_or(0) + x);
            clock_runs += p.runs.iter().filter(|r| r.clock.is_some()).count() as u64;
            c.clock_unit = c.clock_unit.clone().or(pc.clock_unit.clone());
        }
        if let Some(x) = pc.host_cpu_s {
            cpu += x;
            any_cpu = true;
        }
        c.guest_ram_mib = c.guest_ram_mib.max(pc.guest_ram_mib);
        c.engine_peak_mib = c.engine_peak_mib.max(pc.engine_peak_mib);
        c.explorer_peak_mib = c.explorer_peak_mib.max(pc.explorer_peak_mib);
        c.cached += pc.cached;
        for w in &pc.workers {
            c.workers.push(StatsWorker {
                part: Some(i as u64 + 1),
                ..w.clone()
            });
        }
    }
    c.vm_s = round1(c.vm_s);
    c.vm_s_by_role = c
        .vm_s_by_role
        .into_iter()
        .map(|(k, v)| (k, round1(v)))
        .collect();
    c.host_cpu_s = any_cpu.then(|| round1(cpu));
    c.clock_per_run = c.clock.map(|x| x / clock_runs.max(1));
    let busy: f64 = c.workers.iter().map(|w| w.busy_s).sum();
    c.shrink_share = round3(if busy > 0.0 {
        ps.iter()
            .map(|p| p.cost.shrink_share * p.cost.busy_s)
            .sum::<f64>()
            / busy
    } else {
        0.0
    });
    c.busy_s = round1(busy);
    c.starting_s = round1(c.workers.iter().map(|w| w.starting_s).sum());
    c.waiting_s = round1(c.workers.iter().map(|w| w.waiting_s).sum());
    c.idle_s = round1(c.workers.iter().map(|w| w.idle_s).sum());
    let firsts: Vec<f64> = runs
        .iter()
        .filter(|r| !r.refused && r.job_s.is_some())
        .map(|r| r.vm_s)
        .collect();
    c.first_run_s = spread(&firsts);
    // the coverage
    let mut kinds: BTreeMap<String, StatsKind> = BTreeMap::new();
    for p in &ps {
        for k in &p.coverage.kinds {
            let e = kinds.entry(k.kind.clone()).or_insert_with(|| StatsKind {
                kind: k.kind.clone(),
                words: k.words.clone(),
                allowed: k.allowed,
                ..StatsKind::default()
            });
            e.runs += k.runs;
            for (t, n) in &k.targets {
                *e.targets.entry(t.clone()).or_insert(0) += n;
            }
            let s: BTreeSet<u64> = e.steps.iter().chain(&k.steps).copied().collect();
            e.steps = s.into_iter().collect();
        }
    }
    let mut kinds: Vec<StatsKind> = kinds.into_values().collect();
    kinds.sort_by(|a, b| b.runs.cmp(&a.runs).then(a.kind.cmp(&b.kind)));
    let drawn: BTreeSet<&str> = kinds.iter().map(|k| k.kind.as_str()).collect();
    let cov = &first.coverage;
    let named: BTreeSet<&String> = ps
        .iter()
        .flat_map(|p| p.coverage.services.iter())
        .filter(|s| !ps.iter().all(|p| p.coverage.services_never.contains(s)))
        .collect();
    let coverage = StatsCoverage {
        allowed: cov.allowed.clone(),
        services: cov.services.clone(),
        steps: cov.steps,
        without_faults: ps.iter().map(|p| p.coverage.without_faults).sum(),
        never: cov
            .never
            .iter()
            .filter(|n| !drawn.contains(n.kind.as_str()))
            .cloned()
            .collect(),
        services_never: cov
            .services
            .iter()
            .filter(|s| !named.contains(s))
            .cloned()
            .collect(),
        kinds,
    };
    // the estimate: one part's workers, with the measures of all parts
    let jobs: Vec<f64> = runs.iter().filter_map(|r| r.job_s).collect();
    let nw = ps
        .iter()
        .map(|p| p.estimate.workers)
        .max()
        .unwrap_or(1)
        .max(1);
    let mut estimate = StatsEstimate {
        job_s: round3(if jobs.is_empty() {
            c.first_run_s.mean
        } else {
            jobs.iter().sum::<f64>() / jobs.len() as f64
        }),
        starting_s: round1(
            ps.iter().map(|p| p.estimate.starting_s).sum::<f64>() / ps.len().max(1) as f64,
        ),
        shrink_share: c.shrink_share,
        workers: nw,
        machine_workers: None,
        measured_per_hour: (wall_s > 0.0).then(|| round1(runs.len() as f64 / wall_s * 3600.0)),
        fits: Vec::new(),
    };
    estimate.fits = fits(&estimate);
    let seeds = match (runs.first(), runs.last()) {
        (Some(a), Some(b)) => Some(format!("{}-{}", a.seed, b.seed)),
        _ => None,
    };
    Some(StatsDoc {
        hydo_version: first.hydo_version.clone(),
        epoch: first.epoch,
        image: first.image.clone(),
        engine: first.engine.clone(),
        strategy: first.strategy.clone(),
        seeds,
        schedules: runs.len() as u64,
        shards: Some(ps.len() as u64),
        curve,
        cost: c,
        coverage,
        estimate,
        runs,
        ..StatsDoc::default()
    })
}

// ---------------------------------------------------------------- words

/// Seconds in words: `45 s`, `12 min`, `3 h 20 min`.
pub fn secs_words(s: f64) -> String {
    if s < 10.0 {
        format!("{} s", (s * 100.0).round() / 100.0)
    } else if s < 90.0 {
        format!("{} s", s.round())
    } else if s < 3570.0 {
        format!("{} min", (s / 60.0).round())
    } else {
        let m = (s / 60.0).round() as u64;
        if m.is_multiple_of(60) {
            format!("{} h", m / 60)
        } else {
            format!("{} h {} min", m / 60, m % 60)
        }
    }
}

fn pct(x: f64) -> String {
    format!("{}%", (x * 100.0).round())
}

/// Where the runs ran, in plain words.
fn place_words(e: &Engine) -> &'static str {
    match e.backend.as_deref() {
        Some("hvm") => "on this Mac",
        Some("kvm") => "on KVM",
        Some("hvx") => "on hvx",
        _ => "",
    }
}

fn engine_name(e: &Engine) -> &'static str {
    match e.backend.as_deref() {
        Some("hvm") => "the Mac engine",
        Some("kvm") => "the KVM engine",
        Some("hvx") => "hvx",
        _ => "the engine",
    }
}

/// The allowed fault kinds that no schedule drew, in one sentence: one phrase per option that
/// draws them, in the order of the list. None when every allowed kind was drawn.
fn never_line(d: &StatsDoc) -> Option<String> {
    if d.coverage.never.is_empty() {
        return None;
    }
    let mut by: Vec<(Option<&str>, Vec<String>)> = Vec::new();
    for n in &d.coverage.never {
        let o = n.option.as_deref();
        match by.iter_mut().find(|(x, _)| *x == o) {
            Some((_, ws)) => ws.push(n.words.clone()),
            None => by.push((o, vec![n.words.clone()])),
        }
    }
    let ws: Vec<String> = by
        .into_iter()
        .map(|(o, ws)| {
            let l = hydo_schedule::describe::and_list(&ws);
            match o {
                Some(o) => format!("{l} (`{o}` draws them)"),
                None => format!("{l} (more seeds can draw them)"),
            }
        })
        .collect();
    Some(format!(
        "Never drawn, though the project allows them: {}.",
        ws.join("; ")
    ))
}

/// The short section of `hydo report <explore dir>`: how much was explored, whether the curve is
/// still rising, and what an hour more gives. Plain words; `hydo stats` has the rest.
pub fn short_text(d: &StatsDoc) -> Vec<String> {
    let c = &d.cost;
    let mut out = vec![format!(
        "{} runs {} with {} worker{}{} in {}, {} of the virtual machines' time. The workers \
         spent {} of their busy time running failures again and shrinking them.",
        d.schedules,
        place_words(&d.engine),
        d.estimate.workers,
        if d.estimate.workers == 1 { "" } else { "s" },
        match d.shards {
            Some(n) => format!(" in each of {n} parts"),
            None => String::new(),
        },
        secs_words(c.wall_s),
        secs_words(c.vm_s),
        pct(c.shrink_share)
    )];
    out.push(d.curve.judgement.clone());
    out.extend(never_line(d));
    if let Some(f) = d.estimate.fits.first() {
        out.push(format!(
            "An hour more on {} worker{} fits about {} runs.",
            f.workers,
            if f.workers == 1 { "" } else { "s" },
            f.runs
        ));
    }
    out
}

/// Each line of `t` wrapped at 80 characters, the continuation under the line's own indent and
/// two more spaces.
fn wrap_lines(t: &str) -> String {
    let mut o = String::new();
    for l in t.lines() {
        let lead: String = l.chars().take_while(|c| *c == ' ').collect();
        let more = format!("{lead}  ");
        let mut ind = lead.clone();
        let mut cur = String::new();
        for w in l.split_whitespace() {
            if !cur.is_empty() && ind.len() + cur.chars().count() + 1 + w.chars().count() > 80 {
                o.push_str(&ind);
                o.push_str(&cur);
                o.push('\n');
                cur.clear();
                ind = more.clone();
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(w);
        }
        o.push_str(&ind);
        o.push_str(&cur);
        o.push('\n');
    }
    o
}

/// `hydo stats <explore dir>` in words.
pub fn text(d: &StatsDoc, asked: &[(f64, u64, u64)]) -> String {
    wrap_lines(&text_lines(d, asked).replace('`', ""))
}

fn text_lines(d: &StatsDoc, asked: &[(f64, u64, u64)]) -> String {
    let mut o = String::new();
    let c = &d.cost;
    let seeds = d
        .seeds
        .as_ref()
        .map(|s| format!(", seeds {s}"))
        .unwrap_or_default();
    o.push_str(&format!(
        "{} first runs ({} strategy{seeds}) on {} with {} worker{}{}, in {}.\n",
        d.schedules,
        d.strategy,
        engine_name(&d.engine),
        d.estimate.workers,
        if d.estimate.workers == 1 { "" } else { "s" },
        match d.shards {
            Some(n) => format!(" in each of {n} parts"),
            None => String::new(),
        },
        secs_words(c.wall_s)
    ));
    // the curve
    o.push_str("\nNew behaviour\n");
    o.push_str(&format!("  {}\n", d.curve.judgement));
    if let Some(last) = d.curve.points.last() {
        for m in &d.curve.measures {
            let n = last.counts.get(m).copied().unwrap_or(0);
            let grew = d
                .curve
                .grew_at
                .get(m)
                .and_then(|g| g.last())
                .map(|x| format!(", the last new one at run {x}"))
                .unwrap_or_default();
            o.push_str(&format!(
                "  {n} distinct {}{}\n",
                measure_words(m),
                if m == FINGERPRINTS {
                    " (almost every run gives a new one; not judged)".to_string()
                } else {
                    grew
                }
            ));
        }
        let pts: Vec<(String, String)> = d
            .curve
            .points
            .iter()
            .filter(|p| p.runs.is_power_of_two() || p.runs == last.runs)
            .map(|p| {
                let k: u64 = p
                    .counts
                    .iter()
                    .filter(|(m, _)| *m != FINGERPRINTS)
                    .map(|x| x.1)
                    .sum();
                (p.runs.to_string(), k.to_string())
            })
            .collect();
        let (ns, ks): (Vec<String>, Vec<String>) = pts.into_iter().unzip();
        o.push_str(&format!(
            "  After {} runs, the judged measures counted {} distinct things together.\n",
            hydo_schedule::describe::and_list(&ns),
            hydo_schedule::describe::and_list(&ks)
        ));
    }
    o.push_str(
        "  A flat curve says that more seeds of the same strategy and options mostly repeat what \
         these runs showed. It does not say that the system has no more bugs: other fault kinds, \
         other options, a longer workload or other checks can still find more.\n",
    );
    // the cost
    o.push_str("\nCost\n");
    let roles: Vec<String> = ["first", "rerun", "shrink", "error-shrink", "other"]
        .iter()
        .filter_map(|r| {
            let n = c.runs.get(*r).copied().unwrap_or(0);
            (n > 0).then(|| {
                let s = c
                    .vm_s_by_role
                    .get(*r)
                    .map(|s| format!(", {}", secs_words(*s)))
                    .unwrap_or_default();
                format!("{n} {}{s}", role_words(r))
            })
        })
        .collect();
    o.push_str(&format!(
        "  Runs: {}. Engine time {} in all; {} of the workers' busy time went to re-running and \
         shrinking failures.\n",
        roles.join("; "),
        secs_words(c.vm_s),
        pct(c.shrink_share)
    ));
    o.push_str(&format!(
        "  A first run took {} on average (median {}, 90% under {}, the longest {}).\n",
        secs_words(c.first_run_s.mean),
        secs_words(c.first_run_s.median),
        secs_words(c.first_run_s.p90),
        secs_words(c.first_run_s.max)
    ));
    if let (Some(n), Some(per), Some(u)) = (c.clock, c.clock_per_run, &c.clock_unit) {
        o.push_str(&format!(
            "  The engine's clock: {n} {u} over the first runs, {per} per run.\n"
        ));
    }
    let mut m = Vec::new();
    if let Some(x) = c.host_cpu_s {
        m.push(format!("host CPU {}", secs_words(x)));
    }
    if let Some(x) = c.guest_ram_mib {
        m.push(format!("guest RAM {x} MiB a worker"));
    }
    if let Some(x) = c.engine_peak_mib {
        m.push(format!("the largest engine process {x} MiB resident"));
    }
    if let Some(x) = c.explorer_peak_mib {
        m.push(format!("the explorer {x} MiB resident"));
    }
    if !m.is_empty() {
        o.push_str(&format!("  {}.\n", capital(&m.join(", "))));
    }
    let life = c.busy_s + c.starting_s + c.waiting_s + c.idle_s;
    if life > 0.0 {
        o.push_str(&format!(
            "  The workers were busy {} of their time, starting engines {}, waiting for a quiet \
             window {} and idle {}.\n",
            pct(c.busy_s / life),
            pct(c.starting_s / life),
            pct(c.waiting_s / life),
            pct(c.idle_s / life)
        ));
    }
    if c.cached > 0 {
        o.push_str(&format!(
            "  {} first runs came from the result cache and cost no VM.\n",
            c.cached
        ));
    }
    // the coverage
    let cv = &d.coverage;
    o.push_str("\nFaults drawn\n");
    if cv.kinds.is_empty() {
        o.push_str("  No schedule drew a fault.\n");
    }
    for k in &cv.kinds {
        let mut l = format!("  {}: {} of {} runs", k.words, k.runs, d.schedules);
        if !k.targets.is_empty() {
            let ts: Vec<String> = k
                .targets
                .iter()
                .take(6)
                .map(|(t, n)| format!("{t} {n}"))
                .collect();
            l.push_str(&format!(
                "; on {}{}",
                ts.join(", "),
                if k.targets.len() > 6 { ", ..." } else { "" }
            ));
        }
        if !k.steps.is_empty() {
            l.push_str(&match cv.steps {
                Some(n) => format!("; at {} of the {n} workload steps", k.steps.len()),
                None => format!("; at {} workload steps", k.steps.len()),
            });
        }
        if !cv.allowed.is_empty()
            && !k.allowed
            && !matches!(k.kind.as_str(), "timing" | "setting" | "block" | "upgrade")
        {
            l.push_str(" (not in faults:)");
        }
        o.push_str(&l);
        o.push('\n');
    }
    if cv.without_faults > 0 {
        o.push_str(&format!(
            "  {} of {} runs had no fault.\n",
            cv.without_faults, d.schedules
        ));
    }
    if let Some(l) = never_line(d) {
        o.push_str(&format!("  {l}\n"));
    }
    if !cv.services_never.is_empty() {
        o.push_str(&format!(
            "  No fault named {}.\n",
            hydo_schedule::describe::and_list(&cv.services_never)
        ));
    }
    // the estimate
    let e = &d.estimate;
    o.push_str("\nHow many runs fit\n");
    o.push_str(&format!(
        "  Measured: {} a first run on one worker, {} to start a worker, {} of the time on \
         re-runs and shrinks{}.\n",
        secs_words(e.job_s),
        secs_words(e.starting_s),
        pct(e.shrink_share),
        e.measured_per_hour
            .map(|x| format!(
                "; {} first runs an hour on {} workers",
                x.round(),
                e.workers
            ))
            .unwrap_or_default()
    ));
    let mut rows: Vec<(f64, u64, u64, u64)> = e
        .fits
        .iter()
        .map(|f| (f.budget_s, f.workers, f.shards, f.runs))
        .collect();
    for (b, w, s) in asked {
        rows.push((*b, *w, *s, fit(*b, e, *w, *s)));
    }
    for (b, w, s, n) in rows {
        o.push_str(&format!(
            "  {}, {w} worker{}{}: about {n} first runs\n",
            secs_words(b),
            if w == 1 { "" } else { "s" },
            if s > 1 {
                format!(" on each of {s} machines")
            } else {
                String::new()
            }
        ));
    }
    if let Some(m) = e.machine_workers {
        o.push_str(&format!(
            "  This machine fits about {m} workers for this guest now. A shard runs on a machine \
             of its own; the estimate counts each like this one.\n"
        ));
    }
    o
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn role_words(r: &str) -> &'static str {
    match r {
        "first" => "first runs",
        "rerun" => "re-runs of failures",
        "shrink" => "shrink runs",
        "error-shrink" => "shrink runs of error kinds",
        _ => "other runs",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(seed: i64, fp: &str, v: &str, props: &[&str]) -> First {
        First {
            seed,
            schedule: "disk=none dseed=1 | run".into(),
            verdict: v.into(),
            fp: fp.into(),
            vm_s: 1.0,
            job_s: Some(1.5),
            properties: props.iter().map(|x| x.to_string()).collect(),
            ..First::default()
        }
    }

    #[test]
    fn the_curve_counts_what_each_run_showed_first() {
        let mut fs = Vec::new();
        for i in 1..=40 {
            let v = if i == 7 {
                "VIOLATION lost k:3"
            } else {
                "HYDO-ORACLE: OK acked=3"
            };
            fs.push(first(i, &format!("{i:016x}"), v, &["a: held"]));
        }
        let runs = mark_new(&fs);
        let m: Vec<String> = [FINGERPRINTS, VERDICTS, PROPERTIES]
            .iter()
            .map(|x| x.to_string())
            .collect();
        let c = curve(&runs, &m);
        let last = c.points.last().unwrap();
        assert_eq!(last.runs, 40);
        assert_eq!(last.counts[FINGERPRINTS], 40);
        assert_eq!(last.counts[VERDICTS], 2);
        assert_eq!(last.counts[PROPERTIES], 1);
        assert_eq!(c.grew_at[VERDICTS], vec![1, 7]);
        assert_eq!(c.window, 10);
        assert_eq!(c.trend, trend::FLAT);
        assert!(c.judgement.contains("since run 7 of 40"), "{}", c.judgement);
        // a new outcome in the window
        fs[35].properties.push("b: failed".into());
        let c = curve(&mark_new(&fs), &m);
        assert_eq!(c.trend, trend::GROWING);
        assert!(
            c.judgement.contains("1 new property outcome"),
            "{}",
            c.judgement
        );
        // the points: powers of two, the last
        let ns: Vec<u64> = c.points.iter().map(|p| p.runs).collect();
        assert!(ns.starts_with(&[1, 2, 3, 4]), "{ns:?}");
        assert_eq!(*ns.last().unwrap(), 40);
    }

    #[test]
    fn a_merge_keeps_each_key_at_its_first_run() {
        let fs: Vec<First> = (1..=12)
            .map(|i| first(i, &format!("{:016x}", i % 5), "HYDO-ORACLE: OK", &[]))
            .collect();
        let whole = mark_new(&fs);
        let mut parts: Vec<StatsRun> = mark_new(&fs[..6]);
        parts.extend(mark_new(&fs[6..]));
        renew(&mut parts);
        assert_eq!(parts, whole);
    }

    #[test]
    fn faults_are_read_with_their_targets_and_steps() {
        let f = faults_of(
            "disk=cut dseed=1 tj=3 | run K=redis@11+1 C=24 T=3:-60 R=everysec Q=4 A=ev@2+5:kill-now-db",
        );
        let k: Vec<(&str, Option<&str>, Option<u64>)> = f
            .iter()
            .map(|(a, b, c)| (a.as_str(), b.as_deref(), *c))
            .collect();
        assert_eq!(
            k,
            vec![
                ("disk", None, None),
                ("timing", None, None),
                ("kill", Some("redis"), Some(11)),
                ("cut", None, Some(24)),
                ("clock", None, Some(3)),
                ("setting", Some("R=everysec"), None),
                ("kill", Some("db"), Some(2)),
            ]
        );
        assert!(faults_of("disk=none dseed=4 | run").is_empty());
    }

    #[test]
    fn a_budget_fits_whole_runs() {
        let e = StatsEstimate {
            job_s: 10.0,
            starting_s: 100.0,
            shrink_share: 0.5,
            workers: 2,
            ..StatsEstimate::default()
        };
        // (3600 - 100) * 2 / 10 * 0.5 = 350
        assert_eq!(fit(3600.0, &e, 2, 1), 350);
        assert_eq!(fit(3600.0, &e, 2, 4), 1400);
        assert_eq!(fit(50.0, &e, 2, 1), 0);
        assert_eq!(fit_count(10.0, 0.0), 0);
    }

    #[test]
    fn property_outcomes_take_the_worst_evaluation() {
        let t = "HYDO-ASSERT {\"t\":\"always\",\"id\":\"a\",\"ok\":true}\n\
                 HYDO-ASSERT {\"t\":\"always\",\"id\":\"a\",\"ok\":false}\n\
                 HYDO-ASSERT {\"t\":\"sometimes\",\"id\":\"s\",\"ok\":false}\n\
                 HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"r\",\"ok\":true}\n";
        assert_eq!(property_outcomes(t), vec!["a: failed", "r: reached"]);
    }
}
