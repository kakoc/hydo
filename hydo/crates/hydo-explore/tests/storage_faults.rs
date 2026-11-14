//! `--storage-faults` (docs/design/storage-faults.md): corruption (`G=`) and one service's power
//! loss (`L=`) drawn on top of every strategy's schedules from their own stream.
//!
//! - Every other field of a seed's schedule stays as it was: a schedule with storage faults is
//!   the schedule without them plus `G=`/`L=`.
//! - `G=` only on services with `storage:`, `L=` only on those with their own file system, and
//!   neither when the manifest's `faults:` list leaves them out; without the option nothing is
//!   drawn.
//! - Every schedule is canonical for hydo-schedule (the controller's parser) and round-trips
//!   through the explorer's own parse/render; so does every shrink candidate.

use hydo_explore::gen::Ex;
use hydo_explore::space::Space;
use hydo_schedule::{GuestToken, Schedule};
use serde_json::{json, Value};

fn manifest(faults: Option<Value>) -> Value {
    let mut m = json!({
        "name": "t",
        "services": [
            {"name": "n1", "cmd": "a", "ready": "true", "port": 1, "storage": "vol/n1"},
            {"name": "n2", "cmd": "b", "ready": "true", "port": 2,
             "storage": {"dir": "vol/n2", "size_mib": 64}},
            {"name": "n3", "cmd": "c", "ready": "true", "port": 3},
        ],
        "workload": {"steps": 20, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
    });
    if let Some(f) = faults {
        m["faults"] = f;
    }
    m
}

fn ex(m: &Value, storage: bool) -> Ex {
    let sp = Space::from_text(&serde_json::to_string(m).unwrap(), None).unwrap();
    let mut e = Ex::default();
    sp.apply(&mut e);
    e.storage_faults = storage;
    e
}

#[test]
fn storage_faults_only_add_tokens_and_follow_the_rules() {
    let m = manifest(None);
    let (on, off) = (ex(&m, true), ex(&m, false));
    let (mut g, mut l, mut inplace) = (0, 0, 0);
    for st in ["random", "swarm", "swarm-pos", "process"] {
        for seed in 1..=800u64 {
            let base = off.compose_seed(st, seed, false);
            assert!(base.g.is_none() && base.l.is_none());
            let s = on.compose_seed(st, seed, false);
            let mut rest = s.clone();
            rest.g = None;
            rest.l = None;
            assert_eq!(rest, base, "{st} {seed}");
            if let Some(z) = &s.g {
                g += 1;
                assert!(z.svc == "n1" || z.svc == "n2", "{z:?}");
                assert!(z.at >= 1 && z.at < 20);
                if z.down.is_none() {
                    inplace += 1;
                }
            }
            if let Some((svc, at, down, strict)) = &s.l {
                assert!(!strict);
                l += 1;
                assert_eq!(svc, "n2");
                assert!(*at >= 1 && *at < 20 && (1..=3).contains(down));
            }
            let text = on.render(&s);
            let p = Schedule::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(p.to_string(), text);
            assert_eq!(
                p.guest
                    .iter()
                    .filter(|t| matches!(t, GuestToken::Corrupt { .. }))
                    .count(),
                s.g.is_some() as usize
            );
            assert_eq!(on.render(&on.parse(&text).unwrap()), text);
            for c in on.candidates(&s, None) {
                let t = on.render(&c);
                Schedule::parse(&t).unwrap_or_else(|e| panic!("candidate {t}: {e}"));
                assert_eq!(on.render(&on.parse(&t).unwrap()), t);
            }
        }
    }
    let n = 4 * 800;
    assert!(g > n / 3 && g < n * 2 / 3, "G= in {g} of {n}");
    assert!(l > n / 4 && l < n / 2, "L= in {l} of {n}");
    assert!(inplace > 0 && inplace < g);
}

#[test]
fn the_manifest_decides_which_storage_faults() {
    let only_corrupt = ex(&manifest(Some(json!(["kill", "corrupt"]))), true);
    let neither = ex(&manifest(Some(json!(["kill", "cut"]))), true);
    let (mut g, mut l) = (0, 0);
    for seed in 1..=400u64 {
        let s = only_corrupt.compose_seed("random", seed, false);
        g += s.g.is_some() as usize;
        l += s.l.is_some() as usize;
        let s = neither.compose_seed("random", seed, false);
        assert!(s.g.is_none() && s.l.is_none(), "{seed}");
    }
    assert!(g > 0);
    assert_eq!(l, 0);
    // a hand-written token for a service without storage is dropped by the space's filter
    let e = ex(&manifest(None), true);
    let s = e
        .parse("disk=none dseed=1 | run G=n3@4+1:flip L=n1@5+1")
        .unwrap();
    assert!(s.g.is_some() && s.l.is_some());
    assert_eq!(e.render(&s), "disk=none dseed=1 | run");
    let s = e
        .parse("disk=none dseed=1 | run G=n1@4:zero,len=512,seed=3 L=n2@5+2")
        .unwrap();
    assert_eq!(
        e.render(&s),
        "disk=none dseed=1 | run G=n1@4:zero,len=512,seed=3 L=n2@5+2"
    );
    assert_eq!(e.finding_class(&s), "corrupted-files");
}

/// `--corrupt-several` (Milestone 245): only the drawn `G=`'s target changes, to a group's
/// selector when a group's every member has `storage:`, else to a list of two services; every
/// schedule passes the controller's names check and round-trips, and so does every shrink
/// candidate, which takes the target towards fewer services. A strict `L=` written by hand
/// round-trips and shrinks towards the plain one.
#[test]
fn several_services_change_only_the_corruption_target() {
    let mut m = manifest(None);
    let plain = ex(&m, true);
    let mut several = ex(&m, true);
    several.corrupt_several = true;
    let p = plan(&m);
    let (mut lists, mut same) = (0, 0);
    for seed in 1..=400u64 {
        let base = plain.compose_seed("random", seed, false);
        let s = several.compose_seed("random", seed, false);
        let mut rest = s.clone();
        rest.g = base.g.clone();
        assert_eq!(rest, base, "{seed}");
        if let (Some(a), Some(b)) = (&base.g, &s.g) {
            if a.svc == b.svc {
                same += 1;
            } else {
                assert_eq!(b.svc, "n1,n2", "{seed}");
                lists += 1;
            }
            assert_eq!((a.at, a.down, &a.how), (b.at, b.down, &b.how));
        }
        check_all(&several, &p, &s);
    }
    assert!(lists > 0 && same > 0, "{lists} {same}");
    // with a group of the services with storage: its selectors
    m["groups"] = json!([{"name": "pair", "services": ["n1", "n2"]}]);
    let mut g = ex(&m, true);
    g.corrupt_several = true;
    let p = plan(&m);
    let mut sel = 0;
    for seed in 1..=400u64 {
        let s = g.compose_seed("process", seed, false);
        if let Some(z) = &s.g {
            if z.svc.starts_with("pair.") {
                sel += 1;
                let cs = g.candidates(&s, None);
                assert!(
                    cs.iter()
                        .any(|c| c.g.as_ref().is_some_and(|x| x.svc != z.svc)),
                    "{z:?}"
                );
            }
        }
        check_all(&g, &p, &s);
    }
    assert!(sel > 0);
    // a hand-written strict L= and a list G=
    let t = "disk=none dseed=1 | run G=n1,n2@4+1:flip L=n2@5+2:strict";
    let s = plain.parse(t).unwrap();
    assert_eq!(plain.render(&s), t);
    assert_eq!(s.l, Some(("n2".to_string(), 5, 2, true)));
    let cs = plain.candidates(&s, None);
    assert!(cs
        .iter()
        .any(|c| c.l == Some(("n2".to_string(), 5, 2, false))));
    assert!(cs
        .iter()
        .any(|c| c.g.as_ref().is_some_and(|x| x.svc == "n1" || x.svc == "n2")));
    // a list naming a service without storage is dropped by the space's filter
    let s = plain
        .parse("disk=none dseed=1 | run G=n1,n3@4:flip")
        .unwrap();
    assert_eq!(plain.render(&s), "disk=none dseed=1 | run");
}

fn plan(m: &Value) -> hydo_control::Plan {
    let text = serde_json::to_string(m).unwrap();
    let mm = hydo_manifest::from_embedded(&text, None).unwrap();
    hydo_control::Plan::from_manifest(&mm).unwrap()
}

fn check_all(e: &Ex, p: &hydo_control::Plan, s: &hydo_explore::sched::S) {
    let text = e.render(s);
    let parsed = Schedule::parse(&text).unwrap_or_else(|x| panic!("{text}: {x}"));
    assert_eq!(parsed.to_string(), text);
    assert_eq!(e.render(&e.parse(&text).unwrap()), text);
    hydo_control::names::check(p, &parsed).unwrap_or_else(|x| panic!("{text}: {x}"));
    for c in e.candidates(s, None) {
        let t = e.render(&c);
        let pc = Schedule::parse(&t).unwrap_or_else(|x| panic!("candidate {t}: {x}"));
        assert_eq!(e.render(&e.parse(&t).unwrap()), t);
        hydo_control::names::check(p, &pc).unwrap_or_else(|x| panic!("{t}: {x}"));
    }
}
