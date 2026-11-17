//! `hydo inspect values <run dir>` (Milestone 285, docs/design/value-history.md): the values
//! that functions' `// hydo: record` lines name, from the run's records and the image's plan,
//! per key, each stop placed at the run's stops and workload steps, and each change marked. It
//! reads the run's own files only (no VM).

use super::drive::{resolve, Conv, Recorded};
use super::{Args, Problem};
use hydo_contracts::plan::Plan;
use hydo_contracts::values::{histories, points, History};
use hydo_control::transcript::{Record, Transcript};
use hydo_results::{ValueChange, ValueHistory, ValueKey, ValuePoint};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The plan of the run's image (`<image>.contracts.json`), or the one given.
pub fn plan_of(rec: &Recorded, given: Option<&Path>) -> Result<(PathBuf, Plan), Problem> {
    let path = match given {
        Some(p) => p.to_path_buf(),
        None => {
            let image = resolve(&rec.image_text, &rec.dir).ok_or_else(|| {
                Problem::new(
                    "no-plan",
                    format!(
                        "the run's image {:?} is not there any more (result.json image.path), \
                         and the record plan is next to it",
                        rec.image_text
                    ),
                    vec![format!(
                        "hydo inspect values {} --plan <image>.contracts.json",
                        rec.dir.display()
                    )],
                )
            })?;
            hydo_contracts::plan::plan_path(&image)
        }
    };
    if !path.is_file() {
        return Err(Problem::new(
            "no-plan",
            format!(
                "{} is not there: the image has no `// hydo: record` lines (a manifest's \
                 `contracts:` entry names the program and its sources; `hydo build` writes the \
                 plan next to the image)",
                path.display()
            ),
            vec!["hydo guide values".into()],
        ));
    }
    let plan =
        hydo_contracts::plan::read_plan(&path).map_err(|e| Problem::new("no-plan", e, vec![]))?;
    Ok((path, plan))
}

/// Where a stop of the run was: the workload's step range, after the workload, or the recovery
/// boot.
fn step_of(t: Option<&Transcript>, conv: Conv, stop: Option<u64>) -> Option<String> {
    if conv == Conv::Recovery {
        return Some("the recovery boot".into());
    }
    let ranges = hydo_control::search::ranges(t?);
    let stop = stop?;
    if ranges.is_empty() {
        return None;
    }
    Some(match ranges.iter().find(|(_, _, n)| *n >= stop) {
        Some((a, z, _)) => format!("workload steps {a}..{}", z.saturating_sub(1)),
        None => "after the workload".into(),
    })
}

/// The first recorded stop at or after the engine's clock `at` (the last stop when the run
/// recorded none after it).
fn stop_at(t: Option<&Transcript>, at: u64) -> Option<u64> {
    let mut last = None;
    for r in &t?.records {
        if let Record::Stop { n, at: sa, .. } = r {
            last = Some(*n);
            if *sa >= at {
                return last;
            }
        }
    }
    last
}

/// The run's value history: every key, with its points placed at the run's stops.
pub fn keys(rec: &Recorded, plan: &Plan) -> Result<(u64, Vec<ValueKey>), Problem> {
    let p = rec.dir.join(rec.files.contracts());
    let (recs, _) =
        hydo_contracts::plan::read_run(&p).map_err(|e| Problem::new("no-run", e, vec![]))?;
    let pts = points(plan, &recs);
    let calls = recs
        .iter()
        .filter(|r| {
            plan.programs
                .iter()
                .flat_map(|p| &p.functions)
                .any(|f| f.id == r.f && !f.values.is_empty())
        })
        .count() as u64;
    let has_recovery = rec.transcript(Conv::Recovery).is_some();
    let out = histories(pts)
        .into_iter()
        .map(|h| key_doc(rec, h, has_recovery))
        .collect();
    Ok((calls, out))
}

