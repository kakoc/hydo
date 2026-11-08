//! Faults aimed at several services or at a role, and partitions (Milestone 195) on the scripted
//! agent and engine: lists and group selectors for `K=`, `P=` and `X=` (the selector resolved
//! before its step and logged, the primary by the group's probe), partitions of components and
//! of a group's shapes for `N=`, several `N=` tokens as one fault, and the refusals. Each run is
//! made twice (one transcript) and typed again from its transcript; the transcripts are recorded
//! in `golden/groups.txt` (re-record: `HYDO_BLESS=1 cargo test -p hydo-control --test it
//! groups::`).

use crate::support;

use hydo_agent_proto::msg::{Command, Sig};
use hydo_control::{Ending, Plan, ScheduleController};
use hydo_schedule::Schedule;
use support::{Conv, Item};

const LOOPBACK: &str = "name: groups-test
services:
  - name: n1
    cmd: node 1
    ready: ping 1
    addr: 127.0.0.11
    port: 4001
  - name: n2
    cmd: node 2
    ready: ping 2
    addr: 127.0.0.12
    port: 4002
  - name: n3
    cmd: node 3
    ready: ping 3
    addr: 127.0.0.13
    port: 4003
  - name: client_side
    cmd: side
    ready: \"true\"
groups:
  - name: nodes
    services: [n1, n2, n3]
    primary: HYDO_FAKE_PRIMARY=n2 leader
  - name: lonely
    services: [n1]
  - name: noprobe
    services: [n1, n2]
  - name: deaf
    services: [n1, n2]
    primary: HYDO_FAKE_PRIMARY=nobody leader
workload:
  steps: 8
  run: client
invariants:
  - id: all.done
    final: true
    cmd: final-check
faults: [kill, pause, network, oneway, cut]
";

