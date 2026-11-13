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

/// Stage 2: a budget per kind of run, in wall time.
#[test]
fn budgets_are_durations() {
    let d = schema_dir().join("examples/ci/v1/stage2.yaml");
    let c = ci::parse(&std::fs::read_to_string(&d).unwrap(), "stage2.yaml").unwrap();
    assert_eq!(c.budget("pr").budget.as_deref(), Some("10m"));
    assert_eq!(c.budget("push").budget.as_deref(), Some("10m"));
    let p = ci::parse_duration;
    assert_eq!(p("10m"), Some(600.0));
    assert_eq!(p("1h30m"), Some(5400.0));
    assert_eq!(p("2m30s"), Some(150.0));
    assert_eq!(p("45"), Some(45.0));
    assert_eq!(p("0"), None);
    assert_eq!(p("10x"), None);
    assert_eq!(p("m"), None);
    assert_eq!(p("10m5"), None);
    let e = ci::parse("pr:\n  budget: soon\n", "hydo/ci.yaml")
        .unwrap_err()
        .message;
    assert_eq!(
        e,
        "hydo/ci.yaml: pr.budget: \"soon\" is not a duration like 10m, 90s or 1h30m"
    );
}

/// `hydo/known-findings.yaml`, format 1: its committed schema is the generated one, and it only
/// grows from the released one (new after 0.3.1, so the released copy is the first schema).
#[test]
fn the_known_findings_schema_is_committed_and_only_grows() {
    use hydo_manifest::known_findings as kf;
    let p = schema_dir().join("known-findings-v1.json");
    if std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1") {
        std::fs::write(&p, kf::schema_text()).unwrap();
    }
    let want = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e} (write it with HYDO_BLESS=1)", p.display()));
    assert!(
        want == kf::schema_text(),
        "{} differs from the generated schema; rewrite it with HYDO_BLESS=1",
        p.display()
    );
    let r = schema_dir().join("released/known-findings-v1.json");
    if std::env::var_os("HYDO_RELEASE_SCHEMA").is_some_and(|v| v == "1") {
        std::fs::write(&r, kf::schema_text()).unwrap();
    }
    let old: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&r).unwrap()).unwrap();
    let b: Vec<String> = check(&old, &kf::schema(), &Rules::new(Readers::Current))
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(b.is_empty(), "not additions:\n  {}", b.join("\n  "));
    let ex = schema_dir().join("examples/known-findings/v1/redis.yaml");
    let k = kf::parse(&std::fs::read_to_string(&ex).unwrap(), "redis.yaml").unwrap();
    assert_eq!(k.accepted.len(), 1);
    assert_eq!(k.accepted[0].who.as_deref(), Some(""));
}
