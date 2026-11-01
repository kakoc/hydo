//! Is a schema an additive change of another? [`breaking`] lists every change from `old` to `new`
//! that a version-1 reader written against `old` could not survive: a removed or renamed field,
//! record type or definition, a changed JSON type, constant or format, a field that became
//! required or stopped being required, a narrowed set of values. What it allows: new optional
//! fields, new definitions, new record types (alternatives of the root), new values of an enum,
//! and any change of a description.
//!
//! `tests/schema.rs` runs it on the committed schema against the last released one; the unit
//! tests below are its specification.

use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Keys that carry no meaning for a reader.
const IGNORED: &[&str] = &[
    "description",
    "title",
    "examples",
    "default",
    "$comment",
    "$schema",
];

/// Every non-additive change from `old` to `new`, one line each (empty: `new` is compatible).
pub fn breaking(old: &Value, new: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let empty = Map::new();
    let od = old
        .get("$defs")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let nd = new
        .get("$defs")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    for (name, a) in od {
        match nd.get(name) {
            None => out.push(format!("definition {name}: removed")),
            Some(b) => cmp(name, a, b, false, &mut out),
        }
    }
    let strip = |v: &Value| {
        let mut m = v.as_object().cloned().unwrap_or_default();
        m.remove("$defs");
        Value::Object(m)
    };
    cmp("(root)", &strip(old), &strip(new), true, &mut out);
    out
}

fn types(v: &Value) -> Option<BTreeSet<String>> {
    match v.get("type")? {
        Value::String(s) => Some([s.clone()].into()),
        Value::Array(a) => Some(
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect(),
        ),
        _ => None,
    }
}

