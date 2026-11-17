//! Walks through a web app's pages (Milestone 258, docs/design/ui-exploration.md): a manifest
//! with `browser.explore` gets `I=<seed>` in every schedule.
//!
//! - A manifest without `browser.explore` draws no walk, so its schedules stay as they were.
//! - With it only `I=` is added; every other field of a seed's schedule stays as it was.
//! - Every schedule round-trips through hydo-schedule and the explorer's parse and render and
//!   passes the controller's names check; so does every shrink candidate.
//! - The shrinker cuts a walk by leaving out whole steps (the last first), halves of a step,
//!   fewer actions per step and the first or the last action of a step (Milestone 264), and the
//!   walk is the last thing it cuts.
//! - A violation's signature blanks generated ids, so a shrink candidate whose walk made
//!   another note keeps the failure (Milestone 264).

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
    // Milestone 264: the manifest's 8 actions in each of 6 steps. First a whole step, the last
    // step first; then the later and the earlier half of a step; then half and one less in
    // every step; then the first and the last action of each step
    let mut want: Vec<String> = (0..6).rev().map(|st| format!("9f3a,x{st}.0-7")).collect();
    for st in (0..6).rev() {
        want.push(format!("9f3a,x{st}.4-7"));
        want.push(format!("9f3a,x{st}.0-3"));
    }
    want.push("9f3a,n4".into());
    want.push("9f3a,n7".into());
    for st in 0..6 {
        want.push(format!("9f3a,x{st}.0"));
        want.push(format!("9f3a,x{st}.7"));
    }
    assert_eq!(walks, want);
    // a walk cut to one action per step is cut further only by leaving out a whole step
    let s = e.parse("disk=none dseed=1 | run I=9f3a,n1").unwrap();
    let w: Vec<String> = e
        .candidates(&s, None)
        .into_iter()
        .filter_map(|c| c.i)
        .collect();
    assert_eq!(w.len(), 6);
    assert_eq!(w[0], "9f3a,n1,x5.0");
    // a walk with no action left is not cut further
    let none = (0..6)
        .map(|st| format!("x{st}.0"))
        .collect::<Vec<_>>()
        .join(",");
    let s = e
        .parse(&format!("disk=none dseed=1 | run I=9f3a,n1,{none}"))
        .unwrap();
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
    assert!(w.contains(&"9f3a,n2,x1.1".to_string()), "{w:?}");
    assert!(w.contains(&"9f3a,n3,x1.1".to_string()), "{w:?}");
    // the steps after a failure go first: a walk whose later steps are gone keeps its earlier
    // actions as they were
    assert_eq!(w[0], "9f3a,n4,x0.3,x1.1,x5.0-3");
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

/// Milestone 264: the page of one note and the same page of another are one failure; numbers
/// were `#` before, and words keep their letters.
#[test]
fn a_violation_signature_has_no_generated_ids() {
    use hydo_explore::report::{same_failure, sig_key};
    let a = "HYDO-ORACLE: VIOLATION [browser.no_page_errors] The page /memos/b4R4VE2u3vE8gqW7sZTJtL \
             left a promise rejection unhandled during the test \"walk step 2\": TypeError: Cannot \
             read properties of undefined (reading 'writeText') (2 times in this run; this is the first)";
    let b = a
        .replace("b4R4VE2u3vE8gqW7sZTJtL", "ZCRLYKjHcY8ATGX7oPH7KF")
        .replace("step 2", "step 3");
    assert!(same_failure(a, &b));
    assert_eq!(
        sig_key(a),
        "VIOLATION [browser.no_page_errors] The page /memos/<id> left a promise rejection \
         unhandled during the test \"walk step #\": TypeError: Cannot read properties of \
         undefined (reading 'writeText') (# times in this run; this is the first)"
    );
    // other generated ids: hex, a uuid's parts, a shorter random mix, sixteen letters in mixed
    // case; words stay: an RPC method, a number in a word
    for (v, want) in [
        (
            "VIOLATION [web.failures_shown] order 9f86d081884c7d65 lost",
            "VIOLATION [web.failures_shown] order <id> lost",
        ),
        (
            "VIOLATION [web.failures_shown] /o/3f2b8c1e-7d4a-4e2b-9c1d-2a3b4c5d6e7f",
            "VIOLATION [web.failures_shown] /o/<id>-#d#a-#e#b-#c#d-<id>",
        ),
        ("VIOLATION [web.failures_shown] /n/qjGcd4lJvd", "VIOLATION [web.failures_shown] /n/<id>"),
        (
            "VIOLATION [web.failures_shown] /n/bRxVEuvEgqWsZTJtLk",
            "VIOLATION [web.failures_shown] /n/<id>",
        ),
        (
            "VIOLATION [web.failures_shown] POST /memos.api.MemoService/UpsertMemoReaction got no answer",
            "VIOLATION [web.failures_shown] POST /memos.api.MemoService/UpsertMemoReaction got no answer",
        ),
        (
            "VIOLATION [web.failures_shown] key k:12 lost on thread12",
            "VIOLATION [web.failures_shown] key k:# lost on thread#",
        ),
    ] {
        assert_eq!(sig_key(v), want, "{v}");
    }
    // an error, and a violation of any other invariant, keep their signatures as before: only
    // the page invariants' ids are blanked
    assert_eq!(
        sig_key("ERROR boot 9f86d081884c7d65"),
        "ERROR boot #f#d#c#d#"
    );
    assert_eq!(
        sig_key("VIOLATION error-log [app] config reloaded (4c53c64297e2)"),
        "VIOLATION error-log [app] config reloaded (#c#c#e#)"
    );
    assert_eq!(
        sig_key("VIOLATION [memos.saved_memos_survive] memo memos/J2gBW9VwmmuwFTuefuZpLK lost"),
        "VIOLATION [memos.saved_memos_survive] memo memos/J#gBW#VwmmuwFTuefuZpLK lost"
    );
}

/// Milestone 301: an exploration counts its runs by their part, for the progress line it prints
/// every 30 s (0.4.0's trial: a walk's exploration printed nothing for 30 minutes, most of them
/// shrinking).
#[test]
fn the_progress_line_counts_the_runs_by_their_part() {
    use hydo_explore::results::Progress;
    let p = Progress::default();
    let run =
        |role: &str, kind: &str| json!({"type": "run", "role": role, "verdict": {"kind": kind}});
    for r in [
        run("first", "ok"),
        run("first", "violation"),
        run("first", "ok"),
        run("rerun", "violation"),
        run("shrink", "ok"),
        run("shrink", "violation"),
        run("minimal", "violation"),
        run("probe", "ok"),
        json!({"type": "end"}),
    ] {
        p.count(&r);
    }
    assert_eq!(
        p.line(90),
        "hydo explore: 1 min 30 s in: 3 schedules run (1 failed), 1 replay of a failure, \
         3 shrink runs and 1 other run"
    );
    assert_eq!(
        Progress::default().line(30),
        "hydo explore: 30 s in: 0 schedules run (0 failed)"
    );
}
