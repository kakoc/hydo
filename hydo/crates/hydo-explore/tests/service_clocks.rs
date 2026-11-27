//! `--service-clocks` (Milestone 375, docs/design/service-clocks.md): steps and drifts of one
//! service's own clock, `T=<svc>@<at>:<change>`, for the services with `clock: own`, drawn on
//! top of every strategy's schedules from their own stream.
//!
//! - Without the option, and for a manifest without `clock: own` or without `clock` in
//!   `faults:`, nothing changes.
//! - With it only the clock faults change: a service's token is added, or it takes the place
//!   of the guest's `T=` before the same step; every other field of a seed's schedule stays.
//! - Every schedule is canonical for hydo-schedule, round-trips through the explorer's parse and
//!   render, and passes the controller's names check; so does every shrink candidate.
//! - The shrinker removes a token and halves its step or drift towards 250 ms and 200 ppm.

use hydo_explore::gen::Ex;
use hydo_explore::sched::S;
use hydo_explore::space::Space;
use hydo_schedule::{GuestToken, Schedule, ServiceClockChange};
use serde_json::{json, Value};

fn cluster(own: bool, clock_fault: bool) -> Value {
    let node = |n: u32| {
        let mut s = json!({
            "name": format!("n{n}"), "cmd": format!("node {n}"), "ready": "true",
            "port": 2379 + n, "addr": format!("127.0.0.1{n}"),
        });
        if own && n != 3 {
            s["clock"] = json!("own");
        }
        s
    };
    let mut m = json!({
        "name": "c",
        "services": [node(1), node(2), node(3)],
        "workload": {"steps": 20, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
    });
    if !clock_fault {
        m["faults"] = json!(["kill", "pause", "network"]);
    }
    m
}

fn ex(m: &Value, on: bool) -> Ex {
    let sp = Space::from_text(&serde_json::to_string(m).unwrap(), None).unwrap();
    let mut e = Ex::default();
    sp.apply(&mut e);
    e.service_clocks = on;
    e
}

fn plan(m: &Value) -> hydo_control::Plan {
    let text = serde_json::to_string(m).unwrap();
    let mm = hydo_manifest::from_embedded(&text, None).unwrap();
    hydo_control::Plan::from_manifest(&mm).unwrap()
}

#[test]
fn service_clocks_are_drawn_from_their_own_stream() {
    let m = cluster(true, true);
    let (on, off) = (ex(&m, true), ex(&m, false));
    let p = plan(&m);
    let (mut drawn, mut instead, mut pairs, mut drifts) = (0, 0, 0, 0);
    let total = 4 * 300;
    for st in ["random", "swarm", "swarm-pos", "process"] {
        for seed in 1..=300u64 {
            let base = off.compose_seed(st, seed, false);
            let s = on.compose_seed(st, seed, false);
            assert!(base.ts.is_empty());
            let mut rest: S = s.clone();
            rest.ts = Vec::new();
            if !s.ts.is_empty() && s.t.is_none() && base.t.is_some() {
                // the service's change took the place of the guest's step, before its step
                instead += 1;
                assert_eq!(s.ts[0].1, base.t.unwrap().0, "{st} {seed}");
                rest.t = base.t;
                rest.tp = base.tp.clone();
            }
            assert_eq!(rest, base, "{st} {seed}");
            if !s.ts.is_empty() {
                drawn += 1;
                pairs += (s.ts.len() == 2) as usize;
                drifts += matches!(s.ts[0].2, ServiceClockChange::Drift(_)) as usize;
                for (svc, at, _) in &s.ts {
                    assert!(
                        svc == "n1" || svc == "n2",
                        "{svc}: not a clock: own service"
                    );
                    assert!((0..20).contains(at));
                }
                if s.ts.len() == 2 {
                    assert_ne!(s.ts[0].0, s.ts[1].0);
                    assert_eq!(s.ts[0].1, s.ts[1].1);
                }
            }
            let text = on.render(&s);
            let parsed = Schedule::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(parsed.to_string(), text);
            assert_eq!(on.render(&on.parse(&text).unwrap()), text);
            hydo_control::names::check(&p, &parsed).unwrap_or_else(|e| panic!("{text}: {e}"));
            let n = parsed
                .guest
                .iter()
                .filter(|t| matches!(t, GuestToken::ServiceClock { .. }))
                .count();
            assert_eq!(n, s.ts.len(), "{text}");
            for c in on.candidates(&s, None) {
                let t = on.render(&c);
                let pc = Schedule::parse(&t).unwrap_or_else(|e| panic!("candidate {t}: {e}"));
                hydo_control::names::check(&p, &pc).unwrap_or_else(|e| panic!("{t}: {e}"));
            }
        }
    }
    assert!(
        drawn > total * 2 / 5 && drawn < total * 4 / 5,
        "{drawn} of {total}"
    );
    assert!(instead > 0, "no guest step was replaced");
    assert!(
        pairs > drawn / 8 && pairs < drawn * 2 / 5,
        "{pairs} of {drawn}"
    );
    assert!(
        drifts > drawn / 8 && drifts < drawn / 2,
        "{drifts} of {drawn}"
    );
}

#[test]
fn without_own_clocks_or_the_option_or_clock_faults_nothing_changes() {
    for (m, on) in [
        (cluster(false, true), true),
        (cluster(true, true), false),
        (cluster(true, false), true),
    ] {
        let (a, b) = (ex(&m, on), ex(&m, false));
        for st in ["random", "swarm", "process"] {
            for seed in 1..=200u64 {
                let s = a.compose_seed(st, seed, false);
                assert!(s.ts.is_empty());
                assert_eq!(s, b.compose_seed(st, seed, false));
            }
        }
    }
}

#[test]
fn a_service_clock_shrinks_by_going_and_by_halves() {
    let m = cluster(true, true);
    let e = ex(&m, true);
    let s = e
        .parse("disk=none dseed=0 | run K=n3@2+1 T=n2@3:300 T=n1@3:-50000ppm")
        .unwrap();
    assert_eq!(s.ts.len(), 2);
    let texts: Vec<String> = e.candidates(&s, None).iter().map(|c| e.render(c)).collect();
    for want in [
        "disk=none dseed=0 | run K=n3@2+1 T=n1@3:-50000ppm",
        "disk=none dseed=0 | run K=n3@2+1 T=n2@3:300",
        "disk=none dseed=0 | run K=n3@2+1 T=n2@3:150 T=n1@3:-50000ppm",
        "disk=none dseed=0 | run K=n3@2+1 T=n2@3:300 T=n1@3:-25000ppm",
    ] {
        assert!(texts.iter().any(|t| t == want), "{want} not in {texts:#?}");
    }
    // the halves stop at 250 ms and 200 ppm, and keep one spelling
    let s = e
        .parse("disk=none dseed=0 | run T=n2@3:1500ms T=n1@0:200ppm T=n2@4:-250ms")
        .unwrap();
    let texts: Vec<String> = e.candidates(&s, None).iter().map(|c| e.render(c)).collect();
    assert!(texts
        .iter()
        .any(|t| t == "disk=none dseed=0 | run T=n2@3:750ms T=n1@0:200ppm T=n2@4:-250ms"));
    assert!(!texts
        .iter()
        .any(|t| t.contains("100ppm") || t.contains("125ms")));
    // a schedule with a service's clock is in the class of its clock faults
    let s = e.parse("disk=none dseed=0 | run T=n2@3:5").unwrap();
    assert_eq!(e.finding_class(&s), "clock");
}

#[test]
fn the_option_is_refused_where_it_draws_nothing() {
    let e = ex(&cluster(true, false), true);
    let (refused, _) = hydo_explore::refuse::nothing_drawn(&e, "random");
    assert!(
        refused.iter().any(|r| r.contains("--service-clocks")),
        "{refused:?}"
    );
    let e = ex(&cluster(true, true), true);
    let (refused, _) = hydo_explore::refuse::nothing_drawn(&e, "random");
    assert!(refused.is_empty(), "{refused:?}");
}
