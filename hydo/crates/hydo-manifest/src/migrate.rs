//! The canonical spelling of a format-1 document, `read_project` (a file and its `hydo/` folder
//! read into the model), and `hydo migrate` (an old file rewritten to the canonical spelling).
//!
//! Retired spellings (format 1 still reads them, `hydo migrate` rewrites them):
//! - a history check in `test.checks` (`kind: linearizable`, `list_append` or `list-append`):
//!   its home is the top-level `checks:` (`id`, `type: linearizable|list-append`, `history`,
//!   `final` = the read-back). The expansion has always moved it there; the rewrite writes
//!   exactly the entry the expansion made.
//!
//! A document without `format:` gets `format: 1`. A rewrite is checked before it is written: the
//! new text must expand and validate to exactly the same data and plan as the old one.

use serde_json::{Map, Value};
use std::path::Path;

use crate::model::{self, Project, FORMAT};
use crate::py::{self, str_of};
use crate::{composer, validate, ManifestError};

fn err(msg: impl Into<String>) -> ManifestError {
    ManifestError {
        message: msg.into(),
    }
}

const HISTORY_KINDS: &[&str] = &["linearizable", "list_append", "list-append"];

/// The retired spellings in a document, one line each (empty: the document is canonical).
pub fn retired(doc: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(cs) = doc
        .get("test")
        .and_then(|t| t.get("checks"))
        .and_then(Value::as_array)
    {
        for (i, c) in cs.iter().enumerate() {
            if let Some(k) = c.get("kind").and_then(Value::as_str) {
                if HISTORY_KINDS.contains(&k) {
                    out.push(format!(
                        "test.checks[{i}] ({}, kind {k}): a history check; its home is the top-level checks:",
                        str_of(c.get("name").unwrap_or(&Value::Null))
                    ));
                }
            }
        }
    }
    out
}

/// The document in the canonical spelling: retired spellings replaced (see the module comment).
/// Everything else is kept as it is, key order included.
pub fn canonical_value(doc: &Value) -> Result<Value, ManifestError> {
    let mut d = doc.clone();
    let Some(o) = d.as_object_mut() else {
        return Ok(d);
    };
    let mut moved: Vec<Value> = Vec::new();
    let mut test_empty = false;
    if let Some(Value::Object(t)) = o.get_mut("test") {
        if let Some(Value::Array(cs)) = t.get("checks").cloned() {
            let mut keep = Vec::new();
            for (i, c) in cs.iter().enumerate() {
                let hist = c
                    .get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|k| HISTORY_KINDS.contains(&k));
                match c {
                    Value::Object(co) if hist => {
                        moved.push(composer::history_check(co, &format!("test.checks[{i}]"))?)
                    }
                    other => keep.push(other.clone()),
                }
            }
            if !moved.is_empty() {
                if keep.is_empty() {
                    t.shift_remove("checks");
                } else {
                    t.insert("checks".into(), Value::Array(keep));
                }
                test_empty = t.is_empty();
            }
        }
    }
    if moved.is_empty() {
        return Ok(d);
    }
    if test_empty {
        o.shift_remove("test");
    }
    // the expansion appends them after the top-level checks, sorted by id
    moved.sort_by_key(|a| str_of(&a["id"]));
    let mut all = match o.get("checks") {
        Some(Value::Array(a)) => a.clone(),
        None | Some(Value::Null) => Vec::new(),
        Some(_) => return Err(err("checks: must be a list")),
    };
    all.extend(moved);
    o.insert("checks".into(), Value::Array(all));
    Ok(d)
}

fn read_text(path: &Path) -> Result<String, ManifestError> {
    let b = std::fs::read(path).map_err(|e| err(format!("{}: {e}", path.display())))?;
    let t = String::from_utf8(b).map_err(|_| err(format!("{}: not UTF-8", path.display())))?;
    Ok(t.replace("\r\n", "\n").replace('\r', "\n"))
}

fn parse_doc(text: &str, name: &str) -> Result<Value, ManifestError> {
    let doc = crate::parse_text(text, name, true)?;
    if !doc.is_object() {
        return Err(err("the manifest must be a mapping"));
    }
    model::check_format(&doc, None)?;
    Ok(doc)
}

