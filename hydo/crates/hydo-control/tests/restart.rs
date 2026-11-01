//! Restart policies (`services[].restart:`, docs/HISTORY.md M134) on the scripted agent and engine.
//!
//! The plan has a database and a worker; the worker's command carries `HYDO_FAKE_EXIT=`, so the
//! scripted guest ends its process at a chosen point (after the heal, during the workload, in the
//! recovery boot, right after a restart), and the `eventually` check `HYDO_FAKE_NEEDS=worker`
//! holds only once the faults are over and the worker runs (a worker that must count the votes).
//! Every policy is run, and for each the test checks what the controller did (who started the
//! worker when, its notes, the verdict), that a second run gives the same transcript and that
//! the transcript typed again gives it too. A plan without `restart:` (and one with `restart:
//! "no"`) gives today's behaviour: the worker that dies after the heal stays down. The identity
//! corpora (corpus.rs, features.rs, ...) prove that plans without a policy are unchanged.

mod support;

use hydo_control::{Ending, Plan, ScheduleController, Start, Transcript};
use hydo_schedule::Schedule;
use support::fake::Fake;
use support::{Conv, Item};

fn plan(restart: Option<&str>, exits: &str) -> Plan {
    plan_needing(restart, exits, "worker")
}

/// The plan with the check's `HYDO_FAKE_NEEDS=` word (`worker@5`: the worker's fifth process).
fn plan_needing(restart: Option<&str>, exits: &str, needs: &str) -> Plan {
    let r = restart
        .map(|r| format!("    restart: {r}\n"))
        .unwrap_or_default();
    let text = format!(
        "name: restart-test
services:
  - name: db
    cmd: db-server
    ready: db-ping
  - name: worker
    cmd: worker-loop HYDO_FAKE_EXIT={exits}
    ready: \"true\"
{r}workload:
  steps: 4
  run: client
invariants:
  - id: worker.counted
    type: eventually
    within: 20
    poll: 1
    cmd: count-check HYDO_FAKE_NEEDS={needs}
"
    );
    let m = hydo_manifest::from_embedded(&text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    Plan::from_manifest(&m).unwrap()
}

/// The worker's processes: (stop, who) for each spawn of the handle `worker`.
fn worker_spawns(items: &[Item]) -> Vec<(u64, String)> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                n,
                by,
                cmd: hydo_agent_proto::msg::Command::Spawn(s),
                ..
            } if s.h == "worker" => Some((*n, by.clone())),
            _ => None,
        })
        .collect()
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

fn verdict(c: &Conv) -> String {
    match &c.fin.ending {
        Some(Ending::Verdict(v)) => v.clone(),
        e => panic!("not a verdict: {e:?}"),
    }
}

/// The stop of the engine's DiskFaultsOff ("go").
fn go_stop(c: &Conv) -> u64 {
    c.fin
        .transcript
        .records
        .iter()
        .find_map(|r| match r {
            hydo_control::Record::Engine { n, .. } => Some(*n),
            _ => None,
        })
        .expect("the workload's end")
}

struct Case {
    run: support::Ran,
}

/// Boot, run the schedule from S1, run it again and type its transcript again.
fn run(p: &Plan, sched: &str) -> Case {
    let b = support::boot(p, "restart-test").unwrap();
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
    if let (Some(x), Some(y)) = (&a.recovery, &again.recovery) {
        assert_eq!(
            x.fin.transcript, y.fin.transcript,
            "{sched}: two recoveries"
        );
    }
    Case { run: a }
}

const AFTER_HEAL: &str = "1/go+5/1";

#[test]
fn a_service_that_dies_after_the_heal_stays_down_by_default() {
    for restart in [None, Some("\"no\""), Some("no")] {
        let c = run(&plan(restart, AFTER_HEAL), "disk=none | run");
        let r = &c.run.run;
        // the worker started at boot (S1's) is never started again in the run
        assert_eq!(worker_spawns(&r.items), vec![], "{restart:?}");
        let v = verdict(r);
        assert!(
            v.starts_with("HYDO-ORACLE: VIOLATION [worker.counted] not satisfied"),
            "{restart:?}: {v}"
        );
        assert!(v.contains("worker is down"), "{v}");
        assert!(
            !notes(&r.items).iter().any(|n| n.contains("restart:")),
            "{restart:?}"
        );
    }
    // restart: no is the default: the same transcript, command for command
    let a = run(&plan(None, AFTER_HEAL), "disk=none | run");
    let b = run(&plan(Some("\"no\""), AFTER_HEAL), "disk=none | run");
    assert_eq!(a.run.run.fin.transcript, b.run.run.fin.transcript);
}

