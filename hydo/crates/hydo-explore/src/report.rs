//! What the explorer reads back from a run (its HYDO-ASSERT records, its failed properties) and
//! the verdict helpers the reports use. The property table and the liveness summary are in
//! [`crate::results`].

use crate::pool::Res;
use crate::py::{float_repr, py_str};
use regex::Regex;
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::OnceLock;

fn assert_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"HYDO-ASSERT (\{.*\})").expect("pattern"))
}

/// The HYDO-ASSERT records of a text, one per line that has one (lines that do not parse are skipped).
pub fn records(text: &str) -> Vec<Map<String, Value>> {
    text.split('\n')
        .filter_map(|l| assert_re().captures(l))
        .filter_map(|c| serde_json::from_str::<Value>(&c[1]).ok())
        .filter_map(|v| v.as_object().cloned())
        .collect()
}

/// One explorer run's consoles and its host-checked records as one text.
pub fn merged(outdir: &Path, tag: &str, r: &Res) -> String {
    let texts: Vec<String> = crate::results::explorer_consoles(outdir, tag)
        .into_iter()
        .map(|c| c.1)
        .collect();
    crate::results::merged_text(&texts, r)
}

/// explore.py `assert_failures`: failed property evaluations in one run (its consoles and the
/// host's history-check records), sorted ids.
pub fn assert_failures(outdir: &Path, tag: &str, r: &Res) -> Vec<Value> {
    crate::results::assertion_findings(&merged(outdir, tag, r))
        .into_iter()
        .map(|f| Value::String(f.id))
        .collect()
}

/// explore.py `verdict_kind`.
pub fn verdict_kind(v: &str) -> &'static str {
    hydo_engine::protocol::verdict_kind(v)
}

/// explore.py `signature`: the verdict without its prefix, every number as '#'.
pub fn signature(v: &str) -> String {
    static D: OnceLock<Regex> = OnceLock::new();
    let v = v.replace("HYDO-ORACLE: ", "");
    D.get_or_init(|| Regex::new(r"\d+").expect("p"))
        .replace_all(&v, "#")
        .into_owned()
}

/// The grouping key of a failure: the signature up to " order=" for violations.
pub fn sig_key(v: &str) -> String {
    let s = signature(v);
    if verdict_kind(v) == "violation" {
        s.split(" order=").next().unwrap_or("").to_string()
    } else {
        s
    }
}

/// explore.py `same_failure`.
pub fn same_failure(v_ref: &str, v: &str) -> bool {
    verdict_kind(v) == "violation" && sig_key(v) == sig_key(v_ref)
}

/// A value as `"%s" % v` prints it in a report table.
pub fn cell(v: &Value) -> String {
    match v {
        Value::Number(n) if n.is_f64() => float_repr(n.as_f64().unwrap_or(0.0)),
        other => py_str(other),
    }
}
