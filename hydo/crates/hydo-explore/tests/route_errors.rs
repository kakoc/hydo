//! The explorer and a fake route's `errors:` (Milestone 249, the fakes `hydo fake from-openapi`
//! writes): `--protocol-faults` draws the exact statuses a route lists besides hydo's own four
//! (500, 502, 503, 429), and a route without `errors:` draws the same schedules as before.

use hydo_explore::gen::Ex;
use hydo_explore::space::Space;
use serde_json::{json, Value};

fn system(errors: Option<Value>) -> Value {
    let mut create = json!({"name": "create", "method": "POST", "path": "/v1/payouts"});
    if let Some(e) = errors {
        create["errors"] = e;
    }
    json!({
        "name": "s",
        "services": [{"name": "app", "cmd": "a", "ready": "true", "port": 8080}],
        "fakes": [{"name": "pay", "port": 9001, "routes": [create,
            {"name": "get", "method": "GET", "path": "/v1/payouts/{id}"}]}],
        "workload": {"steps": 20, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
        "faults": ["protocol"],
    })
}

fn schedules(m: &Value) -> Vec<String> {
    let sp = Space::from_text(&serde_json::to_string(m).unwrap(), None).unwrap();
    let mut e = Ex::default();
    sp.apply(&mut e);
    e.new_faults = true;
    e.protocol_faults = true;
    (1..=400u64)
        .map(|seed| e.render(&e.compose_seed("random", seed, false)))
        .collect()
}

#[test]
fn a_route_s_listed_statuses_are_drawn() {
    let with = schedules(&system(Some(
        json!({"409": "{\"error\":\"conflict\"}", "5XX": "", "default": ""}),
    )));
    let tokens: Vec<&str> = with
        .iter()
        .flat_map(|t| t.split_whitespace())
        .filter(|w| w.starts_with("H="))
        .collect();
    let on = |route: &str| {
        tokens
            .iter()
            .filter(|w| w.starts_with(&format!("H={route}@")) && w.contains("status=409"))
            .count()
    };
    assert!(
        on("pay.create") > 0,
        "no status=409 on pay.create: {tokens:?}"
    );
    // a class or default is no status of its own, and the other route lists none
    assert_eq!(on("pay.get"), 0, "{tokens:?}");
    // without errors: the same schedules as a manifest that never had the key
    let plain = schedules(&system(None));
    let again = schedules(&system(Some(json!({"5XX": "", "default": ""}))));
    assert_eq!(plain, again);
}
