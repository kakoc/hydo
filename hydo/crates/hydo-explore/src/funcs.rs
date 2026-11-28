//! Every function's first run on the host (Milestone 394, docs/design/live-map.md): which
//! functions of the image's programs a run reached, from the engine's `{"functions":...}` line
//! in the run's records, each placed at the workload's step range from the run's control
//! transcript, and marked when it ran before the schedule's first fault. The live map
//! ([`crate::live`]) reads them from the runs of `events.jsonl`, and the map of the program's
//! code from the plan ([`map`]).

use crate::pool::Res;
use hydo_contracts::paths::{short_function, Hits};
use hydo_contracts::plan::Plan;
use hydo_control::transcript::{Record, Transcript};
use hydo_results::{FunctionReached, RunFunctions};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// The result key of a run's functions (host only, until [`of_res`] makes them the run's
/// `functions`).
pub const FUNCTIONS: &str = "_functions";

/// The file of the map next to `exploration.json` and `events.jsonl`.
pub const MAP_FILE: &str = "function-map.json";

/// The functions a plan watches.
pub fn count(plan: &Plan) -> usize {
    plan.programs.iter().map(|p| p.fn_places.len()).sum()
}

/// The control transcript of the run conversation next to a run's records
/// (`<tag>.contracts.jsonl` -> `<tag>.control.jsonl`; a fresh-boot run of `hydo run` names it
/// `control.jsonl`).
fn transcript_of(records: &Path) -> Option<Transcript> {
    let name = records.file_name()?.to_str()?;
    let tag = name.strip_suffix(hydo_contracts::plan::RECORDS_SUFFIX)?;
    let text = std::fs::read_to_string(records.with_file_name(format!("{tag}.control.jsonl")))
        .or_else(|_| std::fs::read_to_string(records.with_file_name("control.jsonl")))
        .ok()?;
    Transcript::parse(&text).ok()
}

/// The step range of each clock value of the first boot: the first stop at or after the clock,
/// and the first `workload steps a..z` note at or after that stop.
fn range_of(t: &Transcript, ranges: &[(u64, u64, u64)], at: u64) -> Option<u32> {
    let stop = t.records.iter().find_map(|r| match r {
        Record::Stop { n, at: sa, .. } if *sa >= at => Some(*n),
        _ => None,
    })?;
    ranges
        .iter()
        .position(|&(_, _, n)| n >= stop)
        .map(|i| i as u32)
}

/// A run's functions from its records and its control transcript (None: no line of
/// functions, an engine that does not watch them).
pub fn run_functions(plan: &Plan, records: &Path) -> Result<RunFunctions, String> {
    let hits: Option<Hits> = hydo_contracts::paths::read_function_hits(records)?;
    let functions = count(plan) as u64;
    let Some(h) = hits else {
        return Ok(RunFunctions {
            functions,
            watched: false,
            ..RunFunctions::default()
        });
    };
    let t = transcript_of(records);
    let ranges = t
        .as_ref()
        .map(hydo_control::search::ranges)
        .unwrap_or_default();
    let reached = h
        .list()
        .into_iter()
        .map(|x| FunctionReached {
            function: x.place,
            at: x.at,
            boot: x.boot,
            range: match (&t, x.boot) {
                (Some(t), 0) => range_of(t, &ranges, x.at),
                _ => None,
            },
            before_faults: false,
        })
        .collect();
    Ok(RunFunctions {
        functions,
        reached,
        unwatched: h.unwatched,
        ranges: ranges.iter().map(|&(a, z, _)| (a, z)).collect(),
        first_fault_step: None,
        watched: true,
    })
}

/// Fold a run's functions into its result under [`FUNCTIONS`] (a plan without them: nothing).
pub fn fold(plan: &Plan, records: &Path, res: &mut Res) -> Result<(), String> {
    if count(plan) == 0 {
        return Ok(());
    }
    let rf = run_functions(plan, records)?;
    res.insert(
        FUNCTIONS.into(),
        serde_json::to_value(rf).expect("run functions serialize"),
    );
    Ok(())
}

/// The workload step of a schedule's first fault (None: no fault). A guest token's step is
/// the first number after its `@`, or its leading number (`T=12:-300`, `C=10`); a token whose
/// step this cannot read counts as step 0, as does a fault of the host part (a disk error,
/// tick jitter, `cf=`), which holds from the start. `disk=cut` only names how a power cut
/// treats the disk, and `W=` and `I=` are the workload's draws, not faults.
pub fn first_fault_step(text: &str) -> Option<u64> {
    let (host, guest) = text.split_once('|').unwrap_or((text, ""));
    let mut first: Option<u64> = None;
    let mut take = |s: u64| first = Some(first.map_or(s, |f: u64| f.min(s)));
    for w in host.split_whitespace() {
        let (k, v) = w.split_once('=').unwrap_or((w, ""));
        match (k, v) {
            ("dseed", _) => {}
            ("disk", "none" | "cut") => {}
            _ => take(0),
        }
    }
    for w in guest.split_whitespace().skip(1) {
        let Some((k, v)) = w.split_once('=') else {
            continue;
        };
        if k == "W" || k == "I" {
            continue;
        }
        let digits = |s: &str| -> Option<u64> {
            let d: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
            d.parse().ok()
        };
        let step = match v.find('@') {
            Some(i) => digits(&v[i + 1..]),
            None => digits(v),
        };
        take(step.unwrap_or(0));
    }
    first
}