fn plan_of(text: &str) -> Plan {
    let m = hydo_manifest::from_embedded(text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    Plan::from_manifest(&m).unwrap()
}

fn bridge() -> Plan {
    let text = LOOPBACK
        .replace("127.0.0.1", "10.89.0.1")
        .replace(
            "name: groups-test\n",
            "name: groups-test\nnetwork: bridge\n",
        )
        .replace(
            "    cmd: side\n    ready: \"true\"\n",
            "    cmd: side\n    ready: \"true\"\n    addr: 10.89.0.20\n",
        );
    plan_of(&text)
}

fn run(p: &Plan, sched: &str) -> support::Ran {
    let b = support::boot(p, "groups-test").unwrap();
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

fn has(ns: &[String], s: &str) -> bool {
    ns.iter().any(|n| n.contains(s))
}

/// The filters a fault put on: `<src>/32 > <dst>/32` per u32 filter.
fn filters(items: &[Item]) -> Vec<String> {
    runs(items)
        .iter()
        .filter(|c| c.contains(" filter add "))
        .map(|c| {
            let w: Vec<&str> = c.split(' ').collect();
            let at = |k: &str| {
                w.iter()
                    .position(|x| *x == k)
                    .and_then(|i| w.get(i + 1))
                    .copied()
                    .unwrap_or("")
            };
            format!("{} > {}", at("src"), at("dst"))
        })
        .collect()
}

#[test]
fn a_list_and_a_selector_kill_several_services() {
    let p = plan_of(LOOPBACK);
    let r = run(&p, "disk=none | run K=n1,n3@2+2");
    assert_eq!(signals(&r.run.items, Sig::Kill), ["n1", "n3"]);
    let ns = notes(&r.run.items);
    for s in [
        "KILLED n1 before step 2",
        "KILLED n3 before step 2",
        "restarting n1 before step 4",
        "restarting n3 before step 4",
    ] {
        assert!(has(&ns, s), "{s}: {ns:#?}");
    }
    verdict(&r.run);
    let r = run(&p, "disk=none | run K=nodes.majority.2@2+1");
    assert_eq!(signals(&r.run.items, Sig::Kill), ["n2", "n3"]);
    assert!(has(&notes(&r.run.items), "K: nodes.majority.2 = n2,n3"));
    let r = run(&p, "disk=none | run K=nodes.all@3+2");
    assert_eq!(signals(&r.run.items, Sig::Kill), ["n1", "n2", "n3"]);
}

#[test]
fn the_primary_is_asked_at_the_step() {
    let p = plan_of(LOOPBACK);
    let r = run(&p, "disk=none | run K=nodes.primary@3+1");
    assert_eq!(signals(&r.run.items, Sig::Kill), ["n2"]);
    assert!(has(&notes(&r.run.items), "K: nodes.primary = n2"));
    assert!(runs(&r.run.items)
        .iter()
        .any(|c| c.contains("HYDO_FAKE_PRIMARY=n2 leader")));
    // a probe that never names a member ends the run in an error after its tries
    let r = run(&p, "disk=none | run K=deaf.primary@3+1");
    let v = verdict(&r.run);
    assert!(
        v.contains("deaf.primary: the group's primary: command named no member"),
        "{v}"
    );
    assert!(signals(&r.run.items, Sig::Kill).is_empty());
}

#[test]
fn pauses_and_timed_kills_of_several_services() {
    let p = plan_of(LOOPBACK);
    let r = run(&p, "disk=none | run P=nodes.minority.2@2+2");
    assert_eq!(signals(&r.run.items, Sig::Stop), ["n3"]);
    let ns = notes(&r.run.items);
    assert!(has(&ns, "P: nodes.minority.2 = n3"), "{ns:#?}");
    let r = run(&p, "disk=none | run P=n1,n2@2+2");
    assert_eq!(signals(&r.run.items, Sig::Stop), ["n1", "n2"]);
    assert_eq!(signals(&r.run.items, Sig::Cont), ["n1", "n2"]);
    let r = run(&p, "disk=none | run P=nodes.all@1~100:400");
    assert_eq!(signals(&r.run.items, Sig::Stop), ["n1", "n2", "n3"]);
    assert!(has(
        &notes(&r.run.items),
        "pause timer for n1,n2,n3 armed before step 1"
    ));
    let r = run(&p, "disk=none | run X=nodes.one.1@2~50");
    assert_eq!(signals(&r.run.items, Sig::Kill), ["n2"]);
}

#[test]
fn partitions_cut_the_links_between_components() {
    let p = plan_of(LOOPBACK);
    let r = run(&p, "disk=none | run N=n1/n2,n3@1-4:loss,100%");
    assert_eq!(
        filters(&r.run.items),
        [
            "127.0.0.11/32 > 127.0.0.12/32",
            "127.0.0.11/32 > 127.0.0.13/32",
            "127.0.0.12/32 > 127.0.0.11/32",
            "127.0.0.13/32 > 127.0.0.11/32",
        ]
    );
    let ns = notes(&r.run.items);
    assert!(has(&ns, "PARTITION n1/n2,n3"), "{ns:#?}");
    assert!(has(&ns, "network healed before step 4"), "{ns:#?}");
    // the ring of three: each member hears itself and the next one
    let r = run(&p, "disk=none | run N=nodes.ring@1-4:loss,100%");
    assert_eq!(
        filters(&r.run.items),
        [
            "127.0.0.13/32 > 127.0.0.11/32",
            "127.0.0.11/32 > 127.0.0.12/32",
            "127.0.0.12/32 > 127.0.0.13/32",
        ]
    );
    assert!(has(
        &notes(&r.run.items),
        "N: nodes.ring = n3>n1 n1>n2 n2>n3"
    ));
    // the primary alone
    let r = run(&p, "disk=none | run N=nodes.primary@2-5:delay,300ms");
    assert_eq!(filters(&r.run.items).len(), 4);
    assert!(filters(&r.run.items)
        .iter()
        .all(|f| f.contains("127.0.0.12/32")));
}

#[test]
fn a_partition_with_network_bridge_uses_the_veths() {
    let p = bridge();
    let r = run(&p, "disk=none | run N=nodes.bridge.1@1-3:loss,100%");
    // n2 is the bridge: n1 and n3 cannot reach each other
    assert_eq!(
        filters(&r.run.items),
        [
            "10.89.0.11/32 > 10.89.0.13/32",
            "10.89.0.13/32 > 10.89.0.11/32"
        ]
    );
    let ns = notes(&r.run.items);
    assert!(has(&ns, "PARTITION nodes.bridge.1"), "{ns:#?}");
    assert!(has(&ns, "network fault lifted"), "{ns:#?}");
    let r = run(&p, "disk=none | run N=nodes.halves@2-5:loss,100%");
    assert_eq!(filters(&r.run.items).len(), 4, "{:#?}", notes(&r.run.items));
}

#[test]
fn several_link_tokens_are_one_fault() {
    let p = plan_of(LOOPBACK);
    let r = run(
        &p,
        "disk=none | run N=n1-n2@1-4:loss,100% N=n1-n3@1-4:loss,100%",
    );
    assert_eq!(
        filters(&r.run.items),
        [
            "127.0.0.11/32 > 127.0.0.12/32",
            "127.0.0.12/32 > 127.0.0.11/32",
            "127.0.0.11/32 > 127.0.0.13/32",
            "127.0.0.13/32 > 127.0.0.11/32",
        ]
    );
    assert!(has(
        &notes(&r.run.items),
        "PARTITION n1-n2 n1-n3 (n1>n2 n2>n1 n1>n3 n3>n1)"
    ));
}

fn refused(p: &Plan, sched: &str, why: &str) {
    let b = support::boot(p, "groups-test").unwrap();
    let s = Schedule::parse(sched).unwrap();
    let e = match ScheduleController::run_unchecked(p.clone(), &s, &b.s1.2) {
        Ok(_) => panic!("{sched}: accepted"),
        Err(e) => e.to_string(),
    };
    assert!(e.contains(why), "{sched}: {e}");
}

fn refused_by_names(p: &Plan, sched: &str, why: &str) {
    let s = Schedule::parse(sched).unwrap();
    let e = hydo_control::names::check(p, &s).expect_err(sched);
    assert!(e.contains(why), "{sched}: {e}");
}

#[test]
fn what_is_refused() {
    let p = plan_of(LOOPBACK);
    refused(&p, "disk=cut | run C=3 C=5", "a run has one power cut");
    refused(
        &p,
        "disk=none | run K=n1@1+1 K=n2@1+1",
        "a list (K=a,b@...)",
    );
    refused(
        &p,
        "disk=none | run N=n1-n2@1-4:loss,100% N=n1-n3@2-4:loss,100%",
        "different windows",
    );
    refused(
        &p,
        "disk=none | run N=n1@1-4:loss,100% N=n2-n3@1-4:loss,100%",
        "a fault on the service's port",
    );
    refused_by_names(
        &p,
        "disk=none | run K=cluster.all@1+1",
        "names the group cluster",
    );
    refused_by_names(
        &p,
        "disk=none | run K=noprobe.primary@1+1",
        "no primary: command",
    );
    refused_by_names(
        &p,
        "disk=none | run K=lonely.minority@1+1",
        "too few for minority",
    );
    refused_by_names(&p, "disk=none | run K=n1,ghost@1+1", "the service ghost");
    refused_by_names(
        &p,
        "disk=none | run N=lonely.halves@1-2:loss,100%",
        "at least 2",
    );
    refused_by_names(
        &p,
        "disk=none | run N=noprobe.ring@1-2:loss,100%",
        "at least 3",
    );
    refused_by_names(
        &p,
        "disk=none | run N=n1/client_side@1-2:loss,100%",
        "client_side, which has no addr:",
    );
    // the same target twice: the last one, as before Milestone 195 (the identity corpus)
    let r = run(
        &p,
        "disk=none | run N=n1-n2@1-3:delay,9ms N=n1-n2@2-5:delay,20ms",
    );
    assert!(has(&notes(&r.run.items), "netem delay 20ms"));
    assert!(!has(&notes(&r.run.items), "netem delay 9ms"));
}

/// The transcripts of the schedules above, recorded.
#[test]
fn the_transcripts_are_recorded() {
    let lo = plan_of(LOOPBACK);
    let br = bridge();
    let mut lines = Vec::new();
    for (name, p, s) in [
        ("loopback", &lo, "disk=none | run K=n1,n3@2+2"),
        ("loopback", &lo, "disk=none | run K=nodes.majority.2@2+1"),
        ("loopback", &lo, "disk=none | run K=nodes.primary@3+1"),
        ("loopback", &lo, "disk=none | run K=deaf.primary@3+1"),
        ("loopback", &lo, "disk=none | run P=nodes.all@1~100:400"),
        ("loopback", &lo, "disk=none | run X=nodes.one.1@2~50"),
        ("loopback", &lo, "disk=cut | run K=nodes.all@2+3 C=4"),
        (
            "loopback",
            &lo,
            "disk=none | run N=nodes.ring@1-4:loss,100%",
        ),
        (
            "loopback",
            &lo,
            "disk=none | run N=nodes.primary@2-5:delay,300ms",
        ),
        (
            "loopback",
            &lo,
            "disk=none | run N=n1-n2@1-4:loss,100% N=n1-n3@1-4:loss,100%",
        ),
        (
            "bridge",
            &br,
            "disk=none | run N=nodes.bridge.1@1-3:loss,100%",
        ),
        (
            "bridge",
            &br,
            "disk=none | run N=n1,n2/n3@0-8:loss,100% K=n3@2+2",
        ),
    ] {
        let r = run(p, s);
        lines.push(format!(
            "{name}\t{s}\t{}",
            support::sha256_hex(r.run.fin.transcript.to_jsonl().as_bytes())
        ));
        if let Some(rc) = &r.recovery {
            lines.push(format!(
                "{name}\t{s}\trecovery\t{}",
                support::sha256_hex(rc.fin.transcript.to_jsonl().as_bytes())
            ));
        }
    }
    support::check_golden(
        "groups.txt",
        &[
            "Milestone 195: faults aimed at several services or a role, and partitions, on the",
            "scripted agent and engine (hydo/crates/hydo-control/tests/groups.rs). Re-record:",
            "HYDO_BLESS=1 cargo test -p hydo-control --test it groups::",
        ],
        &[],
        &lines,
    );
}