fn required(v: &Value) -> BTreeSet<String> {
    v.get("required")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `a` and `b` are compatible, without reporting.
fn fits(path: &str, a: &Value, b: &Value) -> bool {
    let mut o = Vec::new();
    cmp(path, a, b, false, &mut o);
    o.is_empty()
}

fn cmp(path: &str, a: &Value, b: &Value, root: bool, out: &mut Vec<String>) {
    let (Some(ao), Some(bo)) = (a.as_object(), b.as_object()) else {
        if a != b {
            out.push(format!("{path}: changed from {a} to {b}"));
        }
        return;
    };
    if let Some(r) = ao.get("$ref") {
        if bo.get("$ref") != Some(r) {
            out.push(format!(
                "{path}: was {r}, now {}",
                bo.get("$ref")
                    .map(Value::to_string)
                    .unwrap_or("not a reference".into())
            ));
        }
    }
    if let Some(ta) = types(a) {
        match types(b) {
            Some(tb) if tb == ta => {}
            tb => out.push(format!(
                "{path}: type {:?} became {:?}",
                ta,
                tb.unwrap_or_default()
            )),
        }
    } else if types(b).is_some() && ao.get("$ref").is_none() && !ao.contains_key("anyOf") {
        out.push(format!("{path}: became restricted to a type"));
    }
    for k in ["const", "format", "minimum", "maximum", "pattern"] {
        if ao.get(k) != bo.get(k) {
            out.push(format!(
                "{path}: {k} {} became {}",
                ao.get(k).map(Value::to_string).unwrap_or("(none)".into()),
                bo.get(k).map(Value::to_string).unwrap_or("(none)".into())
            ));
        }
    }
    if let Some(Value::Array(ea)) = ao.get("enum") {
        let eb = bo.get("enum").and_then(Value::as_array);
        for x in ea {
            if !eb.is_some_and(|eb| eb.contains(x)) {
                out.push(format!("{path}: value {x} no longer allowed"));
            }
        }
    } else if bo.contains_key("enum") {
        out.push(format!("{path}: became restricted to an enum"));
    }
    let empty = Map::new();
    let pa = ao
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let pb = bo
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    for (k, sa) in pa {
        match pb.get(k) {
            None => out.push(format!("{path}.{k}: removed")),
            Some(sb) => cmp(&format!("{path}.{k}"), sa, sb, false, out),
        }
    }
    let (ra, rb) = (required(a), required(b));
    for k in rb.difference(&ra) {
        out.push(format!("{path}.{k}: became required"));
    }
    for k in ra.difference(&rb) {
        out.push(format!("{path}.{k}: no longer required"));
    }
    if let Some(ia) = ao.get("items") {
        match bo.get("items") {
            Some(ib) => cmp(&format!("{path}[]"), ia, ib, false, out),
            None => out.push(format!("{path}[]: item schema removed")),
        }
    }
    match (
        ao.get("additionalProperties"),
        bo.get("additionalProperties"),
    ) {
        (_, None) | (Some(Value::Bool(false)), _) => {}
        (None | Some(Value::Bool(true)), Some(Value::Bool(true))) => {}
        (Some(x), Some(y)) if x.is_object() && y.is_object() => {
            cmp(&format!("{path}{{*}}"), x, y, false, out)
        }
        (_, Some(_)) => out.push(format!("{path}: other fields became restricted")),
    }
    for k in ["anyOf", "oneOf"] {
        let alts_b = bo.get(k).and_then(Value::as_array);
        if let Some(Value::Array(alts_a)) = ao.get(k) {
            let Some(alts_b) = alts_b else {
                out.push(format!("{path}: {k} removed"));
                continue;
            };
            for (i, x) in alts_a.iter().enumerate() {
                let p = format!("{path}.{k}[{i}]");
                if !alts_b.iter().any(|y| fits(&p, x, y)) {
                    // report the change against the alternative at the same place, if any
                    match alts_b.get(i) {
                        Some(y) => cmp(&p, x, y, false, out),
                        None => out.push(format!("{p}: alternative removed")),
                    }
                }
            }
            if !root && alts_b.len() > alts_a.len() {
                out.push(format!(
                    "{path}: {k} gained alternatives (a field's values widened)"
                ));
            }
        } else if alts_b.is_some() && !root {
            out.push(format!("{path}: became a choice ({k})"));
        }
    }
    for (k, va) in ao {
        let known = [
            "$ref",
            "type",
            "const",
            "format",
            "minimum",
            "maximum",
            "pattern",
            "enum",
            "properties",
            "required",
            "items",
            "additionalProperties",
            "anyOf",
            "oneOf",
        ];
        if known.contains(&k.as_str()) || IGNORED.contains(&k.as_str()) {
            continue;
        }
        if bo.get(k) != Some(va) {
            out.push(format!("{path}: {k} changed"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::breaking;
    use serde_json::{json, Value};

    fn base() -> Value {
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "t",
            "anyOf": [{"$ref": "#/$defs/Run"}],
            "$defs": {
                "Run": {
                    "type": "object",
                    "properties": {
                        "v": {"type": "integer", "const": 1},
                        "type": {"$ref": "#/$defs/RunType"},
                        "schedule": {"type": "string"},
                        "secs": {"type": ["number", "null"], "format": "double"},
                        "findings": {"type": "array", "items": {"$ref": "#/$defs/Finding"}}
                    },
                    "required": ["v", "type", "schedule"]
                },
                "RunType": {"type": "string", "enum": ["run"]},
                "Finding": {
                    "type": "object",
                    "properties": {"kind": {"type": "string"}, "id": {"type": "string"}},
                    "required": ["kind", "id"]
                }
            }
        })
    }

    fn with(f: impl FnOnce(&mut Value)) -> Value {
        let mut v = base();
        f(&mut v);
        v
    }

    #[test]
    fn the_same_schema_is_compatible() {
        assert!(breaking(&base(), &base()).is_empty());
    }

    #[test]
    fn additions_are_compatible() {
        let new = with(|v| {
            v["$defs"]["Run"]["properties"]["point"] = json!({"type": ["string", "null"]});
            v["$defs"]["Run"]["description"] = json!("a run, described anew");
            v["$defs"]["Shrink"] = json!({"type": "object", "properties": {}});
            v["anyOf"]
                .as_array_mut()
                .unwrap()
                .push(json!({"$ref": "#/$defs/Shrink"}));
            v["$defs"]["RunType"]["enum"]
                .as_array_mut()
                .unwrap()
                .push(json!("run2"));
        });
        assert_eq!(breaking(&base(), &new), Vec::<String>::new());
    }

    #[test]
    fn a_removed_field_breaks() {
        let new = with(|v| {
            v["$defs"]["Run"]["properties"]
                .as_object_mut()
                .unwrap()
                .remove("secs");
        });
        assert_eq!(breaking(&base(), &new), ["Run.secs: removed"]);
    }

    #[test]
    fn a_renamed_field_breaks() {
        let new = with(|v| {
            let p = v["$defs"]["Run"]["properties"].as_object_mut().unwrap();
            let s = p.remove("secs").unwrap();
            p.insert("seconds".into(), s);
        });
        assert_eq!(breaking(&base(), &new), ["Run.secs: removed"]);
    }

    #[test]
    fn a_changed_type_breaks() {
        let new = with(|v| v["$defs"]["Run"]["properties"]["schedule"] = json!({"type": "array"}));
        assert_eq!(breaking(&base(), &new).len(), 1);
        let new = with(|v| v["$defs"]["Run"]["properties"]["secs"]["format"] = json!("uint64"));
        assert_eq!(
            breaking(&base(), &new),
            ["Run.secs: format \"double\" became \"uint64\""]
        );
    }

    #[test]
    fn a_new_required_field_breaks() {
        let new = with(|v| {
            v["$defs"]["Run"]["properties"]["point"] = json!({"type": "string"});
            v["$defs"]["Run"]["required"]
                .as_array_mut()
                .unwrap()
                .push(json!("point"));
        });
        assert_eq!(breaking(&base(), &new), ["Run.point: became required"]);
    }

    #[test]
    fn a_field_that_stops_being_required_breaks() {
        let new = with(|v| v["$defs"]["Run"]["required"] = json!(["v", "type"]));
        assert_eq!(
            breaking(&base(), &new),
            ["Run.schedule: no longer required"]
        );
    }

    #[test]
    fn a_removed_record_type_or_definition_breaks() {
        let new = with(|v| v["anyOf"] = json!([]));
        assert_eq!(
            breaking(&base(), &new),
            ["(root).anyOf[0]: alternative removed"]
        );
        let new = with(|v| {
            v["$defs"].as_object_mut().unwrap().remove("Finding");
        });
        assert_eq!(breaking(&base(), &new), ["definition Finding: removed"]);
    }

    #[test]
    fn a_narrowed_value_set_breaks() {
        let new = with(|v| v["$defs"]["RunType"]["enum"] = json!(["other"]));
        assert_eq!(
            breaking(&base(), &new),
            ["RunType: value \"run\" no longer allowed"]
        );
        let new = with(|v| v["$defs"]["Run"]["properties"]["v"]["const"] = json!(2));
        assert_eq!(breaking(&base(), &new), ["Run.v: const 1 became 2"]);
    }

    #[test]
    fn a_field_whose_items_change_breaks() {
        let new = with(|v| {
            v["$defs"]["Run"]["properties"]["findings"]["items"] = json!({"type": "string"})
        });
        assert!(!breaking(&base(), &new).is_empty());
    }

    #[test]
    fn closing_an_object_breaks() {
        let new = with(|v| v["$defs"]["Finding"]["additionalProperties"] = json!(false));
        assert_eq!(
            breaking(&base(), &new),
            ["Finding: other fields became restricted"]
        );
    }
}
