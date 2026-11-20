//! `hydo explore --merge DIR... --out DIR` (Milestone 304): one exploration from the parts of a
//! seed range that ran apart, for example `hydo explore --shard I/N` on N machines. It runs no
//! VM.
//!
//! The explorer draws every schedule from its seed, and every run on one engine repeats
//! exactly, so the parts' first runs are the runs of one exploration of the whole range. What
//! one exploration decides over all of its runs is decided again here, in seed order:
//!
//! - the first runs, the re-runs of each failure and the failures, in seed order;
//! - which failures are shrunk: the first of each class and signature (with `--shrink-causes N`,
//!   up to N with other fault kinds), so the kept shrink is the one its part made, renamed to
//!   the number one exploration gives it (`shrink3-…`, `min3-a`);
//! - the error kinds over every run, and which new kinds are shrunk (`--errors-shrink`);
//! - the property table and the liveness summary, from the parts' tables and summaries;
//! - the summary's counts. Its wall times are the longest part's, as the parts ran side by
//!   side, and each part's own times and host CPU are in `shards`.
//!
//! On the scripted engine a merged exploration equals the exploration of the whole range, apart
//! from the wall times, the workers and `shards` (tests/fake_engine.rs, the scenario
//! `merge_equals_one_exploration`).
//!
//! The parts must be one exploration: the same hydo and fingerprint epoch, the same image (by
//! its sha256), the same engine and guest, and the same options apart from `--seeds`, `--shard`,
//! `--out` and `--workers`, with seed ranges that do not overlap. A part's failure is re-run and
//! shrunk on its own machine, so no fingerprint is compared across parts: KVM fingerprints differ
//! between CPU vendors (an AMD and an Intel host), and verdicts and signatures do not.

use crate::args::{self, Args};
use crate::errlog;
use crate::md;
use crate::py::round1;
use crate::run::fault_kinds;
use hydo_errors::Aggregate;
use hydo_results::{
    role, End, ErrorKind, ErrorKinds, Exploration, Failure, Liveness, MergedShard, PathCoverage,
    Property, PropertyTable, Run, Shrink, Start, Summary, TimeLimit,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage: hydo explore --merge DIR [DIR...] --out DIR";

/// One part as read: its directory, its document, its options, and its stream's lines (each
/// with its JSON).
struct Part {
    dir: PathBuf,
    doc: Exploration,
    args: Args,
    lines: Vec<(String, Value)>,
    /// The seeds it explored (its range, or the first ones of it under `--time-limit`).
    lo: u64,
    hi: u64,
}

/// The command line after `--merge`: the parts' directories and `--out`.
fn parse(argv: &[String]) -> Result<(Vec<PathBuf>, PathBuf), String> {
    let mut dirs = Vec::new();
    let mut out = None;
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--merge" {
        } else if a == "--out" {
            i += 1;
            out = Some(PathBuf::from(
                argv.get(i).ok_or("argument --out: expected one argument")?,
            ));
        } else if let Some(v) = a.strip_prefix("--out=") {
            out = Some(PathBuf::from(v));
        } else if a.starts_with("--") {
            return Err(format!(
                "unrecognized arguments: {a} (--merge takes directories and --out)"
            ));
        } else {
            dirs.push(PathBuf::from(a));
        }
        i += 1;
    }
    let out = out.ok_or("the following arguments are required: --out")?;
    if dirs.is_empty() {
        return Err("--merge: name the parts' directories".into());
    }
    Ok((dirs, out))
}

/// The entry point: Some(exit status) when the command line asks for a merge. It prints the
/// summary line (the explorer's last line) unless `plain_end`, where the caller prints the end
/// for a person.
pub fn main_if_asked(argv: &[String], plain_end: bool) -> Option<i32> {
    if !argv.iter().any(|a| a == "--merge") {
        return None;
    }
    let (dirs, out) = match parse(argv) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{USAGE}\nhydo explore: error: {e}");
            return Some(2);
        }
    };
    Some(match merge(&dirs, &out) {
        Ok(doc) => {
            if !plain_end {
                println!("{}", md::summary_line(&doc));
            }
            0
        }
        Err(e) => {
            eprintln!("hydo explore --merge: {e}");
            1
        }
    })
}

