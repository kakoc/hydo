//! `services[].start_after:` (Milestone 196) on the scripted agent and engine: a cluster whose
//! readiness check needs its peers.
//!
//! Three nodes each have a readiness check that passes only while all three run
//! (`HYDO_FAKE_PEERS=` in the scripted guest). By default each service starts only when the one
//! before it is ready, so the first node's check waits for peers that are never started and the
//! boot ends in `ERROR service start (n1)`. With `start_after: started` on the second and third
//! nodes, the three are started first and then checked in their start order, and the boot
//! reaches S1. A service after them without the key waits until all three are ready. A plan
//! with `start_after: ready` written out sends the same commands as one without the key.

use crate::support;

use hydo_agent_proto::msg::Command;
use hydo_control::{Ending, Plan};
use hydo_schedule::Schedule;
use support::Item;

fn plan(after: [&str; 3], tail: bool) -> Plan {
    let key = |a: &str| {
        if a.is_empty() {
            String::new()
        } else {
            format!("    start_after: {a}\n")
        }
    };
    let mut text = String::from("name: cluster\nservices:\n");
    for (i, a) in after.iter().enumerate() {
        text.push_str(&format!(
            "  - name: n{n}\n    cmd: node-{n}\n    ready: node-check HYDO_FAKE_PEERS=n1,n2,n3\n{k}",
            n = i + 1,
            k = key(a)
        ));
    }
    if tail {
        text.push_str("  - name: app\n    cmd: app-server\n    ready: app-ping\n");
    }
    text.push_str(
        "workload:\n  steps: 4\n  run: client\ninvariants:\n  - id: all.done\n    final: true\n    cmd: final-check\n",
    );
    let m = hydo_manifest::from_embedded(&text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    Plan::from_manifest(&m).unwrap()
}

/// What the boot sent, in order: `spawn <h>` and `probe <h>` (the service whose check it is).
fn starts(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Spawn(s),
                ..
            } if !s.h.contains(':') => Some(format!("spawn {}", s.h)),
            Item::Send {
                cmd: Command::Probe(p),
                ..
            } => p.alive.as_ref().map(|h| format!("probe {h}")),
            _ => None,
        })
        .collect()
}

fn boot_ending(p: &Plan) -> Option<Ending> {
    support::boot_conv(p, "cluster").unwrap().0.fin.ending
}

/// The boot's whole transcript (every command, at its stop).
fn boot_items(p: &Plan) -> String {
    support::boot_conv(p, "cluster")
        .unwrap()
        .0
        .fin
        .transcript
        .to_jsonl()
}

#[test]
fn by_default_a_check_that_needs_its_peers_never_passes() {
    for after in [["", "", ""], ["", "ready", "ready"]] {
        match boot_ending(&plan(after, false)) {
            Some(Ending::Verdict(v)) => {
                assert_eq!(v, "HYDO-ORACLE: ERROR service start (n1)", "{after:?}")
            }
            e => panic!("{after:?}: {e:?}"),
        }
    }
}

#[test]
fn started_starts_the_peers_first_and_checks_them_in_order() {
    let p = plan(["", "started", "started"], true);
    let b = support::boot(&p, "cluster").unwrap();
    assert_eq!(
        starts(&b.boot.items),
        [
            "spawn n1",
            "spawn n2",
            "spawn n3",
            "probe n1",
            "probe n2",
            "probe n3",
            "spawn app",
            "probe app"
        ]
    );
    // a run from S1 goes on as usual, twice the same
    let s = Schedule::parse("disk=none | run K=n2@1+1").unwrap();
    let a = support::run_schedule(&b, &s).unwrap();
    let again = support::run_schedule(&b, &s).unwrap();
    assert_eq!(a.run.fin.transcript, again.run.fin.transcript);
    assert!(
        matches!(&a.run.fin.ending, Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE: OK")),
        "{:?}",
        a.run.fin.ending
    );
    // only the services after n1 start early: n1 alone with the key changes nothing
    let one = plan(["started", "", ""], false);
    let none = plan(["", "", ""], false);
    assert_eq!(
        boot_items(&one),
        boot_items(&none),
        "start_after on the first service has nothing to wait for"
    );
}

#[test]
fn ready_written_out_is_the_default_command_for_command() {
    let a = boot_items(&plan(["ready", "ready", "ready"], true));
    let b = boot_items(&plan(["", "", ""], true));
    assert_eq!(a, b);
}

#[test]
fn the_recovery_boot_starts_them_the_same_way() {
    let p = plan(["", "started", "started"], false);
    let b = support::boot(&p, "cluster").unwrap();
    let s = Schedule::parse("disk=none | run C=2").unwrap();
    let r = support::run_schedule(&b, &s).unwrap();
    let rec = r.recovery.expect("a recovery boot after C=");
    let st = starts(&rec.items);
    assert_eq!(
        &st[..6],
        ["spawn n1", "spawn n2", "spawn n3", "probe n1", "probe n2", "probe n3"],
        "{st:?}"
    );
}