/// A project file read into the model: the document (format 1), the `hydo/` folder next to it
/// merged in (`workload/` -> test.commands, `checks/` -> test.checks or, for linearizable_* and
/// list_append_* files, checks:, `regressions/` -> test.regressions), in the canonical spelling.
/// Only the format is checked here, not the value rules (`load` checks those).
pub fn read_project(path: &Path) -> Result<Project, ManifestError> {
    let p = path.to_string_lossy().into_owned();
    let doc = parse_doc(&read_text(path)?, &py::basename(&p))?;
    model::from_value(&doc)?;
    let mut doc = doc;
    if let Some(folder) = composer::scan_folder(&py::dirname(&py::abspath(&p)))? {
        if let Some(o) = doc.as_object_mut() {
            composer::merge_folder(o, &folder)?;
        }
    }
    let mut project = model::from_value(&canonical_value(&doc)?)?;
    // a document without format: is format 1
    project.format = Some(FORMAT);
    Ok(project)
}

/// What the build sees of a document: the validated data and the plan meta, with the defaults of
/// `params:` and the folder (when given). The `test:` section as written and `format:` are left
/// out of the data: the build reads the plan from the meta, and format 1 is the only format.
fn meaning(doc: &Value, folder: Option<&Value>) -> Result<(Value, Option<Value>), ManifestError> {
    let mut m = doc.clone();
    validate::apply_params(&mut m, &[])?;
    let mut w = Vec::new();
    let meta = composer::expand(&mut m, folder, &mut w)?;
    validate::validate(&mut m)?;
    if let Some(o) = m.as_object_mut() {
        o.shift_remove("test");
        o.shift_remove("format");
    }
    Ok((m, meta))
}

/// The result of `migrate`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Migration {
    /// The new text (the old text when nothing changed).
    pub text: String,
    pub changed: bool,
    /// What was rewritten, one line each.
    pub notes: Vec<String>,
}

/// `hydo migrate`: the file's text in the canonical spelling of format 1, with `format: 1`.
/// `folder_dir` is the directory of the file (its `hydo/` folder takes part in the check that the
/// rewrite means the same).
pub fn migrate_text(
    text: &str,
    name: &str,
    folder_dir: Option<&Path>,
) -> Result<Migration, ManifestError> {
    let doc = parse_doc(text, name)?;
    model::from_value(&doc)?;
    let mut notes = retired(&doc);
    let canon = canonical_value(&doc)?;
    let has_format = doc.get("format").is_some();
    if notes.is_empty() && has_format {
        return Ok(Migration {
            text: text.to_string(),
            changed: false,
            notes,
        });
    }
    let mut with_format = Map::new();
    with_format.insert("format".into(), Value::from(FORMAT));
    for (k, v) in canon.as_object().into_iter().flatten() {
        if k != "format" {
            with_format.insert(k.clone(), v.clone());
        }
    }
    let target = Value::Object(with_format);
    let json = name.ends_with(".json") || py::lstrip(text).starts_with('{');
    let new_text = if json {
        serde_json::to_string_pretty(&target).unwrap_or_default() + "\n"
    } else if notes.is_empty() {
        insert_format_line(text).unwrap_or_else(|| emit_yaml(&target))
    } else {
        notes.push(
            "the file was written anew from its data: comments and layout are not kept".into(),
        );
        emit_yaml(&target)
    };
    if !has_format {
        notes.insert(0, format!("format: {FORMAT} added"));
    }
    // the rewrite reads back to the target and means exactly the same
    let back = crate::parse_text(&new_text, name, true)?;
    if back != target {
        return Err(err(
            "migrate: the rewritten text does not read back to the same document (not written; please report this)",
        ));
    }
    let folder = match folder_dir {
        Some(d) => composer::scan_folder(&d.to_string_lossy())?,
        None => None,
    };
    let before = meaning(&doc, folder.as_ref());
    let after = meaning(&back, folder.as_ref());
    match (&before, &after) {
        (Ok(a), Ok(b)) if a == b => {}
        (Err(a), Err(b)) if a == b => {}
        (Err(a), _) => {
            return Err(err(format!(
                "migrate: the file is not valid now ({}); fix it first",
                a.message
            )))
        }
        _ => {
            return Err(err(
                "migrate: the rewritten file would mean something else (not written; please report this)",
            ))
        }
    }
    Ok(Migration {
        text: new_text,
        changed: true,
        notes,
    })
}

