//! Extractors of fakes and proxies (`ref:`, `idempotency:`): form and query names in the nested
//! encoding that Stripe, Rails and PHP use (`metadata[order_id]`) are accepted, found while
//! onboarding a commerce backend whose payment provider sends `metadata[session_id]`.

use hydo_manifest::{parse_text, validate_data};

fn manifest(extractor: &str) -> String {
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
    idempotency: header:Idempotency-Key
    routes:
      - name: intent
        method: POST
        path: /v1/payment_intents
        ref: {extractor}
workload:
  steps: 2
  run: "true"
invariants:
  - id: ok
    cmd: "true"
"#
    )
}

fn check(extractor: &str) -> Result<(), String> {
    let mut v = parse_text(&manifest(extractor), "hydo.yaml", false).map_err(|e| e.to_string())?;
    validate_data(&mut v).map_err(|e| e.to_string())
}

#[test]
fn bracketed_form_and_query_names_are_extractors() {
    for x in [
        "form:metadata[session_id]",
        "form:items[0][price]",
        "query:filter[status]",
        "form:a.b",
        "form:plain_name",
    ] {
        check(x).unwrap_or_else(|e| panic!("{x}: {e}"));
    }
}

#[test]
fn other_extractors_stay_refused() {
    for x in [
        "x",
        "form:",
        "form:a b",
        "header:X[1]",
        "path:a[b]",
        "form:a{b}",
    ] {
        let e = check(x).expect_err(x);
        assert!(e.contains("is not an extractor"), "{x}: {e}");
    }
}
