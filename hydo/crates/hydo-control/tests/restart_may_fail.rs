//! `services[].restart_may_fail:` (docs/HISTORY.md M136; M135 asked for it) on the scripted agent
//! and engine: a service whose restart at the heal fails.
//!
//! The plan has a database and a worker. The schedule kills the worker at the last step, so its
//! restart falls to the heal's restart pass; the worker's command carries `HYDO_FAKE_EXIT=`, so
//! the scripted guest ends that second process at once and the restart fails. Without the key the
//! run ends at once in `HYDO-ORACLE: ERROR restart after the faults (worker)` (the behaviour
//! hydo always had). With it the run goes on: a line says the worker stayed down, the workload's
//! retry is not run, the workload ends (the engine's DiskFaultsOff, where a `cf=` cut goes to its
//! recovery boot), and without a cut the verdict is the same ERROR naming the worker; the final
//! invariants do not run. A worker that does come back changes nothing. Each run is made twice
//! (one transcript) and typed again from its transcript (the same transcript).

use crate::support;

use hydo_control::{Ending, Plan};
use hydo_schedule::Schedule;
use support::{Conv, Item};

fn plan(may_fail: Option<bool>, exits: &str) -> Plan {
    let k = may_fail
        .map(|b| format!("    restart_may_fail: {b}\n"))
        .unwrap_or_default();
    let text = format!(
        "name: rmf-test
services:
  - name: db
    cmd: db-server
    ready: db-ping
  - name: worker
    cmd: worker-loop HYDO_FAKE_EXIT={exits}
    ready: \"true\"
{k}workload:
  steps: 4
  run: client
  retry: client-retry
invariants:
  - id: all.done
    final: true
    cmd: final-check
"
    );
    let m = hydo_manifest::from_embedded(&text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    Plan::from_manifest(&m).unwrap()
}

fn run(p: &Plan, sched: &str) -> support::Ran {
    let b = support::boot(p, "rmf-test").unwrap();
    let s = Schedule::parse(sched).unwrap();
    let a = support::run_schedule(&b, &s).unwrap();
    let again = support::run_schedule(&b, &s).unwrap();
    assert_eq!(
        a.run.fin.transcript, again.run.fin.transcript,
        "{sched}: two runs differ"
    );
    let mut reference = a.run.fin.transcript.clone();
    reference.header.controller = "replay".into();
    assert_eq!(
        a.replay.to_jsonl(),
        reference.to_jsonl(),
        "{sched}: the transcript typed again differs"
    );
    a
}

fn verdict(c: &Conv) -> String {
    match &c.fin.ending {
        Some(Ending::Verdict(v)) => v.clone(),
        e => panic!("not a verdict: {e:?}"),
    }
}

fn notes(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Note { line, .. } => Some(line.clone()),
            _ => None,
        })
        .collect()
}

/// The argv of every `run` command the controller sent (the workload's calls and the checks).
fn runs(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: hydo_agent_proto::msg::Command::Run(r),
                ..
            } => Some(r.argv.join(" ")),
            Item::Send {
                cmd: hydo_agent_proto::msg::Command::Spawn(s),
                ..
            } => Some(s.argv.join(" ")),
            _ => None,
        })
        .collect()
}

fn engine_actions(c: &Conv) -> usize {
    c.fin
        .transcript
        .records
        .iter()
        .filter(|r| matches!(r, hydo_control::Record::Engine { .. }))
        .count()
}

/// The worker is killed at the last step; its restart is the heal's.
const KILL_LAST: &str = "disk=none | run K=worker@3+5";
/// The worker's second process (the heal's restart) exits at once.
const FAILS: &str = "2/+0/1";

#[test]
fn without_the_key_a_failed_restart_is_an_error_at_once() {
    for k in [None, Some(false)] {
        let r = run(&plan(k, FAILS), KILL_LAST);
        let v = verdict(&r.run);
        assert_eq!(
            v, "HYDO-ORACLE: ERROR restart after the faults (worker)",
            "{k:?}"
        );
        // the run ends before the workload's end: no DiskFaultsOff, no retry
        assert_eq!(engine_actions(&r.run), 0, "{k:?}");
        assert!(!runs(&r.run.items)
            .iter()
            .any(|c| c.contains("client-retry")));
    }
    // restart_may_fail: false is the default, command for command
    let a = run(&plan(None, FAILS), KILL_LAST);
    let b = run(&plan(Some(false), FAILS), KILL_LAST);
    assert_eq!(a.run.fin.transcript, b.run.fin.transcript);
}

