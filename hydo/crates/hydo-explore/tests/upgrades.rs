//! `--upgrades` (Milestone 238, docs/design/upgrades.md): upgrades of the services with
//! `versions:`, `U=<target>@<at>[/<gap>][+<back>]:<version>`, drawn on top of every strategy's
//! schedules from their own stream.
//!
//! - Without the option nothing changes, and a manifest without `versions:` draws none.
//! - With it only `U=` is added; every other field of a seed's schedule stays as it was.
//! - Every schedule is canonical for hydo-schedule, round-trips through the explorer's parse and
//!   render, passes the controller's names check, and has every switch on a workload step; so
//!   does every shrink candidate.
//! - A schedule with `U=` is in the class `upgrade`.

use hydo_explore::gen::Ex;
use hydo_explore::space::Space;
use hydo_schedule::{GuestToken, Schedule};
use serde_json::{json, Value};

fn cluster(versions: bool) -> Value {
    let node = |n: u32| {
        let mut s = json!({
            "name": format!("n{n}"), "cmd": format!("node-old {n}"), "ready": "true",
            "port": 2379 + n, "addr": format!("127.0.0.1{n}"),
        });
        if versions {
            s["version"] = json!("3.5.5");
            s["versions"] = json!([
                {"name": "3.6.0", "cmd": format!("node-new {n}")},
                {"name": "3.6.0-rc.2", "cmd": format!("node-rc {n}")},
            ]);
        }
        s
    };
    json!({
        "name": "c",
        "groups": [{"name": "nodes", "services": ["n1", "n2", "n3"]}],
        "services": [node(1), node(2), node(3)],
        "workload": {"steps": 20, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
    })
}

fn ex(m: &Value, on: bool) -> Ex {
    let sp = Space::from_text(&serde_json::to_string(m).unwrap(), None).unwrap();
    let mut e = Ex::default();
    sp.apply(&mut e);
    e.upgrades = on;
    e
}

fn plan(m: &Value) -> hydo_control::Plan {
    let text = serde_json::to_string(m).unwrap();
    let mm = hydo_manifest::from_embedded(&text, None).unwrap();
    hydo_control::Plan::from_manifest(&mm).unwrap()
}

#[test]
fn upgrades_are_drawn_from_their_own_stream() {
    let m = cluster(true);
    let (on, off) = (ex(&m, true), ex(&m, false));
    let p = plan(&m);
    let (mut drawn, mut rolling, mut back) = (0, 0, 0);
    for st in ["random", "swarm", "swarm-pos", "process"] {
        for seed in 1..=300u64 {
            let base = off.compose_seed(st, seed, false);
            let s = on.compose_seed(st, seed, false);
            let mut rest = s.clone();
            rest.u = Vec::new();
            assert_eq!(rest, base, "{st} {seed}");
            assert!(base.u.is_empty());
            let text = on.render(&s);
            let parsed = Schedule::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(parsed.to_string(), text);
            assert_eq!(on.render(&on.parse(&text).unwrap()), text);
            hydo_control::names::check(&p, &parsed).unwrap_or_else(|e| panic!("{text}: {e}"));
            for t in &parsed.guest {
                if let GuestToken::Upgrade(u) = t {
                    drawn += 1;
                    let n = if u.target.service().is_some() { 1 } else { 3 };
                    assert!(u.last_step(n) < 20, "{text}");
                    rolling += (n == 3) as usize;
                    back += u.back.is_some() as usize;
                    // a disk fault or a corruption names its own class first
                    if !s.disk_is("ioerr") && !s.disk_is("lying") && s.g.is_none() {
                        assert_eq!(on.finding_class(&s), "upgrade", "{text}");
                    }
                }
            }
            for c in on.candidates(&s, None) {
                let t = on.render(&c);
                let pc = Schedule::parse(&t).unwrap_or_else(|e| panic!("candidate {t}: {e}"));
                hydo_control::names::check(&p, &pc).unwrap_or_else(|e| panic!("{t}: {e}"));
            }
        }
    }
    let total = 4 * 300;
    assert!(
        drawn > total / 2 && drawn < total * 7 / 10,
        "{drawn} of {total}"
    );
    assert!(
        rolling > drawn / 3 && rolling < drawn * 2 / 3,
        "{rolling} of {drawn}"
    );
    assert!(back > drawn / 8 && back < drawn / 3, "{back} of {drawn}");
}

/// Version names written as `{{param}}` are drawn as the image was built: with the manifest's
/// defaults, and with the image's values (`/etc/hydo/params`) once they are applied.
#[test]
fn version_names_from_parameters() {
    let mut m = cluster(true);
    m["params"] = json!({"old": "3.5.5", "new": "3.6.0"});
    for n in 0..3 {
        m["services"][n]["version"] = json!("{{old}}");
        m["services"][n]["versions"] = json!([{"name": "{{new}}"}]);
    }
    let sp = Space::from_text(&serde_json::to_string(&m).unwrap(), None).unwrap();
    assert_eq!(sp.versions[0].1, "3.5.5");
    assert_eq!(sp.versions[0].2, vec!["3.6.0".to_string()]);
    let sp = sp.with_params(&[("new".into(), "3.6.1".into())]);
    assert_eq!(sp.versions[2].2, vec!["3.6.1".to_string()]);
    let mut e = Ex::default();
    sp.apply(&mut e);
    e.upgrades = true;
    let drawn: Vec<String> = (1..=100u64)
        .flat_map(|seed| e.compose_seed("random", seed, false).u)
        .collect();
    assert!(!drawn.is_empty());
    assert!(drawn.iter().all(|u| u.ends_with(":3.6.1")), "{drawn:?}");
}

#[test]
fn without_versions_or_without_the_option_nothing_is_drawn() {
    for (m, on) in [(cluster(false), true), (cluster(true), false)] {
        let e = ex(&m, on);
        for seed in 1..=200u64 {
            assert!(e.compose_seed("random", seed, false).u.is_empty());
        }
    }
}

#[test]
fn an_upgrade_shrinks_by_going_by_not_rolling_back_and_by_one_step() {
    let e = ex(&cluster(true), true);
    let s = e
        .parse("disk=none dseed=0 | run U=nodes.all@2/3+2:3.6.0")
        .unwrap();
    let c: Vec<String> = e.candidates(&s, None).iter().map(|c| e.render(c)).collect();
    for want in [
        "disk=none dseed=0 | run",
        "disk=none dseed=0 | run U=nodes.all@2/3:3.6.0",
        "disk=none dseed=0 | run U=nodes.all@2+2:3.6.0",
    ] {
        assert!(c.contains(&want.to_string()), "{want} not in {c:#?}");
    }
}
