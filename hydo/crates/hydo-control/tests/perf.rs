//! `perf:` (Milestone 237) on the scripted agent and engine: the workload runs one marked call
//! per step, each call gets `$HYDO_PERF`, and after the verdict the file's lines are reported.
//! Without `perf:` the run sends the same commands as before (one workload call).

use crate::support;

use hydo_agent_proto::msg::Command;
use hydo_control::{Ending, Plan};
use hydo_schedule::Schedule;
use support::Item;

fn plan(perf: &str) -> Plan {
    let text = format!(
        "name: kv\nservices:\n  - name: kv\n    cmd: kv-server\n    ready: kv-ping\n\
         workload:\n  steps: 3\n  run: client\n{perf}invariants:\n  - id: all.done\n    final: true\n    cmd: final-check\n"
    );
    let m = hydo_manifest::from_embedded(&text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    Plan::from_manifest(&m).unwrap()
}

/// The workload calls of a run: each spawn named `workload`, as `HYDO_FROM-HYDO_TO` and whether
/// it had `$HYDO_PERF`.
fn calls(items: &[Item]) -> Vec<(String, bool)> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Spawn(s),
                ..
            } if s.h == "workload" => {
                let env = s.env.clone().unwrap_or_default();
                let get = |k: &str| {
                    env.iter()
                        .find_map(|e| e.strip_prefix(&format!("{k}=")).map(str::to_string))
                        .unwrap_or_default()
                };
                Some((
                    format!("{}-{}", get("HYDO_FROM"), get("HYDO_TO")),
                    env.iter().any(|e| e.starts_with("HYDO_PERF=")),
                ))
            }
            _ => None,
        })
        .collect()
}

fn reports(t: &hydo_control::transcript::Transcript) -> Vec<String> {
    t.report_lines()
        .into_iter()
        .filter(|l| l.starts_with("HYDO-PERF"))
        .map(str::to_string)
        .collect()
}

#[test]
fn perf_runs_one_marked_call_per_step() {
    let p = plan("perf:\n  threshold: \"1.5\"\n");
    assert_eq!(
        p.perf.as_ref().map(|x| (x.steps, x.threshold_bp)),
        Some((true, 150))
    );
    let b = support::boot(&p, "kv").unwrap();
    let s = Schedule::parse("disk=none | run").unwrap();
    let a = support::run_schedule(&b, &s).unwrap();
    assert!(
        matches!(&a.run.fin.ending, Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE: OK")),
        "{:?}",
        a.run.fin.ending
    );
    assert_eq!(
        calls(&a.run.items),
        [
            ("0-1".into(), true),
            ("1-2".into(), true),
            ("2-3".into(), true)
        ]
    );
    let r = reports(&a.run.fin.transcript);
    assert_eq!(
        r,
        [
            "HYDO-PERF-MARK start 0 1",
            "HYDO-PERF-MARK end 0 1 rc=0",
            "HYDO-PERF-MARK start 1 2",
            "HYDO-PERF-MARK end 1 2 rc=0",
            "HYDO-PERF-MARK start 2 3",
            "HYDO-PERF-MARK end 2 3 rc=0",
        ]
    );
    // each step's end and the next step's start are at one stop, so the steps tile the workload
    let again = support::run_schedule(&b, &s).unwrap();
    assert_eq!(a.run.fin.transcript, again.run.fin.transcript);
}

#[test]
fn perf_without_steps_keeps_one_call_and_a_fault_splits_it() {
    let p = plan("perf:\n  steps: false\n");
    let b = support::boot(&p, "kv").unwrap();
    let a = support::run_schedule(&b, &Schedule::parse("disk=none | run").unwrap()).unwrap();
    assert_eq!(calls(&a.run.items), [("0-3".into(), true)]);
    // the kill before step 1 and the restart before step 2 split the workload there
    let s = Schedule::parse("disk=none | run K=kv@1+1").unwrap();
    let a = support::run_schedule(&b, &s).unwrap();
    assert_eq!(
        calls(&a.run.items),
        [
            ("0-1".into(), true),
            ("1-2".into(), true),
            ("2-3".into(), true)
        ]
    );
}

#[test]
fn without_perf_the_workload_is_one_call_without_the_file() {
    let p = plan("");
    assert_eq!(p.perf, None);
    let b = support::boot(&p, "kv").unwrap();
    let s = Schedule::parse("disk=none | run").unwrap();
    let a = support::run_schedule(&b, &s).unwrap();
    assert_eq!(calls(&a.run.items), [("0-3".into(), false)]);
    assert!(reports(&a.run.fin.transcript).is_empty());
}