#[test]
fn always_and_unless_stopped_restart_a_service_that_dies_after_the_heal() {
    for (pol, status) in [
        ("always", 1),
        ("always", 0),
        ("unless-stopped", 1),
        ("unless-stopped", 0),
    ] {
        let p = plan(Some(pol), &format!("1/go+5/{status}"));
        let c = run(&p, "disk=none | run");
        let r = &c.run.run;
        let sp = worker_spawns(&r.items);
        assert_eq!(sp.len(), 1, "{pol} {status}: {sp:?}");
        let (at, by) = &sp[0];
        assert_eq!(by, "restart:worker", "{pol}");
        assert!(*at > go_stop(r), "{pol}: restarted before the heal");
        let ns = notes(&r.items);
        let want = format!(
            "hydo-ob: worker exited (status {status}); restarting it in 100 ms (restart: {pol})"
        );
        assert!(ns.contains(&want), "{pol}: {ns:#?}");
        assert!(ns.contains(&format!(
            "hydo-ob: restarting worker (restart: {pol}, restart 1)"
        )));
        // the restart is a whole start_svc: the command and its readiness check
        assert!(ns
            .iter()
            .any(|n| n.starts_with("hydo-ob: worker ready after")));
        let v = verdict(r);
        assert!(v.starts_with("HYDO-ORACLE: OK"), "{pol} {status}: {v}");
        // the delay is a sleep in the guest, by the supervisor
        assert!(r.items.iter().any(|i| matches!(i,
            Item::Send { by, cmd: hydo_agent_proto::msg::Command::Run(x), .. }
                if by == "restart:worker" && x.argv.ends_with(&["sleep".into(), "0.100".into()]))));
    }
}

#[test]
fn on_failure_restarts_only_after_a_failure_and_at_most_max_times() {
    // a failure: restarted
    let c = run(&plan(Some("on-failure"), "1/go+5/3"), "disk=none | run");
    assert_eq!(worker_spawns(&c.run.run.items).len(), 1);
    assert!(verdict(&c.run.run).starts_with("HYDO-ORACLE: OK"));
    // a clean exit: not restarted, and the check sees it down
    let c = run(&plan(Some("on-failure"), "1/go+5/0"), "disk=none | run");
    let r = &c.run.run;
    assert_eq!(worker_spawns(&r.items), vec![]);
    assert!(notes(&r.items).contains(
        &"hydo-ob: worker exited (status 0); not restarted (restart: on-failure, exit status 0)"
            .to_string()
    ));
    assert!(verdict(r).starts_with("HYDO-ORACLE: VIOLATION [worker.counted]"));
    // on-failure:1: the first failure is restarted, the restarted worker's failure is not
    let c = run(
        &plan(Some("on-failure:1"), "1/go+5/1,2/+60/1"),
        "disk=none | run",
    );
    let r = &c.run.run;
    assert_eq!(worker_spawns(&r.items).len(), 1);
    let ns = notes(&r.items);
    assert!(ns.contains(
        &"hydo-ob: worker exited (status 1); not restarted (restart: on-failure:1, 1 restarts made)"
            .to_string()
    ), "{ns:#?}");
    assert!(verdict(r).starts_with("HYDO-ORACLE: VIOLATION [worker.counted]"));
    // on-failure:0 has no limit, as Docker's
    let c = run(
        &plan(Some("on-failure:0"), "1/go+5/1,2/+60/1,3/+60/1"),
        "disk=none | run",
    );
    assert_eq!(worker_spawns(&c.run.run.items).len(), 2);
}

#[test]
fn the_delay_doubles_for_a_crash_loop() {
    // every process dies 60 units (1.5 s of guest time) after it started: a short run each time
    let c = run(
        &plan_needing(
            Some("always"),
            "1/go+5/1,2/+60/1,3/+60/1,4/+60/1",
            "worker@5",
        ),
        "disk=none | run",
    );
    let r = &c.run.run;
    assert_eq!(worker_spawns(&r.items).len(), 4);
    let ns = notes(&r.items);
    for (k, ms) in [(1, 100), (2, 200), (3, 400), (4, 800)] {
        assert!(
            ns.iter()
                .any(|n| n.ends_with(&format!("restarting it in {ms} ms (restart: always)"))),
            "delay {ms}: {ns:#?}"
        );
        assert!(ns.contains(&format!(
            "hydo-ob: restarting worker (restart: always, restart {k})"
        )));
    }
    assert!(verdict(r).starts_with("HYDO-ORACLE: OK"), "{}", verdict(r));
    // a process that ran 10 s or more (here 500 units, 12.5 s) starts the delay again at 100 ms
    let c = run(
        &plan_needing(Some("always"), "1/go+5/1,2/+500/1", "worker@3"),
        "disk=none | run",
    );
    let r = &c.run.run;
    assert_eq!(worker_spawns(&r.items).len(), 2);
    let delays: Vec<String> = notes(&r.items)
        .into_iter()
        .filter(|n| n.contains("restarting it in"))
        .collect();
    assert_eq!(
        delays,
        vec![
            "hydo-ob: worker exited (status 1); restarting it in 100 ms (restart: always)",
            "hydo-ob: worker exited (status 1); restarting it in 100 ms (restart: always)",
        ]
    );
}

