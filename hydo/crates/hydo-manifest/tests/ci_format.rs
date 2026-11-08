//! `hydo/ci.yaml`, format 1 (`hydo_manifest::ci`; docs/design/ci.md section 8.2).
//!
//! - `hydo/schema/ci-v1.json` must equal the schema generated from the types (`HYDO_BLESS=1`
//!   rewrites it), and it may only grow from `hydo/schema/released/ci-v1.json`, the schema of the
//!   last release (`hydo_schema_compat::check` with [`Readers::Current`]: only hydo reads this
//!   file, and an older hydo refuses a key it does not know, so a key or value may widen). The
//!   format is new after 0.2.0, so the released copy is the first schema; a release copies the
//!   committed one there (`HYDO_RELEASE_SCHEMA=1`).
//! - every example document under `hydo/schema/examples/ci/v1/` reads, and the refusals below
//!   give their messages.

use hydo_manifest::ci::{self, FailOn};
use hydo_schema_compat::{check, Readers, Rules};
use std::path::{Path, PathBuf};

fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema")
}

#[test]
fn the_committed_schema_is_the_generated_one() {
    let p = schema_dir().join("ci-v1.json");
    if std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1") {
        std::fs::write(&p, ci::schema_text()).unwrap();
    }
    let want = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e} (write it with HYDO_BLESS=1)", p.display()));
    assert!(
        want == ci::schema_text(),
        "{} differs from the schema generated from hydo_manifest::ci; review the change and \
         rewrite it with HYDO_BLESS=1 cargo test -p hydo-manifest --test it ci_format::",
        p.display()
    );
}

#[test]
fn the_schema_only_grows_from_the_released_one() {
    let p = schema_dir().join("released/ci-v1.json");
    if std::env::var_os("HYDO_RELEASE_SCHEMA").is_some_and(|v| v == "1") {
        std::fs::write(&p, ci::schema_text()).unwrap();
    }
    let old: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let b: Vec<String> = check(&old, &ci::schema(), &Rules::new(Readers::Current))
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(
        b.is_empty(),
        "hydo/ci.yaml format 1 changed in ways that are not additions:\n  {}",
        b.join("\n  ")
    );
}

#[test]
fn every_example_reads() {
    let d = schema_dir().join("examples/ci/v1");
    let mut n = 0;
    for e in std::fs::read_dir(&d).unwrap().flatten() {
        let p = e.path();
        let text = std::fs::read_to_string(&p).unwrap();
        ci::parse(&text, &p.display().to_string())
            .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        n += 1;
    }
    assert!(n >= 2, "examples in {}", d.display());
    let c = ci::parse(
        &std::fs::read_to_string(d.join("design.yaml")).unwrap(),
        "design.yaml",
    )
    .unwrap();
    assert_eq!(c.engine.as_deref(), Some("kvm"));
    assert_eq!(c.budget("pr").seeds.as_deref(), Some("1-200"));
    assert_eq!(c.budget("push").seeds.as_deref(), Some("1-200"));
    assert_eq!(c.budget("nightly").seeds.as_deref(), Some("1-2000"));
    assert_eq!(c.inputs["payments"], ["services/payments/", "go.sum"]);
    let e = ci::parse("format: 1\n", "e").unwrap();
    assert_eq!(e.fail_on(), FailOn::default_policy());
    assert_eq!(e.budget("nightly"), ci::Budget::default());
}

#[test]
fn what_is_refused_says_why() {
    let msg = |t: &str| ci::parse(t, "hydo/ci.yaml").unwrap_err().message;
    assert_eq!(
        msg("format: 2\nshards: 4\n"),
        "hydo/ci.yaml: format 2: this hydo reads hydo/ci.yaml format 1; a newer hydo is needed"
    );
    assert!(
        msg("pr:\n  seed: 1-20\n").contains("pr.seed"),
        "{}",
        msg("pr:\n  seed: 1-20\n")
    );
    assert!(msg("budget: 10m\n").contains("budget"));
    assert_eq!(
        msg("engine: qemu\n"),
        "hydo/ci.yaml: engine: \"qemu\" is not one of kvm, hvx, hvm"
    );
    assert_eq!(
        msg("pr:\n  seeds: 20-1\n"),
        "hydo/ci.yaml: pr.seeds: \"20-1\" is not a range like 1-200"
    );
    assert!(msg("fail_on: [sometimes]\n").contains("fail_on"));
    assert_eq!(ci::parse_seeds("7"), Some((7, 7)));
    assert_eq!(FailOn::parse("pre-existing"), Some(FailOn::PreExisting));
}
