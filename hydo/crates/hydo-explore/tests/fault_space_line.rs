//! The line `hydo explore` prints before its runs (Milestone 247): the fault kinds it draws in
//! words, and what `--new-faults` adds; before, it printed the fault space as JSON, which is now
//! `fault-space.json` in the exploration's folder.

use hydo_explore::space::Space;
use serde_json::json;

fn redis(faults: Option<serde_json::Value>) -> Space {
    let mut m = json!({
        "name": "redis",
        "services": [{"name": "redis", "cmd": "redis-server", "ready": "true", "port": 6379}],
        "workload": {"steps": 20, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
        "knobs": [{"token": "R", "name": "Redis appendfsync", "values": ["always", "everysec", "no"], "default": "always", "set": "true"}],
    });
    if let Some(f) = faults {
        m["faults"] = f;
    }
    Space::from_text(&serde_json::to_string(&m).unwrap(), None).unwrap()
}

#[test]
fn the_kinds_it_draws_in_words() {
    let sp = redis(None);
    assert_eq!(
        sp.plain(false, "random"),
        "hydo explore: the faults it draws: crashes, network faults, power cuts, disk faults and \
         your setting R (service: redis). --new-faults adds pauses, clock jumps and one-way \
         network faults."
    );
    // with the option, or a strategy that draws them, they are in the list
    for (nf, strategy) in [(true, "random"), (false, "swarm-pos")] {
        let l = sp.plain(nf, strategy);
        assert!(
            l.contains("disk faults, pauses, clock jumps, one-way network faults and"),
            "{l}"
        );
        assert!(!l.contains("--new-faults"), "{l}");
    }
    // a project's `faults:` list keeps the others out
    assert_eq!(
        redis(Some(json!(["kill", "cut", "pause"]))).plain(false, "swarm"),
        "hydo explore: the faults it draws: crashes, power cuts and your setting R (service: \
         redis). --new-faults adds pauses."
    );
    // no JSON on the line
    assert!(!sp.plain(false, "random").contains('{'));
}
