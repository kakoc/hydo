//! `--pause-workload` (Milestone 216): pauses of the workload's own process tree,
//! `P=workload@<at>~<off>:<dur>`, drawn on top of every strategy's schedules from their own stream.
//!
//! - Without the option nothing changes.
//! - With it only `P=` changes; every other field of a seed's schedule stays as it was.
//! - Every schedule is canonical for hydo-schedule, round-trips through the explorer's parse and
//!   render, and passes the controller's names check, for a test program (no services, no
//!   `faults:`) and for a system; so does every shrink candidate.

use hydo_explore::gen::Ex;
use hydo_explore::sched::Pause;
use hydo_explore::space::Space;
use hydo_schedule::Schedule;
use serde_json::{json, Value};

fn test_program() -> Value {
    json!({
        "name": "t",
        "workload": {"kind": "test", "steps": 1, "run": "/bin/pkg.test"},
        "invariants": [{"id": "x", "cmd": "true"}],
    })
}

fn system() -> Value {
    json!({
        "name": "s",
        "services": [
            {"name": "db", "cmd": "a", "ready": "true", "port": 1},
            {"name": "web", "cmd": "b", "ready": "true", "port": 2},
        ],
        "workload": {"steps": 30, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
    })
}

fn ex(m: &Value, on: bool) -> Ex {
    let sp = Space::from_text(&serde_json::to_string(m).unwrap(), None).unwrap();
    let mut e = Ex::default();
    sp.apply(&mut e);
    e.workload_pauses = on;
    e
}

fn plan(m: &Value) -> hydo_control::Plan {
    let text = serde_json::to_string(m).unwrap();
    let mm = hydo_manifest::from_embedded(&text, None).unwrap();
    hydo_control::Plan::from_manifest(&mm).unwrap()
}

fn check(m: &Value, steps: i64) {
    let (on, off) = (ex(m, true), ex(m, false));
    let p = plan(m);
    let (mut pauses, mut early, mut long) = (0, 0, 0);
    for st in ["random", "swarm", "swarm-pos", "process"] {
        for seed in 1..=400u64 {
            let base = off.compose_seed(st, seed, false);
            let s = on.compose_seed(st, seed, false);
            let mut rest = s.clone();
            rest.p = base.p.clone();
            assert_eq!(rest, base, "{st} {seed}");
            let text = on.render(&s);
            if let Some(Pause::Timed(t, at, o, d)) = &on.parse(&text).unwrap().p {
                if t == "workload" {
                    pauses += 1;
                    assert!((0..steps).contains(at), "{st} {seed}: step {at}");
                    assert!((0..=1000).contains(o), "{st} {seed}: start {o}");
                    early += (*o < 10) as usize;
                    long += (*d >= 50) as usize;
                }
            }
            let parsed = Schedule::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(parsed.to_string(), text);
            assert_eq!(on.render(&on.parse(&text).unwrap()), text);
            hydo_control::names::check(&p, &parsed).unwrap_or_else(|e| panic!("{text}: {e}"));
            for c in on.candidates(&s, None) {
                let t = on.render(&c);
                let pc = Schedule::parse(&t).unwrap_or_else(|e| panic!("candidate {t}: {e}"));
                hydo_control::names::check(&p, &pc).unwrap_or_else(|e| panic!("{t}: {e}"));
            }
        }
    }
    let total = 4 * 400;
    assert!(
        pauses > total * 2 / 5 && pauses < total * 3 / 5,
        "{pauses} of {total}"
    );
    // half of the starts are drawn on a log scale: about a quarter start in the first 10 ms
    assert!(
        early > pauses / 5,
        "{early} of {pauses} start in the first 10 ms"
    );
    assert!(long > pauses / 3, "{long} of {pauses} last 50 ms or more");
}

#[test]
fn workload_pauses_of_a_test_program() {
    check(&test_program(), 1);
}

#[test]
fn workload_pauses_of_a_systems_workload() {
    check(&system(), 30);
}

#[test]
fn a_workload_pause_shrinks_shorter_and_earlier() {
    let e = ex(&test_program(), true);
    let s = e
        .parse("disk=none dseed=0 | run P=workload@0~4:45")
        .unwrap();
    let c: Vec<String> = e.candidates(&s, None).iter().map(|c| e.render(c)).collect();
    assert!(c.contains(&"disk=none dseed=0 | run".to_string()), "{c:#?}");
    assert!(
        c.contains(&"disk=none dseed=0 | run P=workload@0~4:22".to_string()),
        "{c:#?}"
    );
    assert!(
        c.contains(&"disk=none dseed=0 | run P=workload@0~0:45".to_string()),
        "{c:#?}"
    );
    // a hand-written pause stays, though the project's faults: has no pause
    assert_eq!(e.render(&s), "disk=none dseed=0 | run P=workload@0~4:45");
}

/// `--pause-thread` (Milestone 226): the same draws as `--pause-workload`, with the target drawn
/// last among the named threads (and `workload` with `--pause-workload` too). Every schedule
/// parses, round-trips, passes the names check, and so does every shrink candidate.
#[test]
fn thread_pauses_draw_the_named_threads() {
    let m = test_program();
    let p = plan(&m);
    let whole = ex(&m, true);
    let mut threads = ex(&m, true);
    threads.pause_targets = vec![
        "workload/tests::wal_reco".to_string(),
        "workload/tokio-rt-worker#2".to_string(),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for seed in 1..=400u64 {
        let base = whole.compose_seed("random", seed, false);
        let s = threads.compose_seed("random", seed, false);
        match (&base.p, &s.p) {
            (Some(Pause::Timed(t0, a0, o0, d0)), Some(Pause::Timed(t, a, o, d))) => {
                assert_eq!(t0, "workload");
                assert!(threads.pause_targets.contains(t), "{seed}: {t}");
                assert_eq!((a0, o0, d0), (a, o, d), "{seed}");
                seen.insert(t.clone());
            }
            (b, s) => assert_eq!(b.is_some(), s.is_some(), "{seed}"),
        }
        let mut rest = s.clone();
        rest.p = base.p.clone();
        assert_eq!(rest, base, "{seed}");
        let text = threads.render(&s);
        let parsed = Schedule::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(parsed.to_string(), text);
        assert_eq!(threads.render(&threads.parse(&text).unwrap()), text);
        hydo_control::names::check(&p, &parsed).unwrap_or_else(|e| panic!("{text}: {e}"));
        for c in threads.candidates(&s, None) {
            let t = threads.render(&c);
            let pc = Schedule::parse(&t).unwrap_or_else(|e| panic!("candidate {t}: {e}"));
            hydo_control::names::check(&p, &pc).unwrap_or_else(|e| panic!("{t}: {e}"));
        }
    }
    assert_eq!(seen.len(), 2, "{seen:?}");
    // --pause-min and --pause-lengths: every start in the range, every length from the list
    let mut narrow = ex(&m, true);
    narrow.pause_targets = vec!["workload/tests::wal_reco".to_string()];
    (narrow.pause_min, narrow.pause_max) = (4400, 4500);
    narrow.pause_lengths = vec![500, 1000];
    let (mut low, mut lens) = (0, std::collections::BTreeSet::new());
    for seed in 1..=400u64 {
        if let Some(Pause::Timed(_, _, o, d)) = &narrow.compose_seed("random", seed, false).p {
            assert!((4400..=4500).contains(o), "{seed}: start {o}");
            low += (*o < 4410) as usize;
            lens.insert(*d);
        }
    }
    assert!(low > 20, "{low} starts in the first 10 ms of the range");
    assert_eq!(lens.into_iter().collect::<Vec<_>>(), [500, 1000]);
    let s = threads
        .parse("disk=none dseed=0 | run P=workload/tests::wal_reco@0~1800:200")
        .unwrap();
    let c: Vec<String> = threads
        .candidates(&s, None)
        .iter()
        .map(|c| threads.render(c))
        .collect();
    assert!(
        c.contains(&"disk=none dseed=0 | run P=workload/tests::wal_reco@0~1800:100".to_string()),
        "{c:#?}"
    );
}
