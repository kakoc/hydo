//! `--cluster-faults` (Milestone 195): kills and pauses of a group's members and partitions of a
//! group, drawn on top of every strategy's schedules from their own stream.
//!
//! - Without the option nothing changes: a manifest with `groups:` draws the schedules it drew
//!   before the option existed.
//! - With it only K=, P=, N= (and O=, which a partition replaces) change; every other field of a
//!   seed's schedule stays as it was.
//! - Every schedule is canonical for hydo-schedule, round-trips through the explorer's parse and
//!   render, and passes the controller's names check against the manifest's plan; so does every
//!   shrink candidate, and the candidates take a selector towards one service.

use hydo_explore::gen::Ex;
use hydo_explore::space::Space;
use hydo_schedule::Schedule;
use serde_json::{json, Value};

fn manifest() -> Value {
    json!({
        "name": "t",
        "network": "bridge",
        "services": [
            {"name": "n1", "cmd": "a", "ready": "true", "port": 1, "addr": "10.89.0.11"},
            {"name": "n2", "cmd": "b", "ready": "true", "port": 2, "addr": "10.89.0.12"},
            {"name": "n3", "cmd": "c", "ready": "true", "port": 3, "addr": "10.89.0.13"},
            {"name": "web", "cmd": "w", "ready": "true", "port": 4, "addr": "10.89.0.20"},
        ],
        "groups": [
            {"name": "nodes", "services": ["n1", "n2", "n3"], "primary": "leader"},
            {"name": "pair", "services": ["n1", "web"]},
        ],
        "workload": {"steps": 20, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
    })
}

fn ex(m: &Value, cluster: bool) -> Ex {
    let sp = Space::from_text(&serde_json::to_string(m).unwrap(), None).unwrap();
    let mut e = Ex::default();
    sp.apply(&mut e);
    e.cluster_faults = cluster;
    e
}

fn plan(m: &Value) -> hydo_control::Plan {
    let text = serde_json::to_string(m).unwrap();
    let mm = hydo_manifest::from_embedded(&text, None).unwrap();
    hydo_control::Plan::from_manifest(&mm).unwrap()
}

#[test]
fn cluster_faults_change_only_kills_pauses_and_partitions() {
    let m = manifest();
    let (on, off) = (ex(&m, true), ex(&m, false));
    let p = plan(&m);
    let (mut k, mut pz, mut n, mut shrunk) = (0, 0, 0, 0);
    for st in ["random", "swarm", "swarm-pos", "process"] {
        for seed in 1..=500u64 {
            let base = off.compose_seed(st, seed, false);
            let s = on.compose_seed(st, seed, false);
            let mut rest = s.clone();
            rest.k = base.k.clone();
            rest.p = base.p.clone();
            rest.n = base.n.clone();
            rest.o = base.o.clone();
            assert_eq!(rest, base, "{st} {seed}");
            k += s.k.as_ref().is_some_and(|x| x.0.contains('.')) as usize;
            pz += s.p.as_ref().is_some_and(|x| x.svc().contains('.')) as usize;
            n += s.n.as_ref().is_some_and(|x| x.0.contains('.')) as usize;
            let text = on.render(&s);
            let parsed = Schedule::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(parsed.to_string(), text);
            assert_eq!(on.render(&on.parse(&text).unwrap()), text);
            hydo_control::names::check(&p, &parsed).unwrap_or_else(|e| panic!("{text}: {e}"));
            for c in on.candidates(&s, None) {
                let t = on.render(&c);
                let pc = Schedule::parse(&t).unwrap_or_else(|e| panic!("candidate {t}: {e}"));
                hydo_control::names::check(&p, &pc).unwrap_or_else(|e| panic!("{t}: {e}"));
                if c.k != s.k && c.k.is_some() && !c.k.as_ref().unwrap().0.contains("majority") {
                    shrunk += 1;
                }
            }
        }
    }
    let total = 4 * 500;
    assert!(k > total / 3, "K= with a selector in {k} of {total}");
    assert!(pz > total / 6, "P= with a selector in {pz} of {total}");
    assert!(n > total / 6, "N= with a shape in {n} of {total}");
    assert!(shrunk > 0);
}

#[test]
fn a_selector_shrinks_towards_one_service() {
    let e = ex(&manifest(), true);
    assert_eq!(e.shrink_target("nodes.majority.2", false), ["nodes.one"]);
    assert_eq!(e.shrink_target("nodes.one.1", false), ["n2"]);
    assert_eq!(e.shrink_target("nodes.one", false), ["n1"]);
    assert_eq!(
        e.shrink_target("n1,n2,n3", false),
        ["n2,n3", "n1,n3", "n1,n2"]
    );
    assert_eq!(e.shrink_target("n1", false), Vec::<String>::new());
    assert_eq!(e.shrink_target("nodes.ring", true), ["nodes.isolate"]);
    assert_eq!(
        e.shrink_target("nodes.isolate.1", true),
        Vec::<String>::new()
    );
    assert_eq!(e.shrink_target("n1-n2", true), Vec::<String>::new());
    // a hand-written target of a group the project lacks is dropped by the space's filter
    let s = e
        .parse("disk=none dseed=1 | run K=ghost.all@2+1 N=ghost.halves@1-3:loss,100%")
        .unwrap();
    assert_eq!(e.render(&s), "disk=none dseed=1 | run");
    let s = e
        .parse("disk=none dseed=1 | run K=nodes.all@2+1 N=nodes.halves@1-3:loss,100%")
        .unwrap();
    assert_eq!(
        e.render(&s),
        "disk=none dseed=1 | run K=nodes.all@2+1 N=nodes.halves@1-3:loss,100%"
    );
}
