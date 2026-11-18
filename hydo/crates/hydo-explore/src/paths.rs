//! Error paths on the host (Milestone 306, docs/design/error-paths.md): which error-handling
//! places of the image's programs a run reached, from the engine's line in the run's records,
//! and across an exploration's first runs, which places ran, how often, first in which schedule,
//! and which places only runs with faults reached.

use crate::pool::Res;
use hydo_contracts::paths::{kind_words, short_function, Hits};
use hydo_contracts::plan::Plan;
use hydo_results::{PathCoverage, PathFirst, PathPlaceCoverage, PathReached, RunPaths};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// The places of a plan.
pub fn places(plan: &Plan) -> usize {
    plan.programs.iter().map(|p| p.paths.len()).sum()
}

/// A run's error paths from its records (`<tag>.contracts.jsonl`).
pub fn run_paths(plan: &Plan, records: &Path) -> Result<RunPaths, String> {
    let hits: Option<Hits> = hydo_contracts::paths::read_hits(records)?;
    Ok(match hits {
        Some(h) => RunPaths {
            places: places(plan) as u64,
            reached: h
                .list()
                .into_iter()
                .map(|x| PathReached {
                    place: x.place,
                    at: x.at,
                    boot: x.boot,
                })
                .collect(),
            unwatched: h.unwatched,
            watched: true,
        },
        None => RunPaths {
            places: places(plan) as u64,
            watched: false,
            ..RunPaths::default()
        },
    })
}

/// Fold a run's error paths into its result under [`crate::results::PATHS`] (a plan without
/// places: nothing).
pub fn fold(plan: &Plan, records: &Path, res: &mut Res) -> Result<(), String> {
    if places(plan) == 0 {
        return Ok(());
    }
    let rp = run_paths(plan, records)?;
    res.insert(
        crate::results::PATHS.into(),
        serde_json::to_value(rp).expect("run paths serialize"),
    );
    Ok(())
}