fn key_doc(rec: &Recorded, h: History, has_recovery: bool) -> ValueKey {
    let points = h
        .points
        .iter()
        .map(|p| {
            let conv = if p.boot > 0 && has_recovery {
                Conv::Recovery
            } else {
                Conv::Run
            };
            let t = rec.transcript(conv);
            let stop = stop_at(t, p.at);
            ValuePoint {
                function: p.function.clone(),
                file: p.file.clone(),
                line: p.line,
                call: p.call,
                when: p.when.into(),
                key: p.key.clone().filter(|k| Some(k) != h.key.as_ref()),
                caller: p.caller.clone(),
                point: stop.map(|s| format!("{}:{s}", conv.name())),
                conversation: conv.name().into(),
                at: p.at,
                step: step_of(t, conv, stop),
                values: p
                    .values
                    .iter()
                    .map(|(n, v)| (n.clone(), v.to_string()))
                    .collect(),
            }
        })
        .collect();
    ValueKey {
        key: h.key,
        copies: h.copies,
        names: h.names,
        points,
        changes: h
            .changes
            .into_iter()
            .map(|c| ValueChange {
                name: c.name,
                from: c.from,
                to: c.to,
                point: c.point as u64,
                before: c.before as u64,
            })
            .collect(),
    }
}

