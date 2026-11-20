#![doc = include_str!("../README.md")]
//!
//! ## Notes for hydo's developers
//!
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
//! - `compare.json` (`hydo compare-versions`; added in Milestone 298): one [`CompareDoc`]
//!   document, the schedules run on two versions of the system and what differs between them.
//! - `stats.json` next to `exploration.json` (added in Milestone 336): one [`StatsDoc`]
//!   document, how much an exploration explored, what it cost and which faults it drew.
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
/// `hydo versions`' document (added in Milestone 221): findings replayed on versions of the
/// system.
pub const VERSIONS_DOC: &str = "versions.json";
/// `hydo compare-versions`' document (added in Milestone 298): schedules run on two versions of
/// the system, and what differs between the two runs of each.
pub const COMPARE_DOC: &str = "compare.json";

/// The `compare.json` of a directory (`<dir>/compare.json`). None when there is none or it does
/// not read.
pub fn compare_of(dir: &std::path::Path) -> Option<(std::path::PathBuf, v1::CompareDoc)> {
    let p = dir.join(COMPARE_DOC);
    let text = std::fs::read_to_string(&p).ok()?;
    match reader::document(&text).ok()? {
        reader::Record::Compare(d) => Some((p, *d)),
        _ => None,
    }
}
/// `hydo explore --configs`' document (added in Milestone 299): the explorations of each
/// combination of the manifest's settings.
pub const CONFIGS_DOC: &str = "configs.json";

/// The `configs.json` of a directory, if it has one that reads.
pub fn configs_of(dir: &std::path::Path) -> Option<v1::ConfigsDoc> {
    let text = std::fs::read_to_string(dir.join(CONFIGS_DOC)).ok()?;
    match reader::document(&text).ok()? {
        reader::Record::Configs(d) => Some(*d),
        _ => None,
    }
}

/// An exploration's statistics (added in Milestone 336): how much it explored, what it cost,
/// which faults it drew, and how many first runs fit in a time budget.
pub const STATS_DOC: &str = "stats.json";

/// The `stats.json` of an exploration's directory, if it has one that reads.
pub fn stats_of(dir: &std::path::Path) -> Option<v1::StatsDoc> {
    let text = std::fs::read_to_string(dir.join(STATS_DOC)).ok()?;
    match reader::document(&text).ok()? {
        reader::Record::Stats(d) => Some(*d),
        _ => None,
    }
}

/// The `versions.json` that belongs to a run or exploration directory: `<dir>/versions.json`, or
/// `<dir>/versions/versions.json` (where `hydo versions <dir>` writes it). None when there is
/// none or it does not read.
pub fn versions_of(dir: &std::path::Path) -> Option<(std::path::PathBuf, v1::VersionsDoc)> {
    [
        dir.join(VERSIONS_DOC),
        dir.join("versions").join(VERSIONS_DOC),
    ]
    .into_iter()
    .find_map(|p| {
        let text = std::fs::read_to_string(&p).ok()?;
        match reader::document(&text).ok()? {
            reader::Record::Versions(d) => Some((p, *d)),
            _ => None,
        }
    })
}

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
