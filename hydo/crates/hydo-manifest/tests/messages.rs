//! `checks[].type: messages` (docs/design/messaging.md "The declaration"): the keys and values a
//! declaration takes, the combinations refused before any run, `include:` bundles resolved at
//! load time, and the resolved declaration in `hydo build --check`'s summary.

use hydo_manifest::{check_summary, load_until_images, LoadOptions};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hydo-manifest-messages-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn manifest(check: &str) -> String {
    format!(
        "format: 1\nname: mq\nservices:\n  - name: q\n    cmd: /bin/q\n    ready: \"true\"\nworkload:\n  steps: 2\n  run: \"true\"\nchecks:\n{check}"
    )
}

fn load(d: &Path, text: &str) -> Result<Value, String> {
    let p = d.join("hydo.yaml");
    std::fs::write(&p, text).unwrap();
    let opts = LoadOptions {
        version_check_warn: true,
        ..LoadOptions::default()
    };
    match load_until_images(&p, &opts).0 {
        Ok(l) => {
            hydo_manifest::check_document(&l).map_err(|e| e.message)?;
            Ok(l.manifest.data["checks"][0].clone())
        }
        Err(e) => Err(e.message),
    }
}

#[test]
fn a_declaration_reads_and_its_mistakes_are_refused() {
    let d = dir("refused");
    let ok = "  - id: jobs\n    type: messages\n    history: jobs\n    final: /bin/c drain\n    claims:\n      delivery: at-least-once\n      lease: {ms: 30000}\n";
    load(&d, &manifest(ok)).unwrap();
    for (check, want) in [
        // an unknown claim, an unknown value
        ("  - id: j\n    type: messages\n    history: h\n    claims:\n      leese: none\n", "unknown field leese"),
        ("  - id: j\n    type: messages\n    history: h\n    claims:\n      order: sorted\n", "claims.order: one of none, fifo"),
        ("  - id: j\n    type: messages\n    history: h\n    acknowledgement: maybe\n", "acknowledgement: one of explicit"),
        ("  - id: j\n    type: messages\n    history: h\n    model: register\n", "model: is for type: linearizable"),
        // a linearizable check does not take a declaration
        ("  - id: j\n    type: linearizable\n    history: h\n    claims: {}\n", "unknown field(s) claims"),
        // the combinations refused before any run
        ("  - id: j\n    type: messages\n    history: h\n    claims:\n      linearizable: fifo\n", "claims.linearizable needs acknowledgement: on-receive"),
        ("  - id: j\n    type: messages\n    history: h\n    acknowledgement: on-receive\n    claims:\n      after_ack: no-redelivery\n", "claims.after_ack is about acknowledgements"),
        ("  - id: j\n    type: messages\n    history: h\n    final: d\n    claims:\n      delivery: exactly-once\n      lease: {ms: 5}\n", "with claims.lease"),
        ("  - id: j\n    type: messages\n    history: h\n    claims:\n      delivery: at-least-once\n", "claims.delivery reads the end of the history, and the check has no final:"),
        ("  - id: j\n    type: messages\n    history: h\n    include: [nothere.yaml]\n", "no such bundle file"),
    ] {
        let e = load(&d, &manifest(check)).unwrap_err();
        assert!(e.contains(want), "{check}\n-> {e}");
    }
}

#[test]
fn includes_merge_in_order_and_the_entry_wins() {
    let d = dir("include");
    std::fs::create_dir_all(d.join("b")).unwrap();
    std::fs::write(
        d.join("b/one.yaml"),
        "retention_ms: 1000\nclaims:\n  delivery: at-least-once\n  lease: {ms: 30000}\nmeasures:\n  quantiles: [0.5, 1]\n",
    )
    .unwrap();
    std::fs::write(
        d.join("b/two.yaml"),
        "claims:\n  after_ack: no-redelivery\n  lease: {ms: 45000}\n",
    )
    .unwrap();
    let c = load(
        &d,
        &manifest("  - id: jobs\n    type: messages\n    history: jobs\n    final: /bin/c drain\n    include: [b/one.yaml, b/two.yaml]\n    claims:\n      processing: once\n"),
    )
    .unwrap();
    assert_eq!(c.get("include"), None);
    assert_eq!(c["retention_ms"], json!(1000));
    assert_eq!(
        c["claims"],
        json!({"delivery": "at-least-once", "lease": {"ms": 45000}, "after_ack": "no-redelivery", "processing": "once"})
    );
    // the summary of hydo build --check names the resolved declaration
    let p = d.join("hydo.yaml");
    let l = load_until_images(&p, &LoadOptions::default()).0.unwrap();
    let s = check_summary("hydo.yaml", &l);
    let line = s
        .iter()
        .find(|x| x.contains("check jobs (messages"))
        .expect("a summary line");
    assert!(
        line.contains("\"lease\":{\"ms\":45000}") && line.contains("\"quantiles\":[0.5,1"),
        "{line}"
    );
    // a bundle with a key a declaration does not have
    std::fs::write(d.join("b/bad.yaml"), "id: other\n").unwrap();
    let e = load(
        &d,
        &manifest(
            "  - id: jobs\n    type: messages\n    history: jobs\n    include: [b/bad.yaml]\n",
        ),
    )
    .unwrap_err();
    assert!(e.contains("a bundle has the keys of a declaration"), "{e}");
    // the merged declaration must be valid on its own
    std::fs::write(d.join("b/lin.yaml"), "claims:\n  linearizable: fifo\n").unwrap();
    let e = load(
        &d,
        &manifest(
            "  - id: jobs\n    type: messages\n    history: jobs\n    include: [b/lin.yaml]\n",
        ),
    )
    .unwrap_err();
    assert!(e.contains("needs acknowledgement: on-receive"), "{e}");
}

#[test]
fn the_manifest_an_image_carries_is_read_without_its_bundles() {
    // an image's /etc/hydo/hydo.yaml keeps include: (the bundles are not in the image; its
    // compiled manifest has the resolved declaration)
    let text = manifest("  - id: jobs\n    type: messages\n    history: jobs\n    final: d\n    include: [bundles/x.yaml]\n");
    hydo_manifest::from_embedded(&text, None).unwrap();
}
