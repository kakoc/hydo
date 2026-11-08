//! A fake that remembers objects (`save:`, `load:` on a route) and signs its webhooks
//! (`webhook.sign`): what the manifest accepts and what it refuses, with the message that says
//! how. Found onboarding a commerce backend (hydo/onboard/examples/medusa): a payment provider's
//! later read of an object must return what it was created with, and its webhooks carry an HMAC
//! signature the system checks.

use hydo_manifest::model::Project;
use hydo_manifest::{parse_text, validate_data};

fn manifest(route_extra: &str, hook_extra: &str) -> String {
    format!(
        r#"format: 1
name: shop
services:
  - name: api
    cmd: /bin/api
    ready: "true"
fakes:
  - name: stripe
    port: 9101
    routes:
      - name: intent_create
        method: POST
        path: /v1/payment_intents
        body: '{{"id":"pi_{{seq}}","currency":"{{obj:currency}}"}}'
        webhook:
          url: http://127.0.0.1:9000/hooks
          body: '{{"id":"pi_{{seq}}"}}'
{hook_extra}
{route_extra}
workload:
  steps: 2
  run: "true"
invariants:
  - id: ok
    cmd: "true"
"#
    )
}

fn check(route_extra: &str, hook_extra: &str) -> Result<(), String> {
    let text = manifest(route_extra, hook_extra);
    let mut v = parse_text(&text, "hydo.yaml", false).map_err(|e| e.to_string())?;
    validate_data(&mut v).map_err(|e| e.to_string())?;
    // the typed model reads the same document
    serde_yaml_ng_or_json(&text)
}

/// The model of the project format reads what the validator accepted.
fn serde_yaml_ng_or_json(text: &str) -> Result<(), String> {
    let v = parse_text(text, "hydo.yaml", false).map_err(|e| e.to_string())?;
    serde_json::from_value::<Project>(v)
        .map(|_| ())
        .map_err(|e| format!("model: {e}"))
}

const SAVE: &str =
    "        save: {key: 'pi_{seq}', fields: {currency: '{form:currency}', amount: 0}}";
const SIGN: &str = "          sign: {header: Stripe-Signature, secret: whsec_x}";

#[test]
fn objects_and_signatures_are_accepted() {
    check(SAVE, "").unwrap();
    check(SAVE, SIGN).unwrap();
    check(
        "        load: '{path:id}'\n        save: {key: '{path:id}', fields: {status: succeeded}}",
        "          sign: {header: webhook-signature, secret: k, payload: '{seq}.{ts}.{body}', value: 'v1,{hmac}', encoding: base64}",
    )
    .unwrap();
}

#[test]
fn mistakes_are_refused_with_what_to_write() {
    for (route, hook, want) in [
        ("        save: pi_1", "", "save: must be a mapping"),
        ("        save: {fields: {a: b}}", "", "save: key:"),
        (
            "        save: {key: k, other: 1}",
            "",
            "unknown field(s) other",
        ),
        (
            "        save: {key: k, fields: [a]}",
            "",
            "save.fields: a mapping",
        ),
        (
            "        save: {key: k, fields: {a: [1]}}",
            "",
            "save.fields.a: a string",
        ),
        ("        load: ''", "", "load: the key"),
        ("", "          sign: {secret: s}", "sign: header:"),
        ("", "          sign: {header: X-Sig}", "sign: secret:"),
        (
            "",
            "          sign: {header: X-Sig, secret: s, value: 'v1={sig}'}",
            "must contain {hmac}",
        ),
        (
            "",
            "          sign: {header: X-Sig, secret: s, encoding: b32}",
            "encoding: hex or base64",
        ),
        (
            "",
            "          sign: {header: X-Sig, secret: s, algo: sha1}",
            "unknown field(s) algo",
        ),
    ] {
        let e = check(route, hook).expect_err(want);
        assert!(e.contains(want), "{want}: {e}");
    }
}
