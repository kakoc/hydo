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
