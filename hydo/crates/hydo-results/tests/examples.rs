//! The permanent example documents: `hydo/schema/examples/results/v1/`, recorded from real runs
//! and explorations (docs/HISTORY.md, Milestone 115, says which). They are the specification by
//! example and must parse forever:
//!
//! - every `*.json` is a document the reader reads, and every `*.jsonl` a stream it reads whole
//!   (no skipped record, no cut-off line);
//! - every document and record is valid against the committed schema `hydo/schema/results-v1.json`
//!   (checked by the small validator below, which knows the keywords the generator writes);
//! - together they show every record type, every finding kind and the run roles a CI job meets.
//!
//! A new version-1 field or record type gets an example here when hydo starts writing it; an
//! example is never edited (a reader that cannot read it is broken).

use hydo_results::reader::{document, stream, Record};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema")
}

fn files(d: &Path, out: &mut Vec<PathBuf>) {
    let mut es: Vec<_> = std::fs::read_dir(d)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    es.sort();
    for p in es {
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// A JSON Schema validator for what schemars writes: $ref to $defs, type (one or a list), const,
/// enum, properties, required, items, additionalProperties, anyOf, oneOf, minimum. Unknown
/// keywords (format, description, ...) are annotations.
fn valid(schema: &Value, defs: &Value, v: &Value, at: &str, errs: &mut Vec<String>) {
    let Some(s) = schema.as_object() else {
        return;
    };
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        let name = r.trim_start_matches("#/$defs/");
        valid(&defs[name], defs, v, at, errs);
    }
    if let Some(t) = s.get("type") {
        let ts: Vec<&str> = match t {
            Value::String(x) => vec![x.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        let ok = ts.iter().any(|t| match *t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "integer" => v.is_i64() || v.is_u64(),
            "number" => v.is_number(),
            "boolean" => v.is_boolean(),
            "null" => v.is_null(),
            _ => true,
        });
        if !ok {
            errs.push(format!("{at}: {v} is not {ts:?}"));
            return;
        }
    }
    if let Some(c) = s.get("const") {
        if c != v {
            errs.push(format!("{at}: {v} is not {c}"));
        }
    }
    if let Some(Value::Array(e)) = s.get("enum") {
        if !e.contains(v) {
            errs.push(format!("{at}: {v} not one of {e:?}"));
        }
    }
    if let Some(m) = s.get("minimum").and_then(Value::as_f64) {
        if v.as_f64().is_some_and(|x| x < m) {
            errs.push(format!("{at}: {v} < {m}"));
        }
    }
    if let (Some(o), Some(props)) = (
        v.as_object(),
        s.get("properties").and_then(Value::as_object),
    ) {
        for r in s
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let r = r.as_str().unwrap_or_default();
            if !o.contains_key(r) {
                errs.push(format!("{at}: no {r}"));
            }
        }
        for (k, x) in o {
            match props.get(k) {
                Some(ps) => valid(ps, defs, x, &format!("{at}.{k}"), errs),
                None => {
                    if let Some(ap) = s.get("additionalProperties") {
                        if ap == &Value::Bool(false) {
                            errs.push(format!("{at}: field {k} not allowed"));
                        } else if ap.is_object() {
                            valid(ap, defs, x, &format!("{at}.{k}"), errs);
                        }
                    }
                }
            }
        }
    } else if let (Some(o), Some(ap)) = (v.as_object(), s.get("additionalProperties")) {
        if ap.is_object() {
            for (k, x) in o {
                valid(ap, defs, x, &format!("{at}.{k}"), errs);
            }
        }
    }
    if let (Some(a), Some(it)) = (v.as_array(), s.get("items")) {
        for (i, x) in a.iter().enumerate() {
            valid(it, defs, x, &format!("{at}[{i}]"), errs);
        }
    }
    for k in ["anyOf", "oneOf"] {
        if let Some(Value::Array(alts)) = s.get(k) {
            let n = alts
                .iter()
                .filter(|a| {
                    let mut e = Vec::new();
                    valid(a, defs, v, at, &mut e);
                    e.is_empty()
                })
                .count();
            if n == 0 || (k == "oneOf" && n > 1) {
                errs.push(format!("{at}: {n} alternatives of {k} match"));
            }
        }
    }
}

#[derive(Default)]
struct Seen {
    types: BTreeSet<String>,
    kinds: BTreeSet<String>,
    roles: BTreeSet<String>,
    verdicts: BTreeSet<String>,
    recovery: bool,
    shrinks: bool,
    error_kinds: bool,
    properties: bool,
}

fn note_run(s: &mut Seen, r: &hydo_results::Run) {
    s.kinds.extend(r.findings.iter().map(|f| f.kind.clone()));
    s.roles.extend(r.role.clone());
    s.verdicts.insert(r.verdict.kind.clone());
    s.recovery |= r
        .artifacts
        .as_ref()
        .is_some_and(|a| a.consoles.iter().any(|c| c.ends_with(".recovery.log")));
    s.properties |= r.properties.is_some();
}

fn note(s: &mut Seen, r: &Record) {
    match r {
        Record::Run(x) => {
            s.types.insert("run".into());
            note_run(s, x);
        }
        Record::Exploration(x) => {
            s.types.insert("exploration".into());
            s.shrinks |= !x.shrinks.is_empty();
            s.error_kinds |= x.error_kinds.as_ref().is_some_and(|k| !k.kinds.is_empty());
            s.properties |= x.properties.is_some();
            for f in &x.failures {
                note_run(s, &f.run);
            }
        }
        Record::Start(_) => {
            s.types.insert("start".into());
        }
        Record::Shrink(_) => {
            s.types.insert("shrink".into());
        }
        Record::End(_) => {
            s.types.insert("end".into());
        }
        Record::Diff(_) => {
            s.types.insert("diff".into());
        }
    }
}

#[test]
fn every_example_parses_and_is_valid() {
    let schema: Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("results-v1.json")).unwrap())
            .unwrap();
    let defs = schema["$defs"].clone();
    let mut fs = Vec::new();
    files(&root().join("examples/results/v1"), &mut fs);
    let mut seen = Seen::default();
    let mut n = 0;
    for p in &fs {
        let text = std::fs::read_to_string(p).unwrap();
        let name = p.display().to_string();
        let values: Vec<Value> = if name.ends_with(".jsonl") {
            let st = stream(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(st.skipped.is_empty(), "{name}: skipped {:?}", st.skipped);
            assert!(!st.incomplete_tail, "{name}: a cut-off last line");
            assert!(
                st.records.len() >= 2,
                "{name}: a stream of {} records",
                st.records.len()
            );
            for r in &st.records {
                note(&mut seen, r);
            }
            text.lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        } else if name.ends_with(".json") {
            let r = document(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            note(&mut seen, &r);
            vec![serde_json::from_str(&text).unwrap()]
        } else {
            continue;
        };
        for (i, v) in values.iter().enumerate() {
            let mut errs = Vec::new();
            valid(&schema, &defs, v, &format!("{name}:{}", i + 1), &mut errs);
            assert!(
                errs.is_empty(),
                "not valid against results-v1.json:\n{}",
                errs.join("\n")
            );
            n += 1;
        }
    }
    assert!(n > 0, "no examples");
    for t in ["run", "exploration", "start", "shrink", "end", "diff"] {
        assert!(seen.types.contains(t), "no example has a {t} record");
    }
    for k in ["invariant", "assertion", "history-check", "error-log"] {
        assert!(seen.kinds.contains(k), "no example has a {k} finding");
    }
    for r in ["first", "rerun", "shrink", "minimal"] {
        assert!(seen.roles.contains(r), "no example has a {r} run");
    }
    for v in ["ok", "violation"] {
        assert!(
            seen.verdicts.contains(v),
            "no example has a run whose verdict is {v}"
        );
    }
    assert!(
        seen.recovery,
        "no example has a power cut with a recovery boot"
    );
    assert!(seen.shrinks && seen.error_kinds && seen.properties);
}

/// The validator itself rejects what the schema does not allow.
#[test]
fn the_validator_rejects_invalid_records() {
    let schema: Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("results-v1.json")).unwrap())
            .unwrap();
    let defs = schema["$defs"].clone();
    for bad in [
        r#"{"v":1,"type":"run","verdict":{"kind":"ok","text":"x"},"fingerprints":{"fp":"1"}}"#,
        r#"{"v":2,"type":"run","schedule":"s","verdict":{"kind":"ok","text":"x"},"fingerprints":{"fp":"1"}}"#,
        r#"{"v":1,"type":"run","schedule":3,"verdict":{"kind":"ok","text":"x"},"fingerprints":{"fp":"1"}}"#,
        r#"{"v":1,"type":"nothing"}"#,
    ] {
        let mut e = Vec::new();
        valid(
            &schema,
            &defs,
            &serde_json::from_str(bad).unwrap(),
            "x",
            &mut e,
        );
        assert!(!e.is_empty(), "accepted {bad}");
    }
    let good = r#"{"v":1,"type":"run","schedule":"s","verdict":{"kind":"ok","text":"x"},"fingerprints":{"fp":"1"},"later":1}"#;
    let mut e = Vec::new();
    valid(
        &schema,
        &defs,
        &serde_json::from_str(good).unwrap(),
        "x",
        &mut e,
    );
    assert!(e.is_empty(), "{e:?}");
}