fn read_part(dir: &Path) -> Result<Part, String> {
    let at = |f: &str| dir.join(f);
    let doc = hydo_results::reader::read_exploration(&at(hydo_results::EXPLORATION_DOC))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let text = std::fs::read_to_string(at(hydo_results::EVENTS))
        .map_err(|e| format!("{}: {e}", at(hydo_results::EVENTS).display()))?;
    let mut lines = Vec::new();
    for l in text.split_inclusive('\n') {
        if !l.ends_with('\n') {
            break; // a line still being written (the reader's rule 6)
        }
        let v: Value = serde_json::from_str(l).map_err(|e| {
            format!(
                "{}: a line that is not JSON: {e}",
                at(hydo_results::EVENTS).display()
            )
        })?;
        lines.push((l.to_string(), v));
    }
    let what = |w: &str| format!("{}: {w}, so it cannot be merged", dir.display());
    if doc.mode != "seeds" {
        return Err(what(&format!("an exploration in the mode {}", doc.mode)));
    }
    if doc.steer.is_some() || doc.freeze_sweep.is_some() || doc.turn_sweep.is_some() {
        return Err(what("a steered search or a sweep"));
    }
    if !doc.shards.is_empty() {
        return Err(what("already a merged exploration"));
    }
    let args = args::parse(&doc.config.argv)
        .map_err(|e| what(&format!("its command line does not read ({e})")))?;
    if args.search.as_deref() == Some("state") {
        return Err(what("a state search"));
    }
    let seeds = match doc.summary.time_limit.as_ref() {
        Some(t) if t.cut => t.explored_seeds.clone().unwrap_or_default(),
        _ => doc.config.seeds.clone().unwrap_or_default(),
    };
    let (lo, hi) = if seeds.is_empty() {
        (0, 0)
    } else {
        hydo_manifest::ci::parse_seeds(&seeds)
            .ok_or_else(|| what(&format!("its seeds `{seeds}` are not lo-hi")))?
    };
    Ok(Part {
        dir: dir.to_path_buf(),
        doc,
        args,
        lines,
        lo,
        hi,
    })
}

/// The options that make two parts one exploration: the command line without `--seeds`,
/// `--shard`, `--out` and `--workers` and their values.
fn same_options(argv: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in argv {
        if skip {
            skip = false;
            continue;
        }
        let key = a.split('=').next().unwrap_or(a);
        if ["--seeds", "--shard", "--out", "--workers"].contains(&key) {
            skip = !a.contains('=');
            continue;
        }
        out.push(a.clone());
    }
    out
}

/// The merged command line: the first part's, with the whole range, without `--shard`, and
/// with the merged directory as `--out`.
fn merged_argv(argv: &[String], range: &str, out: &Path) -> Vec<String> {
    let mut v = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        let key = a.split('=').next().unwrap_or(a).to_string();
        let inline = a.contains('=');
        match key.as_str() {
            "--shard" => {
                i += if inline { 1 } else { 2 };
                continue;
            }
            "--seeds" | "--out" => {
                let val = if key == "--seeds" {
                    range.to_string()
                } else {
                    out.display().to_string()
                };
                if inline {
                    v.push(format!("{key}={val}"));
                } else {
                    v.push(key);
                    v.push(val);
                    i += 1;
                }
            }
            _ => v.push(a.clone()),
        }
        i += 1;
    }
    v
}

fn check_same(parts: &[Part]) -> Result<(), String> {
    let first = &parts[0];
    for p in &parts[1..] {
        let differ = |what: &str, a: String, b: String| -> Result<(), String> {
            if a == b {
                Ok(())
            } else {
                Err(format!(
                    "the parts are not one exploration: {} has {what} {b}, {} has {a}",
                    p.dir.display(),
                    first.dir.display()
                ))
            }
        };
        differ(
            "hydo",
            format!("{:?}", first.doc.hydo_version),
            format!("{:?}", p.doc.hydo_version),
        )?;
        differ(
            "the fingerprint epoch",
            format!("{:?}", first.doc.epoch),
            format!("{:?}", p.doc.epoch),
        )?;
        match (&first.doc.image.sha256, &p.doc.image.sha256) {
            (Some(a), Some(b)) => differ("the image", a.clone(), b.clone())?,
            _ => differ(
                "the image",
                first.doc.image.path.clone(),
                p.doc.image.path.clone(),
            )?,
        }
        let eng = |d: &Exploration| {
            let e = &d.engine;
            format!(
                "{:?} {:?} {:?} {:?} {:?} {:?}",
                e.backend, e.args, e.ram_mib, e.vcpus, e.cpu, e.guest_kernel
            )
        };
        differ("the engine", eng(&first.doc), eng(&p.doc))?;
        differ(
            "the options",
            same_options(&first.doc.config.argv).join(" "),
            same_options(&p.doc.config.argv).join(" "),
        )?;
        let sh = |d: &Exploration| {
            d.config
                .shard
                .as_ref()
                .map(|s| format!("{} shards of {}", s.count, s.range))
                .unwrap_or_else(|| "no --shard".into())
        };
        differ("", sh(&first.doc), sh(&p.doc))?;
    }
    for w in parts.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if a.lo != 0 && b.lo != 0 && b.lo <= a.hi {
            return Err(format!(
                "the parts overlap: {} explored {}-{}, {} explored {}-{}",
                a.dir.display(),
                a.lo,
                a.hi,
                b.dir.display(),
                b.lo,
                b.hi
            ));
        }
    }
    Ok(())
}