/// `format: 1` as a new first key of a block-style YAML document, after its leading comments.
fn insert_format_line(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut done = false;
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        if !done && !t.is_empty() && !t.starts_with('#') {
            if t.starts_with('{') || t.starts_with('-') || t.starts_with('[') {
                return None;
            }
            let indent = &line[..line.len() - t.len()];
            out.push_str(&format!("{indent}format: {FORMAT}\n"));
            done = true;
        }
        out.push_str(line);
    }
    if !done {
        out.push_str(&format!("format: {FORMAT}\n"));
    }
    Some(out)
}

// ------------------------------------------------------------------ the YAML writer

fn plain_key(k: &str) -> bool {
    let ok = !k.is_empty()
        && k.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    ok && !is_keyword(k)
}

fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "true" | "True" | "yes" | "false" | "False" | "no" | "null" | "~"
    )
}

fn key_text(k: &str) -> String {
    if plain_key(k) {
        k.to_string()
    } else {
        serde_json::to_string(k).unwrap_or_default()
    }
}

/// A one-line string the YAML subset reads back as itself without quotes.
fn plain_scalar(s: &str) -> bool {
    !s.is_empty()
        && !is_keyword(s)
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '$'))
        && !s.ends_with(' ')
        && !s.ends_with(':')
        && !s.contains(": ")
        && !s.contains("  ")
        && !(s.chars().all(|c| c.is_ascii_digit() || c == '-'))
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '_' | '.' | '/' | '=' | '+' | '@' | '$' | '-' | ' ' | ',' | ':'
                )
        })
}

fn scalar_text(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) if plain_scalar(s) => s.clone(),
        Value::String(s) => serde_json::to_string(s).unwrap_or_default(),
        Value::Array(a) if a.is_empty() => "[]".into(),
        Value::Object(o) if o.is_empty() => "{}".into(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// A multi-line string as a `|` or `|-` block when the subset reads it back exactly.
fn block_ok(s: &str) -> Option<&'static str> {
    if !s.contains('\n') || s.contains('\r') {
        return None;
    }
    let body = s.strip_suffix('\n');
    let (lines, chomp) = match body {
        Some(b) if !b.ends_with('\n') => (b, "|"),
        Some(_) => return None,
        None => (s, "|-"),
    };
    let first = lines.split('\n').next().unwrap_or("");
    if first.is_empty() || first.starts_with(' ') || first.starts_with('\t') {
        return None;
    }
    if lines.split('\n').any(|l| {
        (!l.is_empty() && l.trim().is_empty())
            || l.chars()
                .take_while(|c| c.is_whitespace())
                .any(|c| c == '\t')
    }) {
        return None;
    }
    if lines.ends_with('\n') {
        return None;
    }
    Some(chomp)
}

fn is_scalar(v: &Value) -> bool {
    match v {
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => true,
    }
}

/// `key: value` lines of a mapping at `indent`; the first line's own prefix is `first`.
fn emit_map(o: &Map<String, Value>, indent: usize, first: Option<&str>, out: &mut String) {
    let pad = " ".repeat(indent);
    for (i, (k, v)) in o.iter().enumerate() {
        let lead = match (i, first) {
            (0, Some(f)) => f.to_string(),
            _ => pad.clone(),
        };
        let kt = key_text(k);
        match v {
            Value::String(s) if block_ok(s).is_some() => {
                out.push_str(&format!("{lead}{kt}: {}\n", block_ok(s).unwrap_or("|")));
                let bp = " ".repeat(indent + 2);
                let body = s.strip_suffix('\n').unwrap_or(s);
                for l in body.split('\n') {
                    if l.is_empty() {
                        out.push('\n');
                    } else {
                        out.push_str(&format!("{bp}{l}\n"));
                    }
                }
            }
            v if is_scalar(v) => out.push_str(&format!("{lead}{kt}: {}\n", scalar_text(v))),
            Value::Object(m) => {
                out.push_str(&format!("{lead}{kt}:\n"));
                emit_map(m, indent + 2, None, out);
            }
            Value::Array(a) => match flow_list(a) {
                Some(f) if lead.len() + kt.len() + f.len() < 100 => {
                    out.push_str(&format!("{lead}{kt}: {f}\n"))
                }
                _ => {
                    out.push_str(&format!("{lead}{kt}:\n"));
                    emit_seq(a, indent + 2, out);
                }
            },
            _ => {}
        }
    }
}

