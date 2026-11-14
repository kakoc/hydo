//! /etc/hydo/world.json carries a fake route's `save:` and `load:` and a webhook's `sign:` to
//! hydo-world as written (hydo-world's objects.rs and sign.rs read them); a manifest without them
//! gets the world.json it had before (no new key).

use serde_json::{json, Value};

#[test]
fn save_load_and_sign_reach_world_json() {
    let route = json!({
        "name": "capture", "method": "POST", "path": "/v1/payment_intents/{id}/capture",
        "load": "{path:id}", "save": {"key": "{path:id}", "fields": {"status": "succeeded"}},
        "webhook": {"url": "http://127.0.0.1:9000/hooks",
                    "sign": {"header": "Stripe-Signature", "secret": "whsec_x"}}
    });
    let m = json!({"services": [], "fakes": [{"name": "stripe", "port": 9101, "routes": [route]}]});
    let w: Value = serde_json::from_str(&hydo_build::shell::world_config(&m)).unwrap();
    let r = &w["fakes"][0]["routes"][0];
    assert_eq!(r["load"], "{path:id}");
    assert_eq!(r["save"]["fields"]["status"], "succeeded");
    assert_eq!(r["webhook"]["sign"]["header"], "Stripe-Signature");
    let plain = json!({"services": [], "fakes": [{"name": "stripe", "port": 9101,
        "routes": [{"name": "charge", "path": "/v1/charges", "webhook": {"url": "http://127.0.0.1:9000/h"}}]}]});
    let text = hydo_build::shell::world_config(&plain);
    for k in ["\"save\"", "\"load\"", "\"sign\""] {
        assert!(!text.contains(k), "{k} in {text}");
    }
}

/// Milestone 249: a fake route's `errors:` (the error answers of `hydo fake from-openapi`) reaches
/// world.json as written; a route without it gets no new key.
#[test]
fn errors_reach_world_json() {
    let m = json!({"services": [], "fakes": [{"name": "pay", "port": 9101, "routes": [
        {"name": "create", "method": "POST", "path": "/v1/things",
         "errors": {"409": "{\"code\":\"conflict\"}", "default": ""}},
        {"name": "get", "method": "GET", "path": "/v1/things/{id}"}]}]});
    let w: Value = serde_json::from_str(&hydo_build::shell::world_config(&m)).unwrap();
    assert_eq!(
        w["fakes"][0]["routes"][0]["errors"],
        json!({"409": "{\"code\":\"conflict\"}", "default": ""})
    );
    assert!(w["fakes"][0]["routes"][1].get("errors").is_none());
}
