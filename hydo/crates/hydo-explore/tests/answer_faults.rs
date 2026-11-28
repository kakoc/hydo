//! `--answer-faults` and `--link-cuts` (Milestone 393): answers in another order
//! (`H=<route>@<a>-<b>:order=<k>[,wait=<ms>]`) on the GET routes of the fakes and HTTP proxies,
//! requests sent twice (`H=...:twice`) on the others, and cuts of the browser's link while a
//! step runs (`N=<browser>@<at>~<off>:<dur>`), each drawn from its own stream.
//!
//! - Without the options, and for a manifest without fakes, proxies or a browser, nothing
//!   changes.
//! - With them only their tokens are added; every other field of a seed's schedule stays.
//! - Every schedule is canonical for hydo-schedule, round-trips through the explorer's parse and
//!   render, and passes the controller's names check; so does every shrink candidate.
//! - The shrinker removes a link cut, halves its length and moves it to its step's start.

use hydo_explore::gen::Ex;
use hydo_explore::space::Space;
use hydo_schedule::{GuestToken, Schedule};
use serde_json::{json, Value};

fn shop(browser: bool) -> Value {
    let mut m = json!({
        "name": "shop",
        "network": "bridge",
        "fakes": [{"name": "api", "port": 9001, "addr": "10.89.0.1", "routes": [
            {"name": "search", "method": "GET", "path": "/api/search", "body": "[]"},
            {"name": "products", "method": "GET", "path": "/api/products", "body": "[]"},
            {"name": "place_order", "method": "POST", "path": "/api/orders", "body": "{}"},
        ]}],
        "faults": ["protocol", "clock", "network"],
    });
    if browser {
        m["browser"] = json!({"explore": {"start": "http://localhost:3000/"}});
    } else {
        m["services"] = json!([{"name": "app", "cmd": "app", "ready": "true", "addr": "10.89.0.11", "port": 3000}]);
        m["workload"] = json!({"steps": 6, "run": "true"});
        m["invariants"] = json!([{"id": "x", "cmd": "true"}]);
    }
    m
}

fn ex(m: &Value, answers: bool, cuts: bool) -> Ex {
    let sp = Space::from_text(&serde_json::to_string(m).unwrap(), None).unwrap();
    let mut e = Ex::default();
    sp.apply(&mut e);
    e.answer_faults = answers;
    e.link_cuts = cuts;
    e
}

fn plan(m: &Value) -> hydo_control::Plan {
    let text = serde_json::to_string(m).unwrap();
    let mm = hydo_manifest::from_embedded(&text, None).unwrap();
    hydo_control::Plan::from_manifest(&mm).unwrap()
}

#[test]
fn answer_faults_and_link_cuts_are_drawn_from_their_own_streams() {
    let m = shop(true);
    let (on, off) = (ex(&m, true, true), ex(&m, false, false));
    let p = plan(&m);
    let (mut orders, mut twices, mut cuts) = (0, 0, 0);
    let total = 3 * 200;
    for st in ["random", "swarm", "process"] {
        for seed in 1..=200u64 {
            let base = off.compose_seed(st, seed, false);
            let s = on.compose_seed(st, seed, false);
            let mut rest = s.clone();
            rest.h
                .retain(|h| !h.rules.iter().any(|(k, _)| k == "order" || k == "twice"));
            rest.lc = None;
            assert_eq!(rest, base, "{st} {seed}");
            for h in &s.h {
                if h.rules.iter().any(|(k, _)| k == "order") {
                    orders += 1;
                    assert!(
                        h.target == "api.search" || h.target == "api.products",
                        "{h:?}"
                    );
                }
                if h.rules.iter().any(|(k, _)| k == "twice") {
                    twices += 1;
                    assert_eq!(h.target, "api.place_order");
                }
                assert!(h.a < h.b, "{h:?}");
            }
            if let Some((svc, at, _, dur)) = &s.lc {
                cuts += 1;
                assert_eq!(svc, "browser");
                assert!((0..4).contains(at) && *dur >= 500);
            }
            let text = on.render(&s);
            let parsed = Schedule::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(parsed.to_string(), text);
            assert_eq!(on.render(&on.parse(&text).unwrap()), text);
            hydo_control::names::check(&p, &parsed).unwrap_or_else(|e| panic!("{text}: {e}"));
            let n = parsed
                .guest
                .iter()
                .filter(|t| matches!(t, GuestToken::LinkCut { .. }))
                .count();
            assert_eq!(n, s.lc.iter().count(), "{text}");
            for c in on.candidates(&s, None) {
                let t = on.render(&c);
                let pc = Schedule::parse(&t).unwrap_or_else(|e| panic!("candidate {t}: {e}"));
                hydo_control::names::check(&p, &pc).unwrap_or_else(|e| panic!("{t}: {e}"));
            }
        }
    }
    assert!(
        orders > total * 3 / 5 && orders < total * 9 / 10,
        "{orders} of {total}"
    );
    assert!(
        twices > total / 4 && twices < total * 11 / 20,
        "{twices} of {total}"
    );
    assert!(
        cuts > total * 2 / 5 && cuts < total * 4 / 5,
        "{cuts} of {total}"
    );
}

#[test]
fn without_the_options_or_a_browser_nothing_changes() {
    for (m, a, c) in [(shop(true), false, false), (shop(false), false, true)] {
        let (x, y) = (ex(&m, a, c), ex(&m, false, false));
        for st in ["random", "swarm", "process"] {
            for seed in 1..=150u64 {
                let s = x.compose_seed(st, seed, false);
                assert!(s.lc.is_none());
                assert_eq!(s, y.compose_seed(st, seed, false));
            }
        }
    }
    // without a browser --link-cuts draws nothing and is refused
    let e = ex(&shop(false), false, true);
    let (refused, _) = hydo_explore::refuse::nothing_drawn(&e, "random");
    assert!(
        refused.iter().any(|r| r.contains("--link-cuts")),
        "{refused:?}"
    );
    let e = ex(&shop(true), true, true);
    let (refused, _) = hydo_explore::refuse::nothing_drawn(&e, "random");
    assert!(refused.is_empty(), "{refused:?}");
}

#[test]
fn a_link_cut_shrinks_by_going_by_halves_and_to_its_step() {
    let e = ex(&shop(true), true, true);
    let s = e
        .parse("disk=none dseed=0 | run N=browser@2~500:5000 I=3")
        .unwrap();
    let texts: Vec<String> = e.candidates(&s, None).iter().map(|c| e.render(c)).collect();
    for want in [
        "disk=none dseed=0 | run I=3",
        "disk=none dseed=0 | run N=browser@2~500:2500 I=3",
        "disk=none dseed=0 | run N=browser@2~0:5000 I=3",
    ] {
        assert!(texts.iter().any(|t| t == want), "{want} not in {texts:#?}");
    }
    assert_eq!(e.finding_class(&s), "network");
}
