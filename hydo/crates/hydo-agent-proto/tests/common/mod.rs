//! Helpers of the schema and example tests: a small validator of the keywords the generated
//! schema uses, and sample instances made from a definition.
#![allow(dead_code)]

use hydo_agent_proto::json::{Obj, Value};
use std::path::{Path, PathBuf};

pub fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema")
}

fn resolve<'a>(root: &'a Value, r: &str) -> &'a Value {
    let name = r.strip_prefix("#/$defs/").expect("a local reference");
    let Value::Obj(o) = root else { panic!() };
    let Some(Value::Obj(d)) = o.get("$defs") else {
        panic!()
    };
    d.get(name)
        .unwrap_or_else(|| panic!("no definition {name}"))
}

/// Whether `v` is valid by schema `s` (the root `root` for references): `type`, `const`,
/// `enum`, `minimum`, `maximum`, `properties`, `required`, `additionalProperties: false`,
/// `items`, `oneOf` (exactly one) and `$ref`.
pub fn validate_at(root: &Value, s: &Value, v: &Value) -> Result<(), String> {
    let Value::Obj(s) = s else { return Ok(()) };
    if let Some(Value::Str(r)) = s.get("$ref") {
        return validate_at(root, resolve(root, r), v);
    }
    if let Some(Value::Arr(alts)) = s.get("oneOf") {
        let ok: Vec<_> = alts
            .iter()
            .filter(|a| validate_at(root, a, v).is_ok())
            .collect();
        return match ok.len() {
            1 => Ok(()),
            0 => Err("matches no alternative".into()),
            k => Err(format!("matches {k} alternatives")),
        };
    }
    if let Some(Value::Str(t)) = s.get("type") {
        let fits = matches!(
            (t.as_str(), v),
            ("object", Value::Obj(_))
                | ("array", Value::Arr(_))
                | ("string", Value::Str(_))
                | ("integer", Value::Int(_))
                | ("boolean", Value::Bool(_))
                | ("null", Value::Null)
        );
        if !fits {
            return Err(format!("{} where {t} was expected", v.kind()));
        }
    }
    if let Some(c) = s.get("const") {
        if c != v {
            return Err(format!(
                "{} where {} was expected",
                v.to_canonical(),
                c.to_canonical()
            ));
        }
    }
    if let Some(Value::Arr(e)) = s.get("enum") {
        if !e.contains(v) {
            return Err(format!("{} is not in the enum", v.to_canonical()));
        }
    }
    if let (Some(Value::Int(lo)), Value::Int(n)) = (s.get("minimum"), v) {
        if n < lo {
            return Err(format!("{n} < {lo}"));
        }
    }
    if let (Some(Value::Int(hi)), Value::Int(n)) = (s.get("maximum"), v) {
        if n > hi {
            return Err(format!("{n} > {hi}"));
        }
    }
    if let Value::Obj(m) = v {
        let empty = Obj::new();
        let props = match s.get("properties") {
            Some(Value::Obj(p)) => p,
            _ => &empty,
        };
        if let Some(Value::Arr(req)) = s.get("required") {
            for r in req {
                let Value::Str(r) = r else { continue };
                if !m.contains_key(r) {
                    return Err(format!("{r} is missing"));
                }
            }
        }
        for (k, x) in m {
            match props.get(k) {
                Some(ps) => validate_at(root, ps, x).map_err(|e| format!("{k}: {e}"))?,
                None if s.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{k} is not allowed"))
                }
                None => {}
            }
        }
    }
    if let (Some(items), Value::Arr(a)) = (s.get("items"), v) {
        for (i, x) in a.iter().enumerate() {
            validate_at(root, items, x).map_err(|e| format!("[{i}]: {e}"))?;
        }
    }
    Ok(())
}

pub fn validate(root: &Value, v: &Value) -> Result<(), String> {
    validate_at(root, root, v)
}

/// An instance of a property schema (a fixed value of its kind).
fn sample_value(s: &Value, key: &str) -> Value {
    let Value::Obj(s) = s else { panic!() };
    if let Some(c) = s.get("const") {
        return c.clone();
    }
    if let Some(Value::Arr(e)) = s.get("enum") {
        return e.last().cloned().expect("an enum value");
    }
    match s.get("type") {
        Some(Value::Str(t)) => match t.as_str() {
            "string" => Value::Str(format!("{key}-\u{e9}\n\"x\"")),
            "integer" => match s.get("maximum") {
                Some(Value::Int(m)) => Value::Int(*m),
                _ => Value::Int(1234567890123),
            },
            "boolean" => Value::Bool(true),
            "array" => Value::Arr(vec![sample_value(s.get("items").expect("items"), key); 2]),
            k => panic!("no sample for {k}"),
        },
        _ => panic!("no type in {key}"),
    }
}

/// An instance of a definition: every property (`full`) or the required ones only.
pub fn sample(def: &Value, full: bool, _name: &str) -> Value {
    let Value::Obj(d) = def else { panic!() };
    let Some(Value::Obj(props)) = d.get("properties") else {
        panic!()
    };
    let req: Vec<String> = match d.get("required") {
        Some(Value::Arr(r)) => r
            .iter()
            .filter_map(|x| {
                if let Value::Str(s) = x {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .collect(),
        _ => vec![],
    };
    let mut m = Obj::new();
    for (k, ps) in props {
        if full || req.contains(k) {
            m.insert(k.clone(), sample_value(ps, k));
        }
    }
    Value::Obj(m)
}
