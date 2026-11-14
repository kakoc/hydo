//! Pauses of the workload (Milestone 216) on the scripted agent and engine: the timed pause
//! `P=workload@<at>~<off>:<dur>` stops the workload's own process tree (a test program's) for
//! some guest time from a moment inside a step, as a container's CPU limit does. The pause by
//! steps of the workload, and every other fault on `workload`, are refused before the run.

use crate::support;

use hydo_agent_proto::msg::{Command, Sig};
use hydo_control::Plan;
use hydo_schedule::Schedule;
use support::Item;

const TEST: &str = "name: pause-test
workload:
  kind: test
  steps: 1
  run: /bin/pkg.test -test.run TestX
invariants:
  - id: test.passes
    cmd: grep -q PASS /data/t/out
";

const SERVICES: &str = "name: pause-svc
services:
  - name: n1
    cmd: node 1
    ready: ping 1
workload:
  steps: 4
  run: client
invariants:
  - id: all.done
    final: true
    cmd: final-check
faults: [kill, pause]
";

fn plan_of(text: &str) -> Plan {
    let m = hydo_manifest::from_embedded(text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    Plan::from_manifest(&m).unwrap()
}

fn run(p: &Plan, sched: &str) -> Vec<Item> {
    let b = support::boot(p, "pause-test").unwrap();
    let s = Schedule::parse(sched).unwrap();
    hydo_control::names::check(p, &s).unwrap_or_else(|e| panic!("{sched}: refused: {e}"));
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
    a.run.items
}

fn signals(items: &[Item], sig: Sig) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Signal(s),
                ..
            } if s.sig == sig => Some(s.h.clone()),
            _ => None,
        })
        .collect()
}

fn runs(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Run(r),
                ..
            } => Some(r.argv.join(" ")),
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

#[test]
fn a_timed_pause_stops_the_workload_tree() {
    let p = plan_of(TEST);
    let items = run(&p, "disk=none | run P=workload@0~120:45");
    // the stop, its continue by the timer, and the main sequence's continue at the end
    assert_eq!(signals(&items, Sig::Stop), ["workload"]);
    assert_eq!(signals(&items, Sig::Cont), ["workload", "workload"]);
    let r = runs(&items);
    assert!(r.iter().any(|c| c.ends_with("sleep 0.120000")), "{r:#?}");
    assert!(r.iter().any(|c| c.ends_with("sleep 0.045")), "{r:#?}");
    // no request-in-flight check: the paused tree is the client itself
    assert!(!r.iter().any(|c| c.contains("pgrep")), "{r:#?}");
    let ns = notes(&items);
    assert!(
        ns.iter()
            .any(|n| n
                .contains("pause timer for workload armed before step 0 (after 120ms, for 45ms)")),
        "{ns:#?}"
    );
    assert!(ns
        .iter()
        .any(|n| n.contains("PAUSED workload (timer, mid_request=false)")));
}

/// A pause of threads of the workload (Milestone 226): no signal at all; the timer's guest
/// commands freeze the threads in a threaded cgroup and thaw them.
#[test]
fn a_timed_pause_of_threads_freezes_them() {
    let p = plan_of(TEST);
    let items = run(&p, "disk=none | run P=workload/worker#2@0~120:45");
    assert!(signals(&items, Sig::Stop).is_empty());
    assert!(signals(&items, Sig::Cont).is_empty());
    let r = runs(&items);
    assert!(r.iter().any(|c| c.ends_with("sleep 0.120000")), "{r:#?}");
    assert!(r.iter().any(|c| c.ends_with("sleep 0.045")), "{r:#?}");
    assert!(
        r.iter()
            .any(|c| c.contains("echo 1 >$D/cgroup.freeze") && c.ends_with(" worker 2")),
        "{r:#?}"
    );
    assert!(
        r.iter()
            .any(|c| c.contains("echo 0 >$1/cgroup.freeze")
                && c.contains(" /sys/fs/cgroup/hydo-pause ")),
        "{r:#?}"
    );
    let ns = notes(&items);
    assert!(
        ns.iter().any(|n| n.contains(
            "pause timer for workload/worker#2 armed before step 0 (after 120ms, for 45ms)"
        )),
        "{ns:#?}"
    );
    assert!(
        ns.iter()
            .any(|n| n.contains("PAUSED workload/worker#2 (timer, threads ")
                && n.ends_with(" worker)")),
        "{ns:#?}"
    );
    assert!(ns
        .iter()
        .any(|n| n.contains("RESUMED workload/worker#2 (timer)")));
    // no thread of that name: nothing is frozen, and the names the workload has are logged
    let items = run(&p, "disk=none | run P=workload/nonesuch@0~10:20");
    let ns = notes(&items);
    assert!(
        ns.iter().any(|n| n.contains(
            "PAUSED nothing: no thread of the workload matches workload/nonesuch (timer; its \
             threads: pkg.test, worker (3))"
        )),
        "{ns:#?}"
    );
    assert!(!runs(&items)
        .iter()
        .any(|c| c.contains("echo 0 >$1/cgroup.freeze")));
}

#[test]
fn the_workload_and_services_together() {
    let p = plan_of(SERVICES);
    let items = run(&p, "disk=none | run P=workload,n1@2~30:500");
    assert_eq!(signals(&items, Sig::Stop), ["workload", "n1"]);
    // a service's timed pause keeps its request-in-flight check
    let items = run(&p, "disk=none | run P=n1@2~30:500");
    assert!(runs(&items).iter().any(|c| c.contains("pgrep")));
}

#[test]
fn what_is_refused() {
    let p = plan_of(SERVICES);
    for (s, why) in [
        (
            "disk=none | run P=workload@1+2",
            "the timed form P=workload@<at>~<off ms>:<dur ms>",
        ),
        (
            "disk=none | run P=workload,n1@1+2",
            "pauses the workload between steps",
        ),
        ("disk=none | run K=workload@1+1", "the service workload"),
        ("disk=none | run X=workload@1~10", "the service workload"),
        (
            "disk=none | run P=workload,ghost@1~10:10",
            "the service ghost",
        ),
    ] {
        let e = hydo_control::names::check(&p, &Schedule::parse(s).unwrap()).expect_err(s);
        assert!(e.contains(why), "{s}: {e}");
    }
}
