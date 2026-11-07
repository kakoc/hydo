//! hydo's results format: what a run and an exploration report, in one versioned,
//! machine-readable model.
//!
//! Two documents and one stream, all made of the types in [`v1`]:
//!
//! - `result.json` in a run directory (`hydo run`, `hydo repro`): one [`Run`] document.
//! - `exploration.json` in an exploration directory (`hydo explore`): one [`Exploration`]
//!   document (configuration, summary, failures and their reproductions, shrinks, error kinds,
//!   property table, liveness).
//! - `events.jsonl` next to it: one record per line, written while the exploration runs: a
//!   [`Start`], a [`Run`] for every run the explorer made (first runs as they finish, in seed
//!   order; re-runs, shrink candidates and confirmations after them), a [`Shrink`] per minimal
//!   schedule, and an [`End`].
//! - `diff.json` (`hydo diff`, `hydo explain`; added in M165): one [`Diff`] document, where two
//!   runs first differ. `diff.txt` next to it is its view for people.
//!
//! Every record and document is a JSON object with `"v"` (the format version, 1) and `"type"`.
//! Within a version only additions are made: new optional fields and new record types. A field
//! is never renamed, removed or given another meaning; that needs a new version, which readers
//! read alongside this one. Readers ignore fields and record types they do not know
//! ([`reader`]). The JSON Schema `hydo/schema/results-v1.json` is generated from these types
//! ([`schema`]) and checked against the last released one (`tests/schema.rs`, with the
//! `hydo-schema-compat` check); the documents under
//! `hydo/schema/examples/results/v1/` were recorded from real runs and must parse forever.
//!
//! The format is experimental until hydo 1.0, and it follows the additions-only rule from
//! version 1 on.

pub mod reader;
pub mod v1;

pub use reader::{read_dir, ReadError, Results, Skipped, Stream};
pub use v1::*;

/// The format version this crate writes.
pub const VERSION: u64 = 1;
/// A run directory's document.
pub const RUN_DOC: &str = "result.json";
/// An exploration directory's document.
pub const EXPLORATION_DOC: &str = "exploration.json";
/// An exploration directory's event stream.
pub const EVENTS: &str = "events.jsonl";
/// A diff's document (added in M165).
pub const DIFF_DOC: &str = "diff.json";

/// One stream record as a line: compact JSON and a newline.
pub fn to_line<T: serde::Serialize>(rec: &T) -> String {
    let mut s = serde_json::to_string(rec).expect("a results record serializes");
    s.push('\n');
    s
}

/// One document as written to its file: indented JSON and a newline.
pub fn to_document<T: serde::Serialize>(doc: &T) -> String {
    let mut s = serde_json::to_string_pretty(doc).expect("a results document serializes");
    s.push('\n');
    s
}

/// Write a document to its file whole (a temporary file renamed over it), so a reader never sees
/// half of one.
pub fn write_document<T: serde::Serialize>(path: &std::path::Path, doc: &T) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, to_document(doc))?;
    std::fs::rename(&tmp, path)
}

/// The JSON Schema of every version-1 document and record, generated from the types (the
/// committed copy is `hydo/schema/results-v1.json`; `tests/schema.rs` keeps them equal).
pub fn schema() -> serde_json::Value {
    let s = schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<v1::AnyV1>();
    let mut v = serde_json::to_value(s).expect("a schema serializes");
    if let Some(o) = v.as_object_mut() {
        o.insert(
            "title".into(),
            serde_json::Value::String("hydo results, version 1".into()),
        );
    }
    v
}
