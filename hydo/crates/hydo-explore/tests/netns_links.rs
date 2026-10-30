//! The `Links` part of hydo/onboard/tests/test_netns.py (deleted in M107) in Rust: with
//! `network: bridge` the explorer draws only links that carry traffic (depends_on pairs and each
//! talking service's link to the rest), without depends_on every pair, and on the loopback every
//! pair as before. The validation and compile parts are in hydo-build's `tests/netns.rs`.

use hydo_explore::space::Space;
use serde_json::{json, Value};

fn base() -> Value {
    json!({
        "name": "t", "network": "bridge",
        "services": [
            {"name": "db", "cmd": "db", "ready": "true", "port": 5432, "addr": "10.89.0.11"},
            {"name": "api", "cmd": "api", "ready": "true", "port": 80, "addr": "10.89.0.12", "depends_on": ["db"]},
            {"name": "web", "cmd": "web", "ready": "true", "port": 80, "addr": "10.89.0.13", "depends_on": ["api"]},
        ],
        "workload": {"steps": 10, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
    })
}

fn links(m: &Value) -> Vec<String> {
    Space::from_text(&serde_json::to_string(m).unwrap(), None)
        .unwrap()
        .links
}

#[test]
fn talking_links_only() {
    assert_eq!(links(&base()), ["db-api", "api-web", "api-*"]);
}

#[test]
fn peers_without_depends_on() {
    let mut m = base();
    for s in m["services"].as_array_mut().unwrap() {
        s.as_object_mut().unwrap().remove("depends_on");
    }
    assert_eq!(
        links(&m),
        ["db-api", "db-web", "api-web", "db-*", "api-*", "web-*"]
    );
}

#[test]
fn loopback_keeps_every_pair() {
    let mut m = base();
    m.as_object_mut().unwrap().remove("network");
    for (i, s) in m["services"].as_array_mut().unwrap().iter_mut().enumerate() {
        s["addr"] = json!(format!("127.0.0.{}", 11 + i));
    }
    assert_eq!(
        links(&m),
        ["db-api", "db-web", "api-web", "db-*", "api-*", "web-*"]
    );
}
