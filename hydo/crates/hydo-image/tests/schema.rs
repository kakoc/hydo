//! The schema is generated from the types and committed, and it only grows.
//!
//! - `hydo/schema/image-v1.json` must equal [`hydo_results::schema`]: a change of the types is
//!   a change of the committed schema, reviewed in its diff. `HYDO_BLESS=1` rewrites it.
//! - `hydo/schema/released/image-v1.json` is the schema of the last release (the first one is
//!   the schema of M155, when the format was introduced); the committed schema must be an
//!   additive change of it (`hydo_schema_compat::check` with [`Readers::Released`]: readers built
//!   for the released schema read what is written now, ignoring unknown fields and record types
//!   and nothing else, so a known field may not widen either). A release copies the committed
//!   schema there (`HYDO_RELEASE_SCHEMA=1` does it). A breaking change needs a new version
//!   (`image-v2.json`, read alongside version 1), never an edit of version 1.

use hydo_schema_compat::{check, Readers, Rules};
use std::path::{Path, PathBuf};

fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema")
}

fn current() -> String {
    let mut s = serde_json::to_string_pretty(&hydo_image::schema()).unwrap();
    s.push('\n');
    s
}

#[test]
fn the_committed_schema_is_the_generated_one() {
    let p = schema_dir().join("image-v1.json");
    if std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1") {
        std::fs::write(&p, current()).unwrap();
    }
    let want = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e} (write it with HYDO_BLESS=1)", p.display()));
    assert!(
        want == current(),
        "{} differs from the schema generated from the types; review the change and rewrite it \
         with HYDO_BLESS=1 cargo test -p hydo-image --test schema",
        p.display()
    );
}

#[test]
fn the_schema_only_grows_from_the_released_one() {
    let p = schema_dir().join("released/image-v1.json");
    if std::env::var_os("HYDO_RELEASE_SCHEMA").is_some_and(|v| v == "1") {
        std::fs::write(&p, current()).unwrap();
    }
    let old: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
    )
    .unwrap();
    let new = hydo_image::schema();
    let b: Vec<String> = check(&old, &new, &Rules::new(Readers::Released))
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(
        b.is_empty(),
        "image records version 1 changed in ways a reader of the released schema cannot survive:\n  {}\n\
         Make the change additive (a new optional field), or start version 2.",
        b.join("\n  ")
    );
}