/// A function's name without its package path (`blnk.setTransactionMetadata`).
pub fn short(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// `hydo inspect values <run dir> [--key K] [--name N] [--changes] [--plan FILE]`.
pub fn cmd(rest: &[String]) -> Result<Value, Problem> {
    let mut a = Args::new(rest);
    let key = a.val("--key")?;
    let name = a.val("--name")?;
    let changes = a.flag("--changes");
    let plan_file = a.val("--plan")?.map(PathBuf::from);
    let limit = a.num("--limit")?.unwrap_or(50);
    let offset = a.num("--offset")?.unwrap_or(0);
    let dir = a.one("a run directory")?;
    a.done()?;
    let rec = Recorded::load(Path::new(&dir))?;
    let (path, plan) = plan_of(&rec, plan_file.as_deref())?;
    let (calls, all) = keys(&rec, &plan)?;
    let matching: Vec<ValueKey> = all
        .into_iter()
        .filter(|k| {
            key.as_ref().is_none_or(|want| {
                k.key.as_ref() == Some(want) || k.copies.iter().any(|c| c == want)
            })
        })
        .filter(|k| name.as_ref().is_none_or(|n| k.names.contains(n)))
        .filter(|k| {
            !changes
                || k.changes
                    .iter()
                    .any(|c| name.as_ref().is_none_or(|n| *n == c.name))
        })
        .collect();
    let total = matching.len() as u64;
    let changed = matching.iter().filter(|k| !k.changes.is_empty()).count() as u64;
    let shown: Vec<ValueKey> = matching
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    let truncated = offset + (shown.len() as u64) < total;
    let next = truncated.then(|| {
        let mut w: Vec<String> = vec!["hydo inspect values".into(), dir.clone()];
        for (i, r) in rest.iter().enumerate() {
            if r == &dir || r == "--offset" || (i > 0 && rest[i - 1] == "--offset") {
                continue;
            }
            w.push(super::work::quote(r));
        }
        w.push(format!("--offset {}", offset + shown.len() as u64));
        w.join(" ")
    });
    let functions = plan
        .programs
        .iter()
        .flat_map(|p| &p.functions)
        .filter(|f| !f.values.is_empty())
        .map(|f| f.name.clone())
        .collect();
    let doc = ValueHistory {
        run_dir: rec.dir.display().to_string(),
        verdict: rec.doc.verdict.clone(),
        plan: path.display().to_string(),
        functions,
        calls,
        total,
        changed,
        offset,
        keys: shown,
        truncated,
        next,
        ..ValueHistory::default()
    };
    Ok(serde_json::to_value(&doc).unwrap_or(json!(null)))
}

/// One point as a line: where, which call, and the values (a changed value with what it was).
pub fn point_line(k: &ValueKey, i: usize) -> String {
    let p = &k.points[i];
    format!(
        "  {:<11} {}",
        p.point.clone().unwrap_or_else(|| "-".into()),
        point_text(k, i)
    )
}

/// One point without its place: the function, the call, and the values.
pub fn point_text(k: &ValueKey, i: usize) -> String {
    let p = &k.points[i];
    let mut s = format!("{} {}", short(&p.function), p.when);
    if let Some(own) = &p.key {
        s.push_str(&format!(" [{own}]"));
    }
    if let Some(c) = &p.caller {
        s.push_str(&format!(", called from {}", short(c)));
    }
    s.push(':');
    for n in &k.names {
        let Some(v) = p.values.get(n) else { continue };
        match k
            .changes
            .iter()
            .find(|c| c.point == i as u64 && &c.name == n)
        {
            Some(c) => s.push_str(&format!("  {n} = {v} (CHANGED from {})", c.from)),
            None => s.push_str(&format!("  {n} = {v}")),
        }
    }
    s
}

/// The `--text` view.
pub fn text(doc: &ValueHistory) -> String {
    let mut s = format!(
        "hydo inspect values: {} calls of {} recorded ({}); the run: {} {}\n",
        doc.calls,
        doc.functions
            .iter()
            .map(|f| short(f))
            .collect::<Vec<_>>()
            .join(", "),
        doc.plan,
        doc.verdict.kind,
        doc.verdict.text
    );
    s.push_str(&format!(
        "{} keys, {} of them with a change; {} shown\n",
        doc.total,
        doc.changed,
        doc.keys.len()
    ));
    for k in &doc.keys {
        s.push('\n');
        let mut head = match &k.key {
            Some(key) => format!("key {key:?}"),
            None => "values without a key".into(),
        };
        if !k.copies.is_empty() {
            head.push_str(&format!(" (and {})", k.copies.join(", ")));
        }
        let step = k.points.first().and_then(|p| p.step.clone());
        if let Some(st) = step {
            head.push_str(&format!(", first seen in {st}"));
        }
        s.push_str(&head);
        s.push('\n');
        for i in 0..k.points.len() {
            s.push_str(&point_line(k, i));
            s.push('\n');
        }
        for c in &k.changes {
            let p = &k.points[c.point as usize];
            let b = &k.points[c.before as usize];
            s.push_str(&format!(
                "  change: {} {} -> {} between {} {} ({}) and {} {} ({})\n",
                c.name,
                c.from,
                c.to,
                short(&b.function),
                b.when,
                b.point.clone().unwrap_or_default(),
                short(&p.function),
                p.when,
                p.point.clone().unwrap_or_default()
            ));
        }
    }
    if let Some(n) = &doc.next {
        s.push_str(&format!("\nmore: {n}\n"));
    }
    s
}

/// The run's value points as timeline events (kind `value`), for `hydo inspect events`: one
/// per stop of a recording function, with its key and values.
pub fn events(rec: &Recorded) -> Vec<(Conv, Option<u64>, hydo_results::TimelineEvent)> {
    let Ok((_, plan)) = plan_of(rec, None) else {
        return Vec::new();
    };
    let Ok((_, ks)) = keys(rec, &plan) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for k in &ks {
        for (i, p) in k.points.iter().enumerate() {
            let conv = if p.conversation == "recovery" {
                Conv::Recovery
            } else {
                Conv::Run
            };
            let stop = p
                .point
                .as_ref()
                .and_then(|x| x.split_once(':'))
                .and_then(|x| x.1.parse().ok());
            let who = k.key.clone().unwrap_or_else(|| "-".into());
            out.push((
                conv,
                stop,
                hydo_results::TimelineEvent {
                    point: p.point.clone(),
                    conversation: conv.name().into(),
                    stop,
                    at: Some(p.at),
                    kind: "value".into(),
                    text: format!("{who}: {}", point_text(k, i)),
                    service: None,
                    details: serde_json::to_value(p).ok(),
                },
            ));
        }
    }
    out
}
