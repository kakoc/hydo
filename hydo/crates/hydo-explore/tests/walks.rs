//! Walks through a web app's pages (Milestone 258, docs/design/ui-exploration.md): a manifest
//! with `browser.explore` gets `I=<seed>` in every schedule.
//!
//! - A manifest without `browser.explore` draws no walk, so its schedules stay as they were.
//! - With it only `I=` is added; every other field of a seed's schedule stays as it was.
//! - Every schedule round-trips through hydo-schedule and the explorer's parse and render and
//!   passes the controller's names check; so does every shrink candidate.
//! - The shrinker cuts a walk by fewer actions per step and by leaving out the first action of a
//!   step, and the walk is the last thing it cuts.

use hydo_explore::gen::Ex;
use hydo_explore::space::Space;
use hydo_schedule::{GuestToken, Schedule};
use serde_json::{json, Value};

fn manifest(walk: bool) -> Value {
    let mut m = json!({
        "name": "w",
        "network": "bridge",
        "services": [{"name": "app", "cmd": "app", "ready": "true", "port": 3000, "addr": "10.89.0.11"}],
        "faults": ["kill", "pause", "network", "clock"],
    });
    if walk {
        m["browser"] =
            json!({"explore": {"start": "http://10.89.0.11:3000/", "steps": 6, "actions": 8}});
    } else {
        m["workload"] = json!({"steps": 6, "run": "true"});
        m["invariants"] = json!([{"id": "x", "cmd": "true"}]);
    }
    m
}

fn ex(m: &Value) -> Ex {
    let sp = Space::from_text(&serde_json::to_string(m).unwrap(), None).unwrap();
    let mut e = Ex::default();
    sp.apply(&mut e);
    e
}

fn plan(m: &Value) -> hydo_control::Plan {
    let text = serde_json::to_string(m).unwrap();
    let mm = hydo_manifest::from_embedded(&text, None).unwrap();
    hydo_control::Plan::from_manifest(&mm).unwrap()
}

#[test]
fn every_schedule_of_a_walking_manifest_has_its_own_walk() {
    let (m, plain) = (manifest(true), manifest(false));
    let (on, off) = (ex(&m), ex(&plain));
    assert!(on.walks());
    assert!(!off.walks());
    let p = plan(&m);
    let mut seeds = std::collections::BTreeSet::new();
    for st in ["random", "swarm", "swarm-pos", "process"] {
        for seed in 1..=200u64 {
            let s = on.compose_seed(st, seed, false);
            let mut rest = s.clone();
            rest.i = None;
            // the walk is drawn from its own stream: the faults are those of the same seed
            // without it (the plain manifest has the same services and fault kinds)
            assert_eq!(
                on.render(&rest),
                off.render(&off.compose_seed(st, seed, false)),
                "{st} {seed}"
            );
            assert!(off.compose_seed(st, seed, false).i.is_none());
            let text = on.render(&s);
            let parsed = Schedule::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(parsed.to_string(), text);
            assert_eq!(on.render(&on.parse(&text).unwrap()), text);
            hydo_control::names::check(&p, &parsed).unwrap_or_else(|e| panic!("{text}: {e}"));
            let walk = parsed
                .guest
                .iter()
                .find_map(|t| match t {
                    GuestToken::Walk(w) => Some(w.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no walk in {text}"));
            seeds.insert(walk.seed);
            for c in on.candidates(&s, None) {
                let t = on.render(&c);
                let pc = Schedule::parse(&t).unwrap_or_else(|e| panic!("candidate {t}: {e}"));
                hydo_control::names::check(&p, &pc).unwrap_or_else(|e| panic!("{t}: {e}"));
                assert!(c.i.is_some(), "a candidate keeps its walk: {t}");
            }
        }
    }
    assert!(seeds.len() > 190, "{} seeds", seeds.len());
}

#[test]
fn a_walk_shrinks_by_fewer_actions_and_by_leaving_out_actions() {
    let e = ex(&manifest(true));
    let s = e.parse("disk=none dseed=1 | run I=9f3a").unwrap();
    let walks: Vec<String> = e
        .candidates(&s, None)
        .into_iter()
        .filter_map(|c| c.i)
        .collect();
    // the manifest's 8 actions: half and one less, then the first action of each of the 6 steps
    assert_eq!(
        walks,
        [
            "9f3a,n4",
            "9f3a,n7",
            "9f3a,x0.0",
            "9f3a,x1.0",
            "9f3a,x2.0",
            "9f3a,x3.0",
            "9f3a,x4.0",
            "9f3a,x5.0"
        ]
    );
    // a walk cut to one action per step is not cut further
    let s = e.parse("disk=none dseed=1 | run I=9f3a,n1").unwrap();
    assert!(e.candidates(&s, None).is_empty());
    // fewer actions drop the left-out actions they no longer have
    let s = e
        .parse("disk=none dseed=1 | run I=9f3a,n4,x0.3,x1.1")
        .unwrap();
    let w: Vec<String> = e
        .candidates(&s, None)
        .into_iter()
        .filter_map(|c| c.i)
        .collect();
    assert_eq!(w[0], "9f3a,n2,x1.1");
    assert_eq!(w[1], "9f3a,n3,x1.1");
    // a fault goes before the walk is cut
    let s = e.parse("disk=none dseed=1 | run K=app@2+1 I=9f3a").unwrap();
    let first = &e.candidates(&s, None)[0];
    assert!(first.k.is_none() && first.i.as_deref() == Some("9f3a"));
}

#[test]
fn a_walk_needs_a_walking_image() {
    let p = plan(&manifest(false));
    let s = Schedule::parse("disk=none | run I=1").unwrap();
    let e = hydo_control::names::check(&p, &s).unwrap_err();
    assert!(e.contains("not one"), "{e}");
}