#[test]
fn with_the_key_the_run_goes_on_without_the_retry_and_ends_in_error() {
    let r = run(&plan(Some(true), FAILS), KILL_LAST);
    let ns = notes(&r.run.items);
    assert!(
        ns.iter().any(|n| n.starts_with(
            "hydo-ob: worker did not come back after the faults (restart_may_fail): no workload retry"
        )),
        "{ns:#?}"
    );
    let cmds = runs(&r.run.items);
    assert!(
        !cmds.iter().any(|c| c.contains("client-retry")),
        "the retry ran: {cmds:#?}"
    );
    // the workload ended (where a cf= cut moves the run into its recovery VM) ...
    assert_eq!(engine_actions(&r.run), 1);
    // ... and without a cut the verdict names the service; the final invariants did not run
    assert_eq!(
        verdict(&r.run),
        "HYDO-ORACLE: ERROR restart after the faults (worker)"
    );
    assert!(!cmds.iter().any(|c| c.contains("final-check")), "{cmds:#?}");
}

/// A kill whose restart point is the workload's end (`K=worker@3+1`, 4 steps): the K= restart
/// happens after the last step. The worker's second and third processes exit at once.
const KILL_TO_END: &str = "disk=none | run K=worker@3+1";
const FAILS_TWICE: &str = "2/+0/1,3/+0/1";

#[test]
fn a_kill_restart_at_the_workloads_end_follows_the_key() {
    // without the key: the K= restart's own ERROR, at once
    let r = run(&plan(None, FAILS_TWICE), KILL_TO_END);
    assert_eq!(verdict(&r.run), "HYDO-ORACLE: ERROR worker restart failed");
    assert_eq!(engine_actions(&r.run), 0);
    // with it: left to the heal, whose restart fails too; no retry, the workload's end, the ERROR
    let r = run(&plan(Some(true), FAILS_TWICE), KILL_TO_END);
    let ns = notes(&r.run.items);
    assert!(
        ns.iter().any(|n| n
            == "hydo-ob: worker restart failed after the last step (restart_may_fail): left to the heal"),
        "{ns:#?}"
    );
    assert!(ns
        .iter()
        .any(|n| n
            .starts_with("hydo-ob: worker did not come back after the faults (restart_may_fail)")));
    assert!(!runs(&r.run.items)
        .iter()
        .any(|c| c.contains("client-retry")));
    assert_eq!(engine_actions(&r.run), 1);
    assert_eq!(
        verdict(&r.run),
        "HYDO-ORACLE: ERROR restart after the faults (worker)"
    );
}

#[test]
fn with_the_key_a_restart_that_succeeds_changes_nothing() {
    // the worker's second process lives: the key is not used, the transcript is the default one
    let a = run(&plan(None, "9/+0/1"), KILL_LAST);
    let b = run(&plan(Some(true), "9/+0/1"), KILL_LAST);
    assert_eq!(a.run.fin.transcript, b.run.fin.transcript);
    let v = verdict(&a.run);
    assert!(!v.contains("restart after the faults"), "{v}");
    assert!(runs(&a.run.items)
        .iter()
        .any(|c| c.contains("client-retry")));
}

/// The worker stops on its own during the workload (its first process ends one unit after the
/// workload starts), and every restart of it ends at once.
const STOPS_ON_ITS_OWN: &str = "1/wl+1/134,2/+0/134,3/+0/134";

#[test]
fn a_service_that_stops_in_a_run_without_faults_is_named_and_the_checks_run() {
    // open item 86: before, this was `ERROR restart after the faults (worker)` and no check ran
    for k in [None, Some(true)] {
        let r = run(&plan(k, STOPS_ON_ITS_OWN), "disk=none | run");
        assert_eq!(
            verdict(&r.run),
            "HYDO-ORACLE: VIOLATION service stopped in a run without faults (worker)",
            "{k:?}"
        );
        let ns = notes(&r.run.items);
        assert!(
            ns.iter().any(|n| n
                == "hydo-ob: worker stopped in a run without faults and did not come back: no workload retry"),
            "{ns:#?}"
        );
        assert!(
            ns.iter()
                .any(|n| n.starts_with("hydo-ob: the checks with worker stopped: HYDO-ORACLE: ")),
            "{ns:#?}"
        );
        let cmds = runs(&r.run.items);
        assert!(cmds.iter().any(|c| c.contains("final-check")), "{cmds:#?}");
        assert!(
            !cmds.iter().any(|c| c.contains("client-retry")),
            "{cmds:#?}"
        );
        assert_eq!(engine_actions(&r.run), 1);
    }
    // a knob and timing tokens are no faults
    let r = run(&plan(None, STOPS_ON_ITS_OWN), "disk=none tj=1 | run Q=2");
    assert!(verdict(&r.run).contains("without faults (worker)"));
    // with a fault, even one on another service, the verdict is the one recorded so far
    for sched in ["disk=none | run K=db@1+1", "disk=ioerr:5 dseed=1 | run"] {
        let r = run(&plan(None, STOPS_ON_ITS_OWN), sched);
        assert_eq!(
            verdict(&r.run),
            "HYDO-ORACLE: ERROR restart after the faults (worker)",
            "{sched}"
        );
    }
}
