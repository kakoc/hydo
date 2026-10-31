//! The additions-only rule of a format version, as a check of two JSON Schemas (M114): the schema
//! of the last released format version (`hydo/schema/released/project-v1.json`) and the schema
//! the model generates now. A change passes only when every document the old schema accepts is
//! still accepted with the same meaning:
//!
//! - every old property is still there, with a compatible schema; a new property is optional;
//! - `required` and `additionalProperties` stay exactly as they were;
//! - a `type` may gain kinds, an `enum` may gain values, an `anyOf`/`oneOf` may gain variants at
//!   its end (each old variant stays compatible at its place);
//! - `default`, `const`, `pattern`, bounds and every other keyword stay exactly as they were, and
//!   no new constraint keyword appears;
//! - only the notes (`description`, `title`, `$comment`, `examples`, `deprecated`) may change.
//!
//! `$ref`s are followed into `$defs` by name; definitions nothing refers to are not compared.

use serde_json::{Map, Value};
use std::collections::BTreeSet;

const NOTES: &[&str] = &["description", "title", "$comment", "examples", "deprecated"];

struct Cx<'a> {
    old_defs: &'a Map<String, Value>,
    new_defs: &'a Map<String, Value>,
    seen: BTreeSet<(String, String)>,
    out: Vec<String>,
}

fn defs(root: &Value) -> Map<String, Value> {
    root.get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn ref_name(r: &str) -> Option<&str> {
    r.strip_prefix("#/$defs/")
}

fn types(v: Option<&Value>) -> BTreeSet<String> {
    match v {
        Some(Value::String(s)) => [s.clone()].into(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        _ => BTreeSet::new(),
    }
}

impl Cx<'_> {
    fn bad(&mut self, at: &str, what: String) {
        self.out.push(format!("{at}: {what}"));
    }

    fn cmp(&mut self, old: &Value, new: &Value, at: &str) {
        let (Value::Object(o), Value::Object(n)) = (old, new) else {
            if old != new {
                self.bad(at, format!("was {old}, is {new}"));
            }
            return;
        };
        if let Some(r) = o.get("$ref").and_then(Value::as_str) {
            let Some(nr) = n.get("$ref").and_then(Value::as_str) else {
                self.bad(at, format!("was a reference to {r}, is not one now"));
                return;
            };
            let (Some(on), Some(nn)) = (ref_name(r), ref_name(nr)) else {
                self.bad(at, format!("an unknown reference {r} or {nr}"));
                return;
            };
            if self.seen.insert((on.to_string(), nn.to_string())) {
                match (
                    self.old_defs.get(on).cloned(),
                    self.new_defs.get(nn).cloned(),
                ) {
                    (Some(a), Some(b)) => self.cmp(&a, &b, &format!("$defs/{on}")),
                    (Some(_), None) => self.bad(at, format!("the definition {nn} is gone")),
                    _ => {}
                }
            }
        }
        for (k, ov) in o {
            if NOTES.contains(&k.as_str()) || k == "$ref" {
                continue;
            }
            let nv = n.get(k);
            let here = format!("{at}/{k}");
            match k.as_str() {
                "type" => {
                    let (a, b) = (types(Some(ov)), types(nv));
                    if !a.is_subset(&b) {
                        self.bad(&here, format!("the kinds {a:?} became {b:?}"));
                    }
                }
                "enum" => {
                    let b = nv.and_then(Value::as_array).cloned().unwrap_or_default();
                    for x in ov.as_array().into_iter().flatten() {
                        if !b.contains(x) {
                            self.bad(&here, format!("the value {x} is gone"));
                        }
                    }
                }
                "properties" => {
                    let empty = Map::new();
                    let np = nv.and_then(Value::as_object).unwrap_or(&empty);
                    for (p, ps) in ov.as_object().into_iter().flatten() {
                        match np.get(p) {
                            Some(x) => self.cmp(ps, x, &format!("{here}/{p}")),
                            None => self.bad(&here, format!("the key {p} is gone")),
                        }
                    }
                }
                "required" => {
                    let a = types(Some(ov));
                    let b = types(nv);
                    if a != b {
                        self.bad(&here, format!("the required keys {a:?} became {b:?}"));
                    }
                }
                "items" | "additionalProperties" | "not" => match nv {
                    Some(x) => self.cmp(ov, x, &here),
                    None => self.bad(&here, "is gone".into()),
                },
                "anyOf" | "oneOf" | "allOf" | "prefixItems" => {
                    let a = ov.as_array().cloned().unwrap_or_default();
                    let b = nv.and_then(Value::as_array).cloned().unwrap_or_default();
                    if k == "allOf" && a.len() != b.len() {
                        self.bad(&here, "the number of parts changed".into());
                    }
                    if b.len() < a.len() {
                        self.bad(&here, format!("had {} variants, has {}", a.len(), b.len()));
                    }
                    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                        self.cmp(x, y, &format!("{here}/{i}"));
                    }
                }
                "$defs" => {}
                _ => {
                    if nv != Some(ov) {
                        self.bad(
                            &here,
                            format!(
                                "was {ov}, is {}",
                                nv.map(Value::to_string).unwrap_or_else(|| "gone".into())
                            ),
                        );
                    }
                }
            }
        }
        // a keyword the old schema did not have narrows what it accepts, unless it is a note,
        // a default written down for the first time, or the properties of a new key
        let props_new: BTreeSet<&String> = n
            .get("properties")
            .and_then(Value::as_object)
            .map(|p| p.keys().collect())
            .unwrap_or_default();
        let req_new = types(n.get("required"));
        for (k, _) in n {
            if o.contains_key(k) || NOTES.contains(&k.as_str()) || k == "$defs" {
                continue;
            }
            match k.as_str() {
                "default" => {}
                "required" if req_new.is_empty() => {}
                _ => self.bad(&format!("{at}/{k}"), "a new constraint".into()),
            }
        }
        if o.get("properties").is_some() {
            let old_props: BTreeSet<&String> = o
                .get("properties")
                .and_then(Value::as_object)
                .map(|p| p.keys().collect())
                .unwrap_or_default();
            for p in props_new.difference(&old_props) {
                if req_new.contains(*p) {
                    self.bad(
                        &format!("{at}/properties/{p}"),
                        "a new key that is required".into(),
                    );
                }
            }
        }
    }
}