/// `a-b` ranges in order, joined where they touch, and by commas.
fn ranges_text(r: &[(u64, u64)]) -> String {
    let mut v: Vec<(u64, u64)> = r.to_vec();
    v.sort();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (a, b) in v {
        match out.last_mut() {
            Some(l) if a <= l.1 + 1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out.iter()
        .map(|(a, b)| format!("{a}-{b}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// A record's type, role, seed and tag.
fn kind(v: &Value) -> (&str, &str, Option<i64>, &str) {
    (
        v.get("type").and_then(Value::as_str).unwrap_or(""),
        v.get("role").and_then(Value::as_str).unwrap_or(""),
        v.get("seed").and_then(Value::as_i64),
        v.get("tag").and_then(Value::as_str).unwrap_or(""),
    )
}

/// The local number of a shrink chain's tag (`shrink-…` and `min-a` are 0, `shrink3-…` and
/// `min3-b` are 2), and the rest of the tag after its prefix.
fn chain_of(tag: &str) -> Option<(usize, String)> {
    let num = |d: &str| -> Option<usize> {
        if d.is_empty() {
            Some(0)
        } else {
            d.parse::<usize>().ok().filter(|n| *n >= 2).map(|n| n - 1)
        }
    };
    if let Some(r) = tag.strip_prefix("shrink") {
        let (d, rest) = r.split_once('-')?;
        if d.chars().all(|c| c.is_ascii_digit()) {
            return Some((num(d)?, format!("s-{rest}")));
        }
    }
    if let Some(r) = tag.strip_prefix("min") {
        let (d, rest) = r.split_once('-')?;
        if d.chars().all(|c| c.is_ascii_digit()) && (rest == "a" || rest == "b") {
            return Some((num(d)?, format!("m-{rest}")));
        }
    }
    None
}

/// The tag of chain `k` (0-based) for the rest that [`chain_of`] gave.
fn chain_tag(k: usize, rest: &str) -> String {
    let n = if k == 0 {
        String::new()
    } else {
        (k + 1).to_string()
    };
    match rest.split_once('-') {
        Some(("s", r)) => format!("shrink{n}-{r}"),
        Some(("m", r)) => format!("min{n}-{r}"),
        _ => rest.to_string(),
    }
}

/// A tag of a part as the merged exploration names it, by the part's chain numbers.
fn rename_tag(tag: &str, chains: &HashMap<usize, usize>) -> Option<String> {
    let (k, rest) = chain_of(tag)?;
    chains.get(&k).map(|g| chain_tag(*g, &rest))
}

/// Every string of a record that is a tag of `chains`, or a console path of one
/// (`consoles/<tag>.<suffix>`), renamed.
fn rename_value(v: &mut Value, chains: &HashMap<usize, usize>) {
    match v {
        Value::String(s) => {
            if let Some(t) = rename_tag(s, chains) {
                *s = t;
            } else if let Some(rest) = s.strip_prefix("consoles/") {
                let (stem, suffix) = rest.split_once('.').unwrap_or((rest, ""));
                if let Some(t) = rename_tag(stem, chains) {
                    *s = if suffix.is_empty() {
                        format!("consoles/{t}")
                    } else {
                        format!("consoles/{t}.{suffix}")
                    };
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| rename_value(x, chains)),
        Value::Object(o) => o.values_mut().for_each(|x| rename_value(x, chains)),
        _ => {}
    }
}

fn line_of<T: serde::Serialize>(rec: &T) -> String {
    hydo_results::to_line(rec)
}

/// The property tables of the parts, in seed order, as one exploration's table: the counts add
/// up, a property keeps its place of first appearance, and the first failing evaluation is the
/// earliest part's.
fn merge_properties(tables: &[&PropertyTable]) -> Option<PropertyTable> {
    if tables.is_empty() {
        return None;
    }
    let mut t = PropertyTable::default();
    let mut props: Vec<Property> = Vec::new();
    for x in tables {
        t.runs += x.runs;
        t.setup_complete_runs += x.setup_complete_runs;
        for p in &x.properties {
            match props.iter_mut().find(|q| q.id == p.id) {
                None => props.push(p.clone()),
                Some(q) => {
                    if q.message.is_empty() {
                        q.message = p.message.clone();
                    }
                    q.declared |= p.declared;
                    q.passes += p.passes;
                    q.fails += p.fails;
                    q.runs_passed += p.runs_passed;
                    q.runs_failed += p.runs_failed;
                    if q.first_failure.is_none() {
                        q.first_failure = p.first_failure.clone();
                    }
                    q.slowest_ok_s = max_opt(q.slowest_ok_s, p.slowest_ok_s);
                    q.within_s = max_opt(q.within_s, p.within_s);
                }
            }
        }
    }
    for p in props.iter_mut() {
        let v = crate::results::property_verdict(&p.assertion, p.passes, p.fails);
        p.verdict = v.into();
        p.status = crate::results::property_status(v).into();
    }
    t.violations = props
        .iter()
        .filter(|p| p.verdict.starts_with("FAIL"))
        .count() as u64;
    t.properties = props;
    Some(t)
}

/// The error paths of the parts as one exploration's (Milestone 306): the counts add up, and
/// a place's first run is that of the earliest part that ran it (the parts are in seed order).
/// The parts explored one image, so their places are the same, in the same order.
fn merge_paths(parts: &[Option<&PathCoverage>]) -> Option<PathCoverage> {
    let mut it = parts.iter().flatten();
    let mut out = (*it.next()?).clone();
    for p in it {
        out.runs += p.runs;
        out.runs_without_faults += p.runs_without_faults;
        for (o, q) in out.places.iter_mut().zip(&p.places) {
            if o.id != q.id {
                continue;
            }
            o.runs += q.runs;
            o.runs_without_faults += q.runs_without_faults;
            o.unwatched_runs += q.unwatched_runs;
            if o.first.is_none() {
                o.first = q.first.clone();
            }
        }
    }
    Some(out)
}

fn max_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// The run's name as the liveness summary orders runs (by console file name).
fn log_name(run: &Option<String>) -> String {
    format!("{}.log", run.as_deref().unwrap_or(""))
}

/// Of two (value, run) pairs, the larger value; on a tie the run first by console file name (as
/// one exploration's summary, which reads the runs in that order and keeps the first maximum).
fn slowest(
    a: (Option<f64>, Option<String>),
    b: (Option<f64>, Option<String>),
) -> (Option<f64>, Option<String>) {
    match (a.0, b.0) {
        (None, _) => b,
        (_, None) => a,
        (Some(x), Some(y)) => {
            if y > x || (y == x && log_name(&b.1) < log_name(&a.1)) {
                b
            } else {
                a
            }
        }
    }
}

fn merge_liveness(parts: &[&[Liveness]]) -> Vec<Liveness> {
    let mut m: BTreeMap<String, Liveness> = BTreeMap::new();
    for l in parts.iter().flat_map(|x| x.iter()) {
        let Some(q) = m.get_mut(&l.id) else {
            m.insert(l.id.clone(), l.clone());
            continue;
        };
        q.held_runs += l.held_runs;
        q.missed_runs += l.missed_runs;
        let (s, r) = slowest(
            (q.slowest_s, q.slowest_run.clone()),
            (l.slowest_s, l.slowest_run.clone()),
        );
        q.slowest_s = s;
        q.slowest_run = r;
        let (s, r) = slowest(
            (q.slowest_late_s, q.slowest_late_run.clone()),
            (l.slowest_late_s, l.slowest_late_run.clone()),
        );
        q.slowest_late_s = s;
        q.slowest_late_run = r;
        q.within_s = max_opt(q.within_s, l.within_s);
        if l.first_miss_run.is_some()
            && (q.first_miss_run.is_none()
                || log_name(&l.first_miss_run) < log_name(&q.first_miss_run))
        {
            q.first_miss_run = l.first_miss_run.clone();
            q.first_miss = l.first_miss.clone();
        }
    }
    m.into_values().collect()
}

/// The error kinds of the parts, in seed order, as one exploration's: counts add up, a kind
/// keeps its place of first appearance and its first run's details, and its first 12 seeds.
fn merge_kinds(parts: &[&ErrorKinds]) -> ErrorKinds {
    let mut out = ErrorKinds {
        kinds: Vec::new(),
        ..parts[0].clone()
    };
    for p in parts {
        for k in &p.kinds {
            match out.kinds.iter_mut().find(|q| q.signature == k.signature) {
                None => out.kinds.push(ErrorKind {
                    shrink: None,
                    ..k.clone()
                }),
                Some(q) => {
                    q.runs += k.runs;
                    q.new_runs += k.new_runs;
                    q.lines += k.lines;
                    q.new = q.new_runs > 0;
                    for s in &k.seeds {
                        if q.seeds.len() < 12 {
                            q.seeds.push(s.clone());
                        }
                    }
                    if q.first_new.is_none() {
                        q.first_new = k.first_new.clone();
                    }
                    if q.reason.is_none() {
                        q.reason = k.reason.clone();
                    }
                }
            }
        }
    }
    out
}

/// Which part explored a seed.
fn part_of(parts: &[Part], seed: i64) -> Option<usize> {
    let s = u64::try_from(seed).ok()?;
    parts
        .iter()
        .position(|p| p.lo != 0 && p.lo <= s && s <= p.hi)
}

/// Copy a file, or a directory with everything in it, unless the target is there.
fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        return Ok(());
    }
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_all(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// Merge the parts in `dirs` into `out`: exploration.json, events.jsonl, report.md, the
/// consoles of every run it keeps, each failure's replay, and each part's own document under
/// `parts/<n>/`.
pub fn merge(dirs: &[PathBuf], out: &Path) -> Result<Exploration, String> {
    let mut parts: Vec<Part> = dirs
        .iter()
        .map(|d| read_part(d))
        .collect::<Result<_, _>>()?;
    parts.sort_by_key(|p| (p.lo == 0, p.lo));
    check_same(&parts)?;
    let a = parts[0].args.clone();
    let explored: Vec<(u64, u64)> = parts
        .iter()
        .filter(|p| p.lo != 0)
        .map(|p| (p.lo, p.hi))
        .collect();
    let (rlo, rhi) = match parts[0].doc.config.shard.as_ref() {
        Some(s) => hydo_manifest::ci::parse_seeds(&s.range).unwrap_or((1, 1)),
        None => (
            explored.first().map(|x| x.0).unwrap_or(1),
            explored.last().map(|x| x.1).unwrap_or(1),
        ),
    };
    let range = format!("{rlo}-{rhi}");
    let mut missing: Vec<(u64, u64)> = Vec::new();
    let mut next = rlo;
    for (lo, hi) in &explored {
        if *lo > next {
            missing.push((next, lo - 1));
        }
        next = next.max(hi + 1);
    }
    if next <= rhi {
        missing.push((next, rhi));
    }

    std::fs::create_dir_all(out.join("consoles")).map_err(|e| format!("{}: {e}", out.display()))?;

    // the failures in seed order, and the ones one exploration would shrink
    let failures: Vec<(usize, Failure)> = parts
        .iter()
        .enumerate()
        .flat_map(|(i, p)| p.doc.failures.iter().map(move |f| (i, f.clone())))
        .collect();
    let mut reps: Vec<((String, String), Vec<String>)> = Vec::new();
    let mut chosen: Vec<(usize, i64)> = Vec::new();
    for (i, f) in &failures {
        let class = f.run.class.clone().unwrap_or_default();
        let key = (
            crate::advice::shrink_group(&class).to_string(),
            f.signature.clone(),
        );
        let cause = fault_kinds(&f.run.schedule);
        let pick = match reps.iter_mut().find(|(k, _)| *k == key) {
            None => {
                reps.push((key, vec![cause]));
                true
            }
            Some((_, cs)) => {
                let more = cs.len() < a.shrink_causes as usize && !cs.contains(&cause);
                if more {
                    cs.push(cause);
                }
                more
            }
        };
        if pick && !a.no_shrink && f.run.verdict.kind == "violation" {
            if let Some(sd) = f.run.seed {
                chosen.push((*i, sd));
            }
        }
    }
    // each kept shrink: its part, the part's chain number, and the merged number
    let mut kept: Vec<(usize, usize)> = Vec::new();
    for (i, sd) in &chosen {
        match parts[*i].doc.shrinks.iter().position(|s| s.from_seed == *sd) {
            Some(k) => kept.push((*i, k)),
            None => eprintln!(
                "hydo explore --merge: warning: {} did not shrink the failure of seed {sd}, which one exploration of the range shrinks",
                parts[*i].dir.display()
            ),
        }
    }
    let renames: Vec<HashMap<usize, usize>> = (0..parts.len())
        .map(|i| {
            kept.iter()
                .enumerate()
                .filter(|(_, (p, _))| *p == i)
                .map(|(g, (_, k))| (*k, g))
                .collect()
        })
        .collect();

    // the error kinds, and the new kinds one exploration would shrink
    let ek: Vec<&ErrorKinds> = parts
        .iter()
        .filter_map(|p| p.doc.error_kinds.as_ref())
        .collect();
    let mut error_kinds = (!ek.is_empty()).then(|| merge_kinds(&ek));
    let mut err_from: Vec<(String, usize)> = Vec::new();
    if let Some(k) = error_kinds.as_mut() {
        let agg: Vec<Aggregate> = k.kinds.iter().map(Aggregate::from_model).collect();
        if !a.no_shrink && a.errors_shrink > 0 {
            for x in errlog::to_shrink(&agg)
                .into_iter()
                .take(a.errors_shrink as usize)
            {
                let seed = x.first_new.as_ref().and_then(|f| f.0.as_i64()).unwrap_or(0);
                let Some(pi) = part_of(&parts, seed) else {
                    continue;
                };
                let shrink = parts[pi]
                    .doc
                    .error_kinds
                    .as_ref()
                    .and_then(|e| e.kinds.iter().find(|q| q.signature == x.sig))
                    .and_then(|q| q.shrink.clone());
                match shrink {
                    Some(s) => {
                        if let Some(q) = k.kinds.iter_mut().find(|q| q.signature == x.sig) {
                            q.shrink = Some(s);
                        }
                        err_from.push((x.sig.clone(), pi));
                    }
                    None => eprintln!(
                        "hydo explore --merge: warning: {} did not shrink the error kind {}, which one exploration of the range shrinks",
                        parts[pi].dir.display(),
                        x.sig
                    ),
                }
            }
        }
    }

    // the stream, in the order one exploration writes it
    let mut ev = String::new();
    let start: Start = parts[0]
        .lines
        .iter()
        .find(|(_, v)| kind(v).0 == "start")
        .and_then(|(_, v)| serde_json::from_value(v.clone()).ok())
        .ok_or_else(|| format!("{}: no start record", parts[0].dir.display()))?;
    let mut config = parts[0].doc.config.clone();
    config.seeds = Some(range.clone());
    config.shard = None;
    config.argv = merged_argv(&config.argv, &range, out);
    let mut engine = parts[0].doc.engine.clone();
    engine.workers = Some(parts.iter().filter_map(|p| p.doc.engine.workers).sum());
    let cpus: Vec<_> = parts
        .iter()
        .map(|p| p.doc.engine.host_cpu.clone())
        .collect();
    if cpus.iter().any(|c| *c != cpus[0]) {
        engine.host_cpu = None;
        let vendors: Vec<String> = {
            let mut v: Vec<String> = cpus
                .iter()
                .map(|c| {
                    c.as_ref()
                        .map(|c| c.vendor.clone())
                        .unwrap_or_else(|| "?".into())
                })
                .collect();
            v.sort();
            v.dedup();
            v
        };
        if vendors.len() > 1 {
            eprintln!(
                "hydo explore --merge: the parts ran on CPUs of {} vendors ({}): each failure was re-run on its own machine, and fingerprints are compared only there",
                vendors.len(),
                vendors.join(", ")
            );
        }
    }
    ev.push_str(&line_of(&Start {
        config: config.clone(),
        engine: engine.clone(),
        ..start
    }));
    // what the first part wrote before its first runs (the flush-count probe of --more-faults):
    // every part made the same runs
    for (l, v) in &parts[0].lines {
        let (ty, ro, _, _) = kind(v);
        if ty == "start" {
            continue;
        }
        if ro == role::FIRST {
            break;
        }
        ev.push_str(l);
    }
    for ro in [role::FIRST, role::RERUN] {
        for p in &parts {
            for (l, v) in &p.lines {
                if kind(v).1 == ro {
                    ev.push_str(l);
                }
            }
        }
    }
    for (g, (pi, k)) in kept.iter().enumerate() {
        let p = &parts[*pi];
        let sd = p.doc.shrinks[*k].from_seed;
        for (l, v) in &p.lines {
            let (ty, ro, seed, _) = kind(v);
            if ty == "run" && (ro == role::SHRINK || ro == role::MINIMAL) && seed == Some(sd) {
                if *k == g {
                    ev.push_str(l);
                } else {
                    let mut v = v.clone();
                    rename_value(&mut v, &renames[*pi]);
                    let run: Run = serde_json::from_value(v).map_err(|e| e.to_string())?;
                    ev.push_str(&line_of(&run));
                }
            }
        }
    }
    for (sig, pi) in &err_from {
        let (a1, a2, a3) = (
            format!("errshrink-{sig}-"),
            format!("errmin-a-{sig}"),
            format!("errmin-b-{sig}"),
        );
        for (l, v) in &parts[*pi].lines {
            let (ty, ro, _, tag) = kind(v);
            if ty == "run"
                && (ro == role::ERROR_SHRINK || ro == role::ERROR_MINIMAL)
                && (tag.starts_with(&a1) || tag == a2 || tag == a3)
            {
                ev.push_str(l);
            }
        }
    }
    let mut shrinks: Vec<Shrink> = Vec::new();
    for (pi, k) in &kept {
        let mut v = serde_json::to_value(&parts[*pi].doc.shrinks[*k]).map_err(|e| e.to_string())?;
        rename_value(&mut v, &renames[*pi]);
        shrinks.push(serde_json::from_value(v).map_err(|e| e.to_string())?);
    }
    for s in &shrinks {
        ev.push_str(&line_of(s));
    }

    // the summary
    let sums = |f: &dyn Fn(&Summary) -> Option<u64>| -> Option<u64> {
        let v: Vec<u64> = parts.iter().filter_map(|p| f(&p.doc.summary)).collect();
        (!v.is_empty()).then(|| v.iter().sum())
    };
    let firsts: Vec<f64> = parts
        .iter()
        .flat_map(|p| p.lines.iter())
        .filter(|(_, v)| kind(v).1 == role::FIRST)
        .map(|(_, v)| v.get("secs").and_then(Value::as_f64).unwrap_or(0.0))
        .collect();
    let schedules: u64 = parts.iter().map(|p| p.doc.summary.schedules).sum();
    let wall = parts
        .iter()
        .filter_map(|p| p.doc.summary.sweep_wall_s)
        .fold(0.0, f64::max);
    let total = parts
        .iter()
        .filter_map(|p| p.doc.summary.total_wall_s)
        .fold(0.0, f64::max);
    let nw: u64 = parts
        .iter()
        .map(|p| p.doc.engine.workers.unwrap_or(1))
        .sum();
    let boot = parts
        .iter()
        .map(|p| {
            p.doc.summary.worker_boot_s.unwrap_or(0.0) * p.doc.engine.workers.unwrap_or(1) as f64
        })
        .sum::<f64>()
        / nw.max(1) as f64;
    let failures: Vec<Failure> = failures.into_iter().map(|(_, f)| f).collect();
    let mut summary = Summary {
        schedules,
        sweep_wall_s: Some(round1(wall)),
        runs_per_hour: Some(round1(schedules as f64 / wall.max(1e-9) * 3600.0)),
        mean_run_s: Some(round1(
            firsts.iter().sum::<f64>() / firsts.len().max(1) as f64,
        )),
        worker_boot_s: Some(round1(boot)),
        total_wall_s: Some(round1(total)),
        failures: failures.len() as u64,
        reproduced: Some(failures.iter().filter(|f| f.reproduced).count() as u64),
        repro_runs: parts[0].doc.summary.repro_runs,
        quiet_wait_s: {
            let q: f64 = parts
                .iter()
                .filter_map(|p| p.doc.summary.quiet_wait_s)
                .sum();
            (q > 0.0).then(|| round1(q))
        },
        cache_hits: sums(&|s| s.cache_hits),
        runs_with_new_error_kinds: sums(&|s| s.runs_with_new_error_kinds),
        vacuous_passes: sums(&|s| s.vacuous_passes),
        refused: sums(&|s| s.refused),
        ..Summary::default()
    };
    if let Some(k) = &error_kinds {
        summary.error_kinds_new = Some(k.kinds.iter().filter(|x| x.new_runs > 0).count() as u64);
        summary.runs_with_new_error_kinds.get_or_insert(0);
    }
    let limits: Vec<&TimeLimit> = parts
        .iter()
        .filter_map(|p| p.doc.summary.time_limit.as_ref())
        .collect();
    if !limits.is_empty() {
        summary.time_limit = Some(TimeLimit {
            limit_s: limits[0].limit_s,
            planned: limits.iter().map(|t| t.planned).sum(),
            explored: limits.iter().map(|t| t.explored).sum(),
            explored_seeds: (!explored.is_empty()).then(|| ranges_text(&explored)),
            cut: limits.iter().any(|t| t.cut),
        });
    }
    ev.push_str(&line_of(&End {
        summary: summary.clone(),
        ..End::default()
    }));

    let tables: Vec<&PropertyTable> = parts
        .iter()
        .filter_map(|p| p.doc.properties.as_ref())
        .collect();
    let lv: Vec<&[Liveness]> = parts.iter().map(|p| p.doc.liveness.as_slice()).collect();
    let shards: Vec<MergedShard> = parts
        .iter()
        .map(|p| MergedShard {
            index: p.doc.config.shard.as_ref().map(|s| s.index),
            seeds: p.doc.config.seeds.clone().unwrap_or_default(),
            schedules: p.doc.summary.schedules,
            failures: p.doc.summary.failures,
            host_cpu: p.doc.engine.host_cpu.clone(),
            workers: p.doc.engine.workers,
            sweep_wall_s: p.doc.summary.sweep_wall_s,
            total_wall_s: p.doc.summary.total_wall_s,
            dir: Some(p.dir.display().to_string()),
        })
        .collect();
    let doc = Exploration {
        config,
        engine,
        summary,
        failures,
        shrinks,
        error_kinds,
        properties: merge_properties(&tables),
        liveness: merge_liveness(&lv),
        paths: merge_paths(
            &parts
                .iter()
                .map(|p| p.doc.paths.as_ref())
                .collect::<Vec<_>>(),
        ),
        shards,
        missing_seeds: (!missing.is_empty()).then(|| ranges_text(&missing)),
        ..parts[0].doc.clone()
    };
    if let Some(m) = &doc.missing_seeds {
        eprintln!(
            "hydo explore --merge: warning: no part explored the seeds {m} of {range}; the merged exploration has {} schedules",
            doc.summary.schedules
        );
    }

    // the files
    let w = |f: &str, t: &str| {
        std::fs::write(out.join(f), t).map_err(|e| format!("{}: {e}", out.join(f).display()))
    };
    w(hydo_results::EVENTS, &ev)?;
    hydo_results::write_document(&out.join(hydo_results::EXPLORATION_DOC), &doc)
        .map_err(|e| format!("{}: {e}", out.display()))?;
    w("report.md", &md::report(&doc))?;
    for (pi, p) in parts.iter().enumerate() {
        let own: Vec<String> = err_from
            .iter()
            .filter(|(_, i)| *i == pi)
            .map(|(s, _)| s.clone())
            .collect();
        if let Ok(rd) = std::fs::read_dir(p.dir.join("consoles")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let (stem, suffix) = name.split_once('.').unwrap_or((&name, ""));
                let target = if stem.starts_with("seed-") || stem.starts_with("repro") {
                    Some(name.clone())
                } else if chain_of(stem).is_some() {
                    rename_tag(stem, &renames[pi]).map(|t| {
                        if suffix.is_empty() {
                            t
                        } else {
                            format!("{t}.{suffix}")
                        }
                    })
                } else if stem.starts_with("errshrink-") || stem.starts_with("errmin-") {
                    own.iter()
                        .any(|s| stem.ends_with(s.as_str()) || stem.contains(&format!("-{s}-")))
                        .then(|| name.clone())
                } else {
                    (pi == 0).then(|| name.clone())
                };
                if let Some(t) = target {
                    copy_all(&e.path(), &out.join("consoles").join(t))
                        .map_err(|e| format!("{}: {e}", out.display()))?;
                }
            }
        }
        // the rest of the part's folder: each failure's replay and the like (first part wins),
        // and the part's own documents and worker logs under parts/<n>/
        let mine = out.join("parts").join((pi + 1).to_string());
        std::fs::create_dir_all(&mine).map_err(|e| format!("{}: {e}", mine.display()))?;
        if let Ok(rd) = std::fs::read_dir(&p.dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let r = match name.as_str() {
                    "consoles" | "parts" => Ok(()),
                    hydo_results::EXPLORATION_DOC
                    | hydo_results::STATS_DOC
                    | "report.md"
                    | "summary.txt"
                    | "host.txt" => copy_all(&e.path(), &mine.join(&name)),
                    hydo_results::EVENTS => Ok(()),
                    n if n.starts_with("worker-") => copy_all(&e.path(), &mine.join(&name)),
                    _ => copy_all(&e.path(), &out.join(&name)),
                };
                r.map_err(|e| format!("{}: {e}", out.display()))?;
            }
        }
    }
    // Milestone 336: the statistics of the whole range, from the parts' own
    let stats: Vec<hydo_results::StatsDoc> = parts
        .iter()
        .filter_map(|p| hydo_results::stats_of(&p.dir))
        .collect();
    if stats.len() < parts.len() {
        eprintln!(
            "hydo explore --merge: warning: {} of {} parts have no {} (an explorer before Milestone 336); the merged statistics count the others",
            parts.len() - stats.len(),
            parts.len(),
            hydo_results::STATS_DOC
        );
    }
    if let Some(s) = crate::stats::merge(&stats, doc.summary.total_wall_s.unwrap_or(0.0)) {
        hydo_results::write_document(&out.join(hydo_results::STATS_DOC), &s)
            .map_err(|e| format!("{}: {e}", out.display()))?;
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_tags_are_renamed_by_their_number() {
        let m: HashMap<usize, usize> = [(0, 2), (1, 0)].into_iter().collect();
        assert_eq!(rename_tag("shrink-3-1", &m).as_deref(), Some("shrink3-3-1"));
        assert_eq!(rename_tag("min-a", &m).as_deref(), Some("min3-a"));
        assert_eq!(rename_tag("shrink2-1-0", &m).as_deref(), Some("shrink-1-0"));
        assert_eq!(rename_tag("min2-b", &m).as_deref(), Some("min-b"));
        assert_eq!(rename_tag("shrink4-1-0", &m), None);
        assert_eq!(rename_tag("seed-4", &m), None);
        assert_eq!(rename_tag("errmin-a-abc", &m), None);
        let mut v = serde_json::json!({"tag": "min-a", "artifacts": {"consoles": ["consoles/min-a.log", "consoles/min-a.recovery.log"]}});
        rename_value(&mut v, &m);
        assert_eq!(
            v,
            serde_json::json!({"tag": "min3-a", "artifacts": {"consoles": ["consoles/min3-a.log", "consoles/min3-a.recovery.log"]}})
        );
    }

    #[test]
    fn the_merged_command_line_names_the_whole_range() {
        let s = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let argv = s(&[
            "--image", "i.img", "--seeds", "1-40", "--shard", "2/8", "--out", "o2", "--timing",
        ]);
        assert_eq!(
            merged_argv(&argv, "1-40", Path::new("m")),
            s(&["--image", "i.img", "--seeds", "1-40", "--out", "m", "--timing"])
        );
        assert_eq!(
            same_options(&s(&[
                "--seeds=6-10",
                "--workers",
                "3",
                "--shard=2/8",
                "--timing"
            ])),
            s(&["--timing"])
        );
    }

    #[test]
    fn ranges_join_where_they_touch() {
        assert_eq!(ranges_text(&[(6, 10), (1, 5), (21, 30)]), "1-10,21-30");
        assert_eq!(ranges_text(&[(1, 40)]), "1-40");
    }

    #[test]
    fn the_slowest_run_ties_go_to_the_first_console_name() {
        let a = (Some(2.0), Some("seed-2".to_string()));
        let b = (Some(2.0), Some("seed-10".to_string()));
        assert_eq!(slowest(a.clone(), b.clone()).1.as_deref(), Some("seed-10"));
        assert_eq!(slowest(b.clone(), a.clone()).1.as_deref(), Some("seed-10"));
        assert_eq!(slowest((Some(3.0), Some("seed-9".into())), b).0, Some(3.0));
    }

    #[test]
    fn the_error_paths_add_up_and_keep_the_earliest_first_run() {
        use hydo_results::{PathFirst, PathPlaceCoverage};
        let place = |runs, first: Option<i64>| PathPlaceCoverage {
            id: 7,
            kind: "err".into(),
            file: "a.go".into(),
            line: 3,
            function: "f".into(),
            runs,
            runs_without_faults: 0,
            first: first.map(|seed| PathFirst {
                seed,
                tag: format!("seed-{seed}"),
                schedule: "disk=none | run K=s@1+1".into(),
                at: 9,
            }),
            unwatched_runs: 1,
        };
        let a = PathCoverage {
            runs: 5,
            runs_without_faults: 1,
            places: vec![place(0, None)],
        };
        let b = PathCoverage {
            runs: 4,
            runs_without_faults: 2,
            places: vec![place(2, Some(8))],
        };
        let c = PathCoverage {
            runs: 3,
            runs_without_faults: 0,
            places: vec![place(1, Some(12))],
        };
        let m = merge_paths(&[Some(&a), None, Some(&b), Some(&c)]).unwrap();
        assert_eq!((m.runs, m.runs_without_faults), (12, 3));
        assert_eq!((m.places[0].runs, m.places[0].unwatched_runs), (3, 3));
        assert_eq!(m.places[0].first.as_ref().unwrap().seed, 8);
        assert!(merge_paths(&[None, None]).is_none());
    }
}
