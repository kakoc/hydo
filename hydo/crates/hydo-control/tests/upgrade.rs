//! Upgrades, `U=<target>@<at>[/<gap>][+<back>]:<version>` (Milestone 238,
//! docs/design/upgrades.md), on the scripted agent and engine.
//!
//! Three nodes run version `v1` (their own commands) and have the versions `v2` (another
//! command) and `bad` (whose readiness check never passes). An upgrade stops a node with SIGTERM,
//! starts it again with the version's commands and `$HYDO_VERSION`, and its readiness check; a
//! group's rolling upgrade does the nodes one after another; `+<back>` rolls a node back; a
//! version that does not start ends the run with `VIOLATION [upgrade.started]`; a power cut
//! hands the switched versions to the recovery boot; and a plan without `U=` sends what it sent.

use crate::support;

use hydo_agent_proto::msg::{Command, Sig};
use hydo_control::{Ending, Plan};
use hydo_schedule::Schedule;
use support::Item;

const NODES: &str = "\
name: cluster
groups:
  - name: nodes
    services: [n1, n2, n3]
services:
";

fn node(n: u32, cmd: &str) -> String {
    format!(
        "  - name: n{n}
    cmd: {cmd}
    ready: node-check
    version: v1
    versions:
      - name: v2
        cmd: node-v2-{n} --data /data/n{n}
      - name: bad
        cmd: node-bad-{n}
        ready: node-check HYDO_FAKE_PEERS=never
"
    )
}

const TAIL: &str = "\
workload:
  steps: 8
  run: client
invariants:
  - id: all.done
    final: true
    cmd: final-check
";

fn plan_with(cmd1: &str) -> Plan {
    let mut text = NODES.to_string();
    text.push_str(&node(1, cmd1));
    text.push_str(&node(2, "node-v1-2 --data /data/n2"));
    text.push_str(&node(3, "node-v1-3 --data /data/n3"));
    text.push_str(TAIL);
    let m = hydo_manifest::from_embedded(&text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    Plan::from_manifest(&m).unwrap()
}

fn plan() -> Plan {
    plan_with("node-v1-1 --data /data/n1")
}

fn ran(p: &Plan, s: &str) -> support::Ran {
    let b = support::boot(p, "cluster").unwrap();
    let s = Schedule::parse(s).unwrap();
    hydo_control::names::check(p, &s).unwrap();
    support::run_schedule(&b, &s).unwrap()
}

/// The services' starts and stops, in order: `spawn <h> <the version's command word>` and
/// `<SIG> <h>`, as (by, text).
fn events(items: &[Item]) -> Vec<(String, String)> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Spawn(s),
                by,
                ..
            } if !s.h.contains(':') && s.h != "workload" => {
                let text = s.argv.join(" ");
                let word = text
                    .split_whitespace()
                    .find(|w| w.starts_with("node-"))
                    .unwrap_or("?")
                    .to_string();
                let v = s
                    .env
                    .iter()
                    .flatten()
                    .find_map(|e| e.strip_prefix("HYDO_VERSION="))
                    .unwrap_or("-")
                    .to_string();
                Some((by.clone(), format!("spawn {} {word} {v}", s.h)))
            }
            Item::Send {
                cmd: Command::Signal(g),
                by,
                ..
            } if g.sig == Sig::Term || g.sig == Sig::Kill => Some((
                by.clone(),
                format!(
                    "{} {}",
                    if g.sig == Sig::Term { "TERM" } else { "KILL" },
                    g.h
                ),
            )),
            _ => None,
        })
        .collect()
}

fn verdict(r: &support::Ran) -> String {
    match &r.run.fin.ending {
        Some(Ending::Verdict(v)) => v.clone(),
        e => panic!("no verdict: {e:?}"),
    }
}

fn notes(r: &support::Ran) -> Vec<String> {
    r.run
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Note { line, .. } if line.starts_with("HYDO-UPGRADE ") => Some(line.clone()),
            _ => None,
        })
        .collect()
}

fn texts(e: &[(String, String)]) -> Vec<&str> {
    e.iter().map(|(_, t)| t.as_str()).collect()
}

#[test]
fn a_rolling_upgrade_switches_each_member_in_turn() {
    let p = plan();
    let r = ran(&p, "disk=none | run U=nodes.all@1/2:v2");
    let e = events(&r.run.items);
    assert_eq!(
        texts(&e),
        vec![
            "TERM n1",
            "spawn n1 node-v2-1 v2",
            "TERM n2",
            "spawn n2 node-v2-2 v2",
            "TERM n3",
            "spawn n3 node-v2-3 v2",
        ],
        "{e:?}"
    );
    assert!(
        !verdict(&r).contains("VIOLATION [upgrade"),
        "{}",
        verdict(&r)
    );
    let n = notes(&r);
    assert_eq!(n.len(), 3, "{n:?}");
    assert_eq!(
        n[1],
        "HYDO-UPGRADE {\"service\":\"n2\",\"step\":3,\"from\":\"v1\",\"to\":\"v2\",\"rollback\":false,\"started\":true}"
    );
    // the assertion of the fault is declared and reached
    let reports: Vec<&str> = r
        .run
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Report { line, .. } => Some(line.as_str()),
            _ => None,
        })
        .collect();
    assert!(reports
        .iter()
        .any(|l| l.contains("\"id\":\"fault.upgraded\"") && l.contains("HYDO-ASSERT-DECL")));
    assert!(reports
        .iter()
        .any(|l| l.contains("\"id\":\"fault.upgraded\"") && l.contains("\"ok\":true")));
    // the run's replay from its transcript is the same run
    let mut reference = r.run.fin.transcript.clone();
    reference.header.controller = "replay".into();
    assert_eq!(r.replay.to_jsonl(), reference.to_jsonl());
    // and two runs of the schedule are the same run
    let again = ran(&p, "disk=none | run U=nodes.all@1/2:v2");
    assert_eq!(again.run.fin.transcript, r.run.fin.transcript);
}

