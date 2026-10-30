//! Python's view of a manifest value, for the few operations hydo-build applies to values that
//! validation leaves loose: `x.get(k)` with a default, `int(x)`, `"%d" % x`, `str(x)` and
//! `" ".join(xs)`. Where Python stops with an exception the result is a [`Fail::Crash`] with a
//! plain message.

use crate::Fail;
use hydo_manifest::py;
use serde_json::Value;

/// `d.get(k)` of a mapping (`(d or {}).get(k)` when `d` is missing or null).
pub fn get<'a>(d: Option<&'a Value>, k: &str) -> Result<Option<&'a Value>, Fail> {
    match d {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(o)) => Ok(o.get(k)),
        Some(v) if !py::truthy(v) => Ok(None),
        Some(v) => Err(Fail::Crash(format!(
            "a {} where a mapping was expected (looking up {k:?})",
            py::type_name(v)
        ))),
    }
}

/// `(m.get(key) or {})` of a top-level section, as a mapping view.
pub fn section<'a>(m: &'a Value, key: &str) -> Result<Option<&'a Value>, Fail> {
    match m.get(key) {
        None => Ok(None),
        Some(v) if !py::truthy(v) => Ok(None),
        Some(v @ Value::Object(_)) => Ok(Some(v)),
        Some(v) => Err(Fail::Crash(format!(
            "{key}: a {} where a mapping was expected",
            py::type_name(v)
        ))),
    }
}

/// `int(x)`.
pub fn int(v: &Value, what: &str) -> Result<i128, Fail> {
    match v {
        Value::Bool(b) => Ok(*b as i128),
        Value::Number(n) => {
            if let Some(i) = py::int_of(v) {
                Ok(i)
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f.is_finite() {
                    Ok(f.trunc() as i128)
                } else {
                    Err(Fail::Crash(format!(
                        "{what}: {} is not a number",
                        py::repr(v)
                    )))
                }
            }
        }
        Value::String(s) => hydo_manifest::composer::py_int(s, 10)
            .map_err(|_| Fail::Crash(format!("{what}: {} is not a whole number", py::repr(v)))),
        other => Err(Fail::Crash(format!(
            "{what}: {} is not a whole number",
            py::repr(other)
        ))),
    }
}

/// `"%d" % x`.
pub fn pct_d(v: &Value, what: &str) -> Result<String, Fail> {
    match v {
        Value::Bool(_) | Value::Number(_) => int(v, what).map(|i| i.to_string()),
        other => Err(Fail::Crash(format!(
            "{what}: {} is not a number",
            py::repr(other)
        ))),
    }
}

/// `str(x)`.
pub fn s(v: &Value) -> String {
    py::str_of(v)
}

/// `" ".join(xs)` of a list (or of the characters of a string) whose items must be strings.
pub fn join_strs(v: &Value, sep: &str, what: &str) -> Result<String, Fail> {
    match v {
        Value::Array(a) => {
            let mut out = Vec::new();
            for x in a {
                match x {
                    Value::String(s) => out.push(s.clone()),
                    other => {
                        return Err(Fail::Crash(format!(
                            "{what}: {} is not a string",
                            py::repr(other)
                        )))
                    }
                }
            }
            Ok(out.join(sep))
        }
        Value::String(s) => Ok(s.chars().map(String::from).collect::<Vec<_>>().join(sep)),
        Value::Object(o) => Ok(o.keys().cloned().collect::<Vec<_>>().join(sep)),
        other => Err(Fail::Crash(format!(
            "{what}: {} is not a list of strings",
            py::repr(other)
        ))),
    }
}

/// The items of `x or []` when x is iterated (a list; a mapping gives its keys).
pub fn items(v: Option<&Value>, what: &str) -> Result<Vec<Value>, Fail> {
    match v {
        None => Ok(Vec::new()),
        Some(x) if !py::truthy(x) => Ok(Vec::new()),
        Some(Value::Array(a)) => Ok(a.clone()),
        Some(Value::Object(o)) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        Some(Value::String(s)) => Ok(s.chars().map(|c| Value::String(c.into())).collect()),
        Some(other) => Err(Fail::Crash(format!(
            "{what}: a {} is not a list",
            py::type_name(other)
        ))),
    }
}

/// A value that must be a string (Python would stop with a TypeError on anything else).
pub fn need_str<'a>(v: &'a Value, what: &str) -> Result<&'a str, Fail> {
    v.as_str().ok_or_else(|| {
        Fail::Crash(format!(
            "{what}: a {} where a string was expected",
            py::type_name(v)
        ))
    })
}
