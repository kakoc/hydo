//! `hydo fake from-openapi` on two real public OpenAPI descriptions (Milestone 249): each becomes
//! the fake recorded in tests/golden/ (HYDO_BLESS=1 records it again), through the command itself
//! (`-o FILE`), and the written fake is one hydo's validator and project model accept.
//! - tests/fixtures/openapi/petstore-expanded.yaml: the OpenAPI Initiative's expanded Petstore
//!   (OpenAPI 3.0, Apache-2.0);
//! - tests/fixtures/openapi/TransferService-v4.yaml: Adyen's Transfers API v4 (OpenAPI 3.1, MIT),
//!   a payments API with an idempotency header, discriminators and error answers.
//!
//! tests/fixtures/openapi/SOURCE names where each was fetched; the test checks their sha256.

use crate::tmpdir::TmpDir;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn fixture(name: &str, sha256: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/openapi")
        .join(name);
    let b = std::fs::read(&p).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&b)),
        sha256,
        "{} is not the published file (tests/fixtures/openapi/SOURCE)",
        p.display()
    );
    p
}

fn golden(name: &str, text: &str) {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1") {
        std::fs::write(&p, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&p).unwrap_or_default();
    assert!(
        want == text,
        "{} differs from the fake; HYDO_BLESS=1 cargo test -p hydo-compose --test it openapi:: records it again\n{text}",
        p.display()
    );
}

/// `hydo fake from-openapi <spec> <args> -o <tmp>`; the file it wrote.
fn run(tag: &str, spec: &Path, args: &[&str]) -> String {
    let dir = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-openapi-{tag}-{}", std::process::id())),
    );
    let out = dir.join("fake.yaml");
    let mut a: Vec<String> = vec!["from-openapi".into(), spec.to_string_lossy().into_owned()];
    a.extend(args.iter().map(|s| s.to_string()));
    a.push("-o".into());
    a.push(out.to_string_lossy().into_owned());
    assert_eq!(hydo_compose::openapi::main(&a), 0, "{a:?}");
    let text = std::fs::read_to_string(&out).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    text
}

#[test]
fn the_petstore() {
    let spec = fixture(
        "petstore-expanded.yaml",
        "b1633b6309c065c43d56be7c659b0f2c4be03be5a4013b7c3f74b32bd33f62eb",
    );
    let text = run("pet", &spec, &["--stateful", "--name", "petstore"]);
    golden("openapi-petstore.yaml", &text);
    // addPet answers the new pet with the request's own name, and findPetById reads it back
    assert!(
        text.contains(r#"body: '{"name":"{json:name}","tag":"string","id":{seq}}'"#),
        "{text}"
    );
    assert!(text.contains("load: '{path:id}'"), "{text}");
}

#[test]
fn adyen_transfers() {
    let spec = fixture(
        "TransferService-v4.yaml",
        "ce85c66b08bcd5c9b9bcad924e1245374ce2fe1379d87680ed2329d0e2876a7f",
    );
    let text = run(
        "adyen",
        &spec,
        &["--stateful", "--name", "transfers", "--port", "9102"],
    );
    golden("openapi-adyen-transfers.yaml", &text);
    assert!(
        text.contains("idempotency: 'header:Idempotency-Key'"),
        "{text}"
    );
    assert!(text.contains("idempotent: true"), "{text}");
    assert!(text.contains("errors:"), "{text}");
}

#[test]
fn the_same_fake_from_the_spec_as_json() {
    // the YAML loader and serde_json read the description to the same data
    let spec = fixture(
        "petstore-expanded.yaml",
        "b1633b6309c065c43d56be7c659b0f2c4be03be5a4013b7c3f74b32bd33f62eb",
    );
    let v = hydo_compose::openapi::load_spec(&std::fs::read_to_string(&spec).unwrap()).unwrap();
    let dir = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-openapi-spec-{}", std::process::id())),
    );
    let j = dir.join("petstore-expanded.json");
    std::fs::write(&j, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let from_json = run("json", &j, &["--stateful", "--name", "petstore"]);
    let _ = std::fs::remove_dir_all(&dir);
    let from_yaml = run("yaml", &spec, &["--stateful", "--name", "petstore"]);
    // only the comment naming the file and its sha256 differs
    let body = |t: &str| {
        t.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(body(&from_json), body(&from_yaml));
}

#[test]
fn the_example_holds_the_fake_the_command_writes() {
    // hydo/onboard/examples/openapi-payouts/hydo.yaml has the fake that the command in its
    // header wrote, unedited: the same command gives the same fake
    let ex = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../onboard/examples/openapi-payouts");
    let m = std::fs::read_to_string(ex.join("hydo.yaml")).unwrap();
    let v = hydo_manifest::yaml::parse_yaml(&m, "hydo.yaml").unwrap();
    let spec = std::fs::read(ex.join("api/payouts.yaml")).unwrap();
    let o = hydo_compose::openapi::Opts {
        name: Some("payouts".into()),
        port: Some(9001),
        stateful: true,
        reference: Some("json:order".into()),
        command: "hydo fake from-openapi payouts.yaml".into(),
        ..Default::default()
    };
    let g = hydo_compose::openapi::generate_bytes(&spec, &o).unwrap();
    assert!(g.refused.is_empty(), "{:?}", g.refused);
    assert_eq!(v["fakes"][0], g.fake);
}