#[test]
fn a_crash_during_the_workload_is_restarted_at_once_not_at_the_heal() {
    let c = run(&plan(Some("always"), "1/wl+20/1"), "disk=none | run");
    let r = &c.run.run;
    let sp = worker_spawns(&r.items);
    assert_eq!(sp.len(), 1, "{sp:?}");
    assert_eq!(sp[0].1, "restart:worker");
    assert!(sp[0].0 < go_stop(r), "restarted only at the heal");
    let ns = notes(&r.items);
    assert!(
        !ns.iter()
            .any(|n| n.contains("worker is down; restarting it")),
        "{ns:#?}"
    );
    // without a policy, the heal's restart pass brings it back (today's behaviour)
    let c = run(&plan(None, "1/wl+20/1"), "disk=none | run");
    let r = &c.run.run;
    let sp = worker_spawns(&r.items);
    assert_eq!(sp.len(), 1);
    assert_eq!(sp[0].1, "main");
    assert!(notes(&r.items).contains(&"hydo-ob: worker is down; restarting it".to_string()));
}

#[test]
fn hydos_own_kill_keeps_the_schedules_meaning() {
    // K=worker@1+2: killed before step 1, started again before step 3 by the main sequence; the
    // policy does not restart a process hydo killed
    let c = run(
        &plan(Some("always"), "9/+1/1"),
        "disk=none | run K=worker@1+2",
    );
    let r = &c.run.run;
    let sp = worker_spawns(&r.items);
    assert_eq!(sp.len(), 1, "{sp:?}");
    assert_eq!(sp[0].1, "main");
    let ns = notes(&r.items);
    assert!(ns.contains(&"hydo-ob: restarting worker before step 3".to_string()));
    assert!(
        !ns.iter().any(|n| n.contains("(restart: always")),
        "{ns:#?}"
    );
    // the same for X= (the timed kill): the heal's restart pass brings it back
    let c = run(
        &plan(Some("always"), "9/+1/1"),
        "disk=none | run X=worker@1~10",
    );
    let r = &c.run.run;
    let sp = worker_spawns(&r.items);
    assert_eq!(sp.len(), 1, "{sp:?}");
    assert_eq!(sp[0].1, "main");
    assert!(!notes(&r.items)
        .iter()
        .any(|n| n.contains("(restart: always")));
}

#[test]
fn no_restart_after_a_power_cut_and_the_recovery_boot_has_its_own_supervisor() {
    // the worker dies in the recovery boot, 30 units after its crash recovery started it
    let p = plan(Some("always"), "1/rec+30/1");
    let c = run(&p, "disk=cut | run C=2");
    let r = &c.run.run;
    assert!(matches!(r.fin.ending, Some(Ending::PowerCut { .. })));
    assert_eq!(worker_spawns(&r.items), vec![]);
    let rc = c.run.recovery.as_ref().expect("a recovery boot");
    let sp = worker_spawns(&rc.items);
    assert_eq!(sp.len(), 2, "{sp:?}");
    assert_eq!(sp[0].1, "main"); // crash recovery
    assert_eq!(sp[1].1, "restart:worker");
    // without a policy the recovery boot's worker stays down
    let c = run(&plan(None, "1/rec+30/1"), "disk=cut | run C=2");
    let rc = c.run.recovery.as_ref().unwrap();
    assert_eq!(worker_spawns(&rc.items).len(), 1);
}

#[test]
fn solo_mode_follows_the_policy_too() {
    let solo = |p: &Plan| -> Transcript {
        let mut ctl = ScheduleController::solo(p.clone()).unwrap();
        let h = hydo_control::Header {
            controller: String::new(),
            conversation: "solo".into(),
            schedule: None,
            cadence: hydo_control::runner::cadence_of(None),
            start: "boot".into(),
        };
        let c = support::converse(Fake::boot("restart solo", 16), &mut ctl, Start::Boot, h);
        let sp = worker_spawns(&c.items);
        // the boot's start, then the policy's restart during the workload
        assert_eq!(sp.len(), 2, "{sp:?}");
        assert_eq!(sp[1].1, "restart:worker");
        c.fin.transcript
    };
    let p = plan(Some("always"), "1/wl+20/1");
    assert_eq!(solo(&p), solo(&p));
}

#[test]
fn the_policy_is_the_manifests() {
    let p = plan(Some("on-failure:3"), "1/+1/1");
    assert_eq!(
        p.service("worker").unwrap().restart,
        hydo_manifest::Restart::OnFailure { max: 3 }
    );
    assert_eq!(p.service("db").unwrap().restart, hydo_manifest::Restart::No);
    // a bare YAML `no` reads as false, which means no
    assert_eq!(
        plan(Some("no"), "1/+1/1")
            .service("worker")
            .unwrap()
            .restart,
        hydo_manifest::Restart::No
    );
    let bad = "name: x\nservices:\n  - name: a\n    cmd: a\n    ready: a\n    restart: sometimes\nworkload:\n  steps: 1\n  run: c\ninvariants:\n  - id: i\n    cmd: c\n";
    let e = hydo_manifest::from_embedded(bad, None).unwrap_err();
    assert!(
        e.message
            .starts_with("services[0]: restart: no, always, on-failure"),
        "{e}"
    );
}