#[test]
fn every_command_of_a_service_with_versions_sees_its_version() {
    let p = plan();
    let b = support::boot(&p, "cluster").unwrap();
    for i in &b.boot.items {
        if let Item::Send {
            cmd: Command::Spawn(s),
            ..
        } = i
        {
            if !s.h.contains(':') {
                assert!(
                    s.env.iter().flatten().any(|e| e == "HYDO_VERSION=v1"),
                    "{s:?}"
                );
            }
        }
    }
}

#[test]
fn a_rollback_goes_back_to_the_version_before() {
    let p = plan();
    let r = ran(&p, "disk=none | run U=n1@1+2:v2");
    assert_eq!(
        texts(&events(&r.run.items)),
        vec![
            "TERM n1",
            "spawn n1 node-v2-1 v2",
            "TERM n1",
            "spawn n1 node-v1-1 v1"
        ]
    );
    let n = notes(&r);
    assert!(
        n[1].contains("\"from\":\"v2\",\"to\":\"v1\",\"rollback\":true"),
        "{n:?}"
    );
    // a downgrade written as its own token does the same
    let r2 = ran(&p, "disk=none | run U=n1@1:v2 U=n1@3:v1");
    assert_eq!(texts(&events(&r2.run.items)), texts(&events(&r.run.items)));
}

#[test]
fn a_version_that_does_not_start_is_a_violation() {
    let p = plan();
    let r = ran(&p, "disk=none | run U=nodes.all@2:bad");
    let v = verdict(&r);
    assert_eq!(
        v,
        "HYDO-ORACLE: VIOLATION [upgrade.started] n1 did not start on version bad before step 2 (it ran v1)"
    );
    // the rolling upgrade stops at the first member that does not come back
    assert_eq!(
        texts(&events(&r.run.items)),
        vec!["TERM n1", "spawn n1 node-bad-1 bad"]
    );
    assert!(notes(&r)[0].ends_with("\"started\":false}"));
}

#[test]
fn a_process_that_ignores_sigterm_is_killed_after_the_grace_period() {
    let p = plan_with("node-v1-1 HYDO_FAKE_IGNORE_TERM --data /data/n1");
    let r = ran(&p, "disk=none | run U=n1@1:v2");
    let e = events(&r.run.items);
    assert_eq!(
        e,
        vec![
            ("main".to_string(), "TERM n1".to_string()),
            ("upgrade-grace:n1".to_string(), "KILL n1".to_string()),
            ("main".to_string(), "spawn n1 node-v2-1 v2".to_string()),
        ]
    );
}

#[test]
fn a_member_down_at_its_upgrade_starts_on_the_new_version() {
    let p = plan();
    // n2 is killed before step 1 and started again before step 3; its upgrade at step 2 only
    // switches its version
    let r = ran(&p, "disk=none | run K=n2@1+2 U=n2@2:v2");
    assert_eq!(
        texts(&events(&r.run.items)),
        vec!["KILL n2", "spawn n2 node-v2-2 v2"]
    );
    assert!(notes(&r)[0].ends_with("\"started\":null}"));
}

#[test]
fn a_power_cut_carries_the_versions_to_the_recovery_boot() {
    let p = plan();
    let r = ran(&p, "disk=cut | run U=n1,n3@1:v2 C=4");
    let i = r.inputs.as_ref().expect("a power cut");
    assert_eq!(
        i.versions,
        vec![
            ("n1".to_string(), "v2".to_string()),
            ("n3".to_string(), "v2".to_string())
        ]
    );
    let rc = r.recovery.as_ref().expect("a recovery boot");
    assert_eq!(
        texts(&events(&rc.items)),
        vec![
            "spawn n1 node-v2-1 v2",
            "spawn n2 node-v1-2 v1",
            "spawn n3 node-v2-3 v2"
        ]
    );
    // the transcript hands over the same (hydo repro --transcript)
    let t = hydo_control::RecoveryInputs::from_transcript(&r.run.fin.transcript).unwrap();
    assert_eq!(t.versions, i.versions);
}

#[test]
fn what_the_names_check_refuses() {
    let p = plan();
    let check = |s: &str| hydo_control::names::check(&p, &Schedule::parse(s).unwrap());
    let e = check("disk=none | run U=n1@1:v3").unwrap_err();
    assert!(
        e.contains("which n1 does not have (its versions: v1, v2, bad)"),
        "{e}"
    );
    assert!(check("disk=none | run U=n4@1:v2").is_err());
    let plain = support::plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    let e = hydo_control::names::check(
        &plain,
        &Schedule::parse("disk=none | run U=redis@1:v2").unwrap(),
    )
    .unwrap_err();
    assert!(e.contains("has no versions:"), "{e}");
}

#[test]
fn a_plan_without_versions_gets_no_version_variable() {
    let plain = support::plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    let b = support::boot(&plain, "redis").unwrap();
    for i in &b.boot.items {
        if let Item::Send {
            cmd: Command::Spawn(s),
            ..
        } = i
        {
            assert!(
                !s.env
                    .iter()
                    .flatten()
                    .any(|e| e.starts_with("HYDO_VERSION=")),
                "{s:?}"
            );
        }
    }
}