/// A list of one-line scalars as `[a, b]` (the subset's flow lists keep quotes and escapes).
fn flow_list(a: &[Value]) -> Option<String> {
    let mut items = Vec::new();
    for v in a {
        match v {
            Value::Array(_) | Value::Object(_) => return None,
            Value::String(s) if s.contains('\n') => return None,
            Value::String(s) if s.contains(',') => {
                items.push(serde_json::to_string(s).unwrap_or_default())
            }
            other => items.push(scalar_text(other)),
        }
    }
    Some(format!("[{}]", items.join(", ")))
}

fn emit_seq(a: &[Value], indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    for v in a {
        match v {
            Value::Object(m) if !m.is_empty() => {
                // the first key of an item: a plain or quoted key without ':'
                let first_key_ok = m.keys().next().is_some_and(|k| !k.contains(':'));
                if first_key_ok {
                    emit_map(m, indent + 2, Some(&format!("{pad}- ")), out);
                } else {
                    out.push_str(&format!("{pad}- {}\n", scalar_text(v)));
                }
            }
            Value::Array(x) if !x.is_empty() => {
                out.push_str(&format!("{pad}-\n"));
                emit_seq(x, indent + 2, out);
            }
            other => out.push_str(&format!("{pad}- {}\n", scalar_text(other))),
        }
    }
}

/// A document as block-style YAML that hydo's YAML subset reads back to the same data.
pub fn emit_yaml(doc: &Value) -> String {
    let mut out = String::new();
    match doc {
        Value::Object(o) => emit_map(o, 0, None, &mut out),
        other => out.push_str(&scalar_text(other)),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn back(v: &Value) -> Value {
        crate::yaml::parse_yaml(&emit_yaml(v), "t.yaml").unwrap()
    }

    #[test]
    fn yaml_round_trip() {
        for v in [
            json!({"a": 1, "b": "x y", "c": [1, "q", {"r": "s", "t": [1, 2]}], "d": {}, "e": []}),
            json!({"run": "line 1\n  line 2\n\nline 4\n", "x": "no newline\nat end", "y": "two\n\n"}),
            json!({"k": [{"a": "| not a block", "b": {"c": "yes"}}, [1, [2]], null, true, "-1", "0755"]}),
            json!({"/etc/x.conf": "port 1\n", "a b": "#x", "q": "it's \"q\"", "u": "\u{e9}\t"}),
            json!({"list": [{"m": "a\nb\n", "n": 1}], "s": " lead", "t": "trail ", "e": ""}),
        ] {
            assert_eq!(back(&v), v, "{}", emit_yaml(&v));
        }
    }

    #[test]
    fn history_checks_move_to_checks() {
        let doc = json!({
            "name": "x",
            "checks": [{"id": "a", "type": "linearizable", "history": "h"}],
            "test": {"checks": [
                {"name": "z", "kind": "list-append", "history": "l", "consistency": "serializable"},
                {"name": "ok", "kind": "final", "run": ["/bin/ok"]},
                {"name": "b", "kind": "linearizable", "history": "kv", "run": ["/r", "x"]},
            ]},
        });
        let c = canonical_value(&doc).unwrap();
        assert_eq!(
            c["checks"],
            json!([
                {"id": "a", "type": "linearizable", "history": "h"},
                {"id": "b", "type": "linearizable", "history": "kv", "msg": "b", "final": "exec '/r' 'x'"},
                {"id": "z", "type": "list-append", "history": "l", "msg": "z", "consistency": "serializable"},
            ])
        );
        assert_eq!(
            c["test"],
            json!({"checks": [{"name": "ok", "kind": "final", "run": ["/bin/ok"]}]})
        );
        assert_eq!(retired(&doc).len(), 2);
        assert!(retired(&c).is_empty());
        let only = json!({"name": "x", "test": {"checks": [{"name": "b", "kind": "list_append", "history": "kv"}]}});
        let c = canonical_value(&only).unwrap();
        assert!(c.get("test").is_none());
    }

    #[test]
    fn format_line() {
        assert_eq!(
            insert_format_line("# c\n\nname: x\n").unwrap(),
            "# c\n\nformat: 1\nname: x\n"
        );
        assert!(insert_format_line("{\"name\": 1}\n").is_none());
    }
}