/// The ways `new` breaks the additions-only rule against `old` (empty: compatible).
pub fn problems(old: &Value, new: &Value) -> Vec<String> {
    let (od, nd) = (defs(old), defs(new));
    let mut cx = Cx {
        old_defs: &od,
        new_defs: &nd,
        seen: BTreeSet::new(),
        out: Vec::new(),
    };
    cx.cmp(old, new, "#");
    cx.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["name"],
            "properties": {
                "name": {"type": "string"},
                "kind": {"$ref": "#/$defs/Kind", "default": "a"},
                "n": {"type": ["integer", "null"], "minimum": 0},
                "run": {"anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]},
            },
            "$defs": {"Kind": {"type": "string", "enum": ["a", "b"]}},
        })
    }

    fn with(f: impl FnOnce(&mut Value)) -> Vec<String> {
        let mut n = base();
        f(&mut n);
        problems(&base(), &n)
    }

    #[test]
    fn additions_pass() {
        assert!(problems(&base(), &base()).is_empty());
        assert!(with(|n| n["properties"]["new"] = json!({"type": "boolean"})).is_empty());
        assert!(with(|n| n["$defs"]["Kind"]["enum"] = json!(["a", "b", "c"])).is_empty());
        assert!(
            with(|n| n["properties"]["name"]["type"] = json!(["string", "integer"])).is_empty()
        );
        assert!(with(|n| n["properties"]["run"]["anyOf"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type": "integer"})))
        .is_empty());
        assert!(
            with(|n| n["properties"]["name"]["description"] = json!("now documented")).is_empty()
        );
        assert!(with(|n| n["properties"]["name"]["deprecated"] = json!(true)).is_empty());
        assert!(with(|n| n["$defs"]["Unused"] = json!({"type": "string"})).is_empty());
    }

    #[test]
    fn breaking_changes_fail() {
        // a key removed, renamed, made required, or a new required key
        assert!(!with(|n| {
            n["properties"].as_object_mut().unwrap().shift_remove("n");
        })
        .is_empty());
        assert!(!with(|n| {
            let p = n["properties"].as_object_mut().unwrap();
            let x = p.shift_remove("n").unwrap();
            p.insert("count".into(), x);
        })
        .is_empty());
        assert!(!with(|n| n["required"] = json!(["name", "n"])).is_empty());
        assert!(!with(|n| {
            n["properties"]["new"] = json!({"type": "boolean"});
            n["required"] = json!(["name", "new"]);
        })
        .is_empty());
        // another kind of value, a value gone, another default, a tighter bound
        assert!(!with(|n| n["properties"]["name"]["type"] = json!("integer")).is_empty());
        assert!(!with(|n| n["$defs"]["Kind"]["enum"] = json!(["a"])).is_empty());
        assert!(!with(|n| n["properties"]["kind"]["default"] = json!("b")).is_empty());
        assert!(!with(|n| n["properties"]["n"]["minimum"] = json!(1)).is_empty());
        assert!(!with(|n| n["properties"]["name"]["pattern"] = json!("^a")).is_empty());
        // unknown keys allowed, a variant gone, a reference replaced, a definition changed
        assert!(!with(|n| n["additionalProperties"] = json!(true)).is_empty());
        assert!(!with(|n| {
            n["properties"]["run"]["anyOf"]
                .as_array_mut()
                .unwrap()
                .pop();
        })
        .is_empty());
        assert!(!with(|n| n["properties"]["kind"] = json!({"type": "string"})).is_empty());
        assert!(!with(|n| n["$defs"]["Kind"]["type"] = json!("integer")).is_empty());
    }
}