/// A run's functions from its result, with each first run marked when it came before the
/// schedule's first fault.
pub fn of_res(r: &Res, schedule: &str) -> Option<RunFunctions> {
    let mut rf: RunFunctions = r
        .get(FUNCTIONS)
        .and_then(|v| serde_json::from_value(v.clone()).ok())?;
    let ff = first_fault_step(schedule);
    rf.first_fault_step = ff;
    for x in &mut rf.reached {
        x.before_faults = match ff {
            None => true,
            Some(f) => {
                x.boot == 0
                    && x.range
                        .and_then(|i| rf.ranges.get(i as usize))
                        .is_some_and(|&(_, z)| z <= f)
            }
        };
    }
    Some(rf)
}

/// One line per run for people: how many functions ran.
pub fn run_line(rf: &RunFunctions) -> String {
    if !rf.watched {
        return format!(
            "hydo: functions: this engine does not watch them (the Mac engine does); the plan \
             has {} functions",
            rf.functions
        );
    }
    let before = rf.reached.iter().filter(|x| x.before_faults).count();
    let mut s = format!(
        "hydo: functions: {} of {} functions ran, {before} of them before the first fault \
         (`hydo report --live <run dir>` shows them on a map)",
        rf.reached.len(),
        rf.functions
    );
    if !rf.unwatched.is_empty() {
        s.push_str(&format!(
            "; {} functions were not watched: the engine did not find their code in guest memory",
            rf.unwatched.len()
        ));
    }
    s
}

/// The map of the programs' code for the live map: per program, its modules, each with its
/// files and functions (id, name, short name, line, code size, own), and per function the
/// error paths written in it (id, line, kind). Modules and files are in name order, functions
/// by line.
pub fn map(plan: &Plan) -> Value {
    let mut programs = Vec::new();
    for p in &plan.programs {
        if p.fn_places.is_empty() {
            continue;
        }
        // the error paths of each function, by its full name
        let mut eps: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
        for pl in &p.paths {
            eps.entry(pl.function.as_str())
                .or_default()
                .push(json!([pl.id, pl.line, pl.kind]));
        }
        type Files<'a> = BTreeMap<&'a str, Vec<&'a hydo_contracts::paths::PathPlace>>;
        let mut mods: BTreeMap<&str, Files> = BTreeMap::new();
        for f in &p.fn_places {
            mods.entry(f.module.as_str())
                .or_default()
                .entry(f.file.as_str())
                .or_default()
                .push(f);
        }
        let modules: Vec<Value> = mods
            .into_iter()
            .map(|(m, files)| {
                let own = files.values().flatten().any(|f| f.own);
                let files: Vec<Value> = files
                    .into_iter()
                    .map(|(file, mut fs)| {
                        fs.sort_by_key(|f| (f.line, f.id));
                        let fns: Vec<Value> = fs
                            .iter()
                            .map(|f| {
                                let mut v = json!({
                                    "id": f.id,
                                    "name": f.function,
                                    "short": short_function(&f.function),
                                    "line": f.line,
                                    "size": f.size,
                                });
                                if f.own {
                                    v["own"] = json!(true);
                                }
                                if let Some(e) = eps.get(f.function.as_str()) {
                                    v["error_paths"] = json!(e);
                                }
                                v
                            })
                            .collect();
                        json!({"file": file, "functions": fns})
                    })
                    .collect();
                json!({"module": m, "own": own, "files": files})
            })
            .collect();
        programs.push(json!({
            "program": p.program,
            "functions": p.fn_places.len(),
            "error_paths": p.paths.len(),
            "modules": modules,
        }));
    }
    json!({"v": 1, "kind": "function-map", "programs": programs})
}

/// Write the map next to an exploration's files (`function-map.json`): a plan without
/// functions writes nothing.
pub fn write_map(plan: &Plan, dir: &Path) -> std::io::Result<bool> {
    if count(plan) == 0 {
        return Ok(false);
    }
    std::fs::write(
        dir.join(MAP_FILE),
        serde_json::to_string(&map(plan)).expect("the map serializes"),
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_fault_is_the_earliest_step_of_a_fault() {
        assert_eq!(first_fault_step("disk=none | run"), None);
        assert_eq!(first_fault_step("disk=none dseed=0 | run W=1a2b,p1"), None);
        assert_eq!(first_fault_step("disk=cut dseed=5 | run C=10"), Some(10));
        assert_eq!(
            first_fault_step("disk=none | run K=redis@21+1 N=worker-*@14-20:delay,100ms"),
            Some(14)
        );
        assert_eq!(first_fault_step("disk=none | run T=17:-3600"), Some(17));
        assert_eq!(first_fault_step("disk=ioerr:3 | run K=db@9+1"), Some(0));
        assert_eq!(
            first_fault_step("disk=none | run A=meili.swap_acked@20+14:kill-now-meilisearch"),
            Some(20)
        );
        assert_eq!(first_fault_step("disk=none | run M=master"), Some(0));
    }
}