/// A run's error paths from its result.
pub fn of_res(r: &Res) -> Option<RunPaths> {
    r.get(crate::results::PATHS)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Whether a schedule has a fault (any fault token: [`crate::run::fault_kinds`] is not `none`).
pub fn has_faults(schedule: &str) -> bool {
    crate::run::fault_kinds(schedule) != "none"
}

/// The error paths across first runs: (seed, tag, schedule, the run's paths) in seed order.
pub fn coverage(plan: &Plan, runs: &[(i64, String, String, Option<RunPaths>)]) -> PathCoverage {
    let mut out = PathCoverage::default();
    let mut ix: HashMap<u32, usize> = HashMap::new();
    for p in &plan.programs {
        for pl in &p.paths {
            ix.insert(pl.id, out.places.len());
            out.places.push(PathPlaceCoverage {
                id: pl.id,
                kind: pl.kind.clone(),
                file: pl.file.clone(),
                line: pl.line,
                function: pl.function.clone(),
                ..PathPlaceCoverage::default()
            });
        }
    }
    for (seed, tag, schedule, rp) in runs {
        let Some(rp) = rp.as_ref().filter(|r| r.watched) else {
            continue;
        };
        out.runs += 1;
        let clean = !has_faults(schedule);
        out.runs_without_faults += clean as u64;
        for r in &rp.reached {
            let Some(&k) = ix.get(&r.place) else { continue };
            let c = &mut out.places[k];
            c.runs += 1;
            c.runs_without_faults += clean as u64;
            if c.first.is_none() {
                c.first = Some(PathFirst {
                    seed: *seed,
                    tag: tag.clone(),
                    schedule: schedule.clone(),
                    at: r.at,
                });
            }
        }
        for u in &rp.unwatched {
            if let Some(&k) = ix.get(u) {
                out.places[k].unwatched_runs += 1;
            }
        }
    }
    out
}

/// The exploration's error paths from its first runs' results (`res_all`: tag -> (schedule,
/// result)), for the seeds in order.
pub fn of_exploration(
    plan: &Plan,
    seeds: &[i64],
    res_all: &HashMap<String, (String, Res)>,
) -> Option<PathCoverage> {
    if places(plan) == 0 {
        return None;
    }
    let runs: Vec<(i64, String, String, Option<RunPaths>)> = seeds
        .iter()
        .filter_map(|sd| {
            let tag = format!("seed-{sd}");
            let (text, r) = res_all.get(&tag)?;
            Some((*sd, tag, text.clone(), of_res(r)))
        })
        .collect();
    Some(coverage(plan, &runs))
}

/// One line per run for people: how many places ran.
pub fn run_line(rp: &RunPaths) -> String {
    if !rp.watched {
        return format!(
            "hydo: error paths: this engine does not watch them (the Mac engine does); the plan \
             has {} places",
            rp.places
        );
    }
    let mut s = format!(
        "hydo: error paths: {} of {} places ran (`hydo report --paths <run dir>` lists them)",
        rp.reached.len(),
        rp.places
    );
    if !rp.unwatched.is_empty() {
        s.push_str(&format!(
            "; {} places were not watched: the engine did not find their code in guest memory",
            rp.unwatched.len()
        ));
    }
    s
}

/// The text view of an exploration's (or one run's) error paths: what ran, and the places never
/// reached by function. `against`: another coverage (an exploration without faults), whose
/// places that ran are left out of the first list, so that it shows what only this one reached.
pub fn text(c: &PathCoverage, against: Option<&PathCoverage>) -> Vec<String> {
    let mut out = Vec::new();
    let ran: Vec<&PathPlaceCoverage> = c.places.iter().filter(|p| p.runs > 0).collect();
    out.push(format!(
        "{} of {} error paths ran in {} runs ({} of them without a fault).",
        ran.len(),
        c.places.len(),
        c.runs,
        c.runs_without_faults
    ));
    if c.runs == 0 {
        out.push(
            "No run recorded its error paths: the Mac engine watches them, and hvx and KVM do \
             not yet."
                .into(),
        );
        return out;
    }
    let other: HashMap<u32, u64> = against
        .map(|a| a.places.iter().map(|p| (p.id, p.runs)).collect())
        .unwrap_or_default();
    let only_faults: Vec<&&PathPlaceCoverage> = ran
        .iter()
        .filter(|p| p.runs_without_faults == 0 && other.get(&p.id).copied().unwrap_or(0) == 0)
        .collect();
    out.push(String::new());
    out.push(match against {
        Some(_) => format!(
            "Ran only with faults ({}): no run without a fault here or in the other \
             exploration reached them.",
            only_faults.len()
        ),
        None => format!(
            "Ran only with faults ({}): no run without a fault reached them.",
            only_faults.len()
        ),
    });
    for p in &only_faults {
        out.push(format!("  {}", place_line(p)));
    }
    let also: Vec<&&PathPlaceCoverage> = ran
        .iter()
        .filter(|p| !(p.runs_without_faults == 0 && other.get(&p.id).copied().unwrap_or(0) == 0))
        .collect();
    out.push(String::new());
    out.push(format!(
        "Ran also without faults ({}): the workload reaches them by itself.",
        also.len()
    ));
    for p in &also {
        out.push(format!("  {}", place_line(p)));
    }
    // never reached, by function
    let mut by_fn: BTreeMap<String, Vec<&PathPlaceCoverage>> = BTreeMap::new();
    for p in c.places.iter().filter(|p| p.runs == 0) {
        by_fn
            .entry(short_function(&p.function).to_string())
            .or_default()
            .push(p);
    }
    let never: usize = by_fn.values().map(Vec::len).sum();
    out.push(String::new());
    out.push(format!(
        "Never reached ({never} in {} functions), by function:",
        by_fn.len()
    ));
    for (f, ps) in &by_fn {
        let lines: Vec<String> = ps
            .iter()
            .map(|p| format!("{}:{}", p.file, p.line))
            .collect();
        let watched = if ps.iter().any(|p| p.unwatched_runs > 0) {
            " (not watched in some runs)"
        } else {
            ""
        };
        out.push(format!(
            "  {} ({} place{}){watched}: {}",
            if f.is_empty() { "?" } else { f.as_str() },
            ps.len(),
            if ps.len() == 1 { "" } else { "s" },
            lines.join(", ")
        ));
    }
    out
}

/// One place that ran: where, what, in how many runs, and the first schedule.
pub fn place_line(p: &PathPlaceCoverage) -> String {
    let first = p
        .first
        .as_ref()
        .map(|f| format!("; first in {} \"{}\"", f.tag, f.schedule))
        .unwrap_or_default();
    format!(
        "{}:{} {} ({}): {} run{}{first}",
        p.file,
        p.line,
        short_function(&p.function),
        kind_words(&p.kind),
        p.runs,
        if p.runs == 1 { "" } else { "s" }
    )
}

/// The coverage of one run's result (a `hydo run` directory): one run, its schedule.
pub fn of_run(plan: &Plan, schedule: &str, rp: RunPaths) -> PathCoverage {
    coverage(plan, &[(0, "run".into(), schedule.to_string(), Some(rp))])
}

/// The paths of a result map's JSON value (`result.json`'s `paths`).
pub fn of_value(v: &Value) -> Option<RunPaths> {
    serde_json::from_value(v.clone()).ok()
}
