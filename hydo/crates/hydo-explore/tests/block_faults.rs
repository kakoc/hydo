//! `--block-faults` (Milestone 313, docs/design/block-faults.md): a torn write (`torn=`), a
//! misdirected write (`mis=`), a request that never completes (`hang=`) or a latent sector error
//! under a file of a service's storage (`G=...:bad|rot`), drawn on top of every strategy's
//! schedules from their own stream.
//!
//! - Every other field of a seed's schedule stays as it was: a schedule with block faults is the
//!   schedule without them plus one block fault, so an exploration without the flag draws
//!   exactly as before (the identity goldens and `fake_engine` hold that).
//! - A torn write only with the cut disk and a power cut; `G=...:bad|rot` only on a service with
//!   `storage:` and in a schedule with no `G=` yet; the manifest's `faults:` decide (`cut` for a
//!   torn write, `disk` for the others).
//! - Every schedule is canonical for hydo-schedule and round-trips through the explorer's own
//!   parse and render; so does every shrink candidate, and each class is `damaged-disk`.

use hydo_explore::gen::Ex;
use hydo_explore::space::Space;
use hydo_schedule::Schedule;
use serde_json::{json, Value};

fn manifest(faults: Option<Value>) -> Value {
    let mut m = json!({
        "name": "t",
        "services": [
            {"name": "db", "cmd": "a", "ready": "true", "port": 1, "storage": "vol/db"},
            {"name": "web", "cmd": "b", "ready": "true", "port": 2},
        ],
        "workload": {"steps": 20, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
    });
    if let Some(f) = faults {
        m["faults"] = f;
    }
    m
}

fn ex(m: &Value, block: bool) -> Ex {
    let sp = Space::from_text(&serde_json::to_string(m).unwrap(), None).unwrap();
    let mut e = Ex::default();
    sp.apply(&mut e);
    e.block_faults = block;
    e
}

fn sectors(s: &hydo_explore::sched::S) -> bool {
    s.g.as_ref()
        .is_some_and(|g| g.how.starts_with("bad") || g.how.starts_with("rot"))
}

#[test]
fn block_faults_only_add_one_fault_and_follow_the_rules() {
    let m = manifest(None);
    let (on, off) = (ex(&m, true), ex(&m, false));
    let mut kinds = std::collections::BTreeMap::new();
    for st in ["random", "swarm", "swarm-pos", "process"] {
        for seed in 1..=600u64 {
            let base = off.compose_seed(st, seed, false);
            assert!(base.blk.is_empty());
            let s = on.compose_seed(st, seed, false);
            let mut rest = s.clone();
            rest.blk.clear();
            if sectors(&s) {
                rest.g = None;
            }
            assert_eq!(rest, base, "{st} {seed}");
            assert!(
                s.blk.len() + sectors(&s) as usize <= 1,
                "{st} {seed}: {s:?}"
            );
            for b in &s.blk {
                let k = b.split('=').next().unwrap().to_string();
                if k == "torn" {
                    assert_eq!(s.disk, "cut");
                }
                *kinds.entry(k).or_insert(0) += 1;
            }
            if let Some(g) = s.g.as_ref().filter(|_| sectors(&s)) {
                assert_eq!(g.svc, "db");
                *kinds.entry("G".to_string()).or_insert(0) += 1;
            }
            let text = on.render(&s);
            let p = Schedule::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(p.to_string(), text);
            assert_eq!(on.render(&on.parse(&text).unwrap()), text);
            if !s.blk.is_empty() || sectors(&s) {
                // a disk that returns errors or throws writes away keeps its own class
                if !s.disk_is("ioerr") && !s.disk_is("lying") {
                    assert_eq!(on.finding_class(&s), "damaged-disk", "{text}");
                }
                assert_eq!(p.block_faults().len(), 1, "{text}");
            }
            for c in on.candidates(&s, None) {
                let t = on.render(&c);
                Schedule::parse(&t).unwrap_or_else(|e| panic!("candidate {t}: {e}"));
                assert_eq!(on.render(&on.parse(&t).unwrap()), t);
            }
        }
    }
    for k in ["torn", "mis", "hang", "G"] {
        assert!(kinds.get(k).copied().unwrap_or(0) > 20, "{k}: {kinds:?}");
    }
}

#[test]
fn the_manifest_decides_which_block_faults() {
    // without `disk` no mis=, hang= or bad sectors under a file; without `cut` no torn write
    let only_cut = ex(&manifest(Some(json!(["kill", "cut"]))), true);
    let only_disk = ex(&manifest(Some(json!(["kill", "disk", "corrupt"]))), true);
    for seed in 1..=300u64 {
        let s = only_cut.compose_seed("random", seed, false);
        let t = only_cut.render(&s);
        let p = Schedule::parse(&t).unwrap();
        assert!(
            p.block_faults().iter().all(|b| b.starts_with("torn=")),
            "{t}"
        );
        let s = only_disk.compose_seed("random", seed, false);
        let t = only_disk.render(&s);
        assert!(!t.contains("torn="), "{t}");
    }
    // a hand-written block fault round-trips, and shrinks by leaving it out
    let e = ex(&manifest(None), true);
    let t = "disk=cut dseed=1 torn=50:any | run C=4";
    let s = e.parse(t).unwrap();
    assert_eq!(e.render(&s), t);
    let c: Vec<String> = e.candidates(&s, None).iter().map(|c| e.render(c)).collect();
    assert!(
        c.contains(&"disk=cut dseed=1 torn=50 | run C=4".to_string()),
        "{c:?}"
    );
    assert!(c.iter().any(|x| !x.contains("torn=")), "{c:?}");
    // a candidate without the cut disk cannot keep the torn write
    let mut none = s.clone();
    none.disk = "none".into();
    assert!(!e.render(&none).contains("torn="));
    let t = "disk=none dseed=1 mis=7 hang=3+1000 | run";
    assert_eq!(e.render(&e.parse(t).unwrap()), t);
}
