//! Python semantics the reference implementation (hydo-build, hydo_composer.py) relies on, over
//! `serde_json::Value`: `repr()` and `str()` of a value (error messages quote them), truthiness,
//! `isinstance(x, int)` (true for booleans too), `==` across types, `set(x)` of a mapping or a
//! string, `str.strip()` with Python's whitespace, and `json.dumps()` with its default separators.

use serde_json::{Map, Number, Value};

use crate::ManifestError;

/// Python's `str.isspace()` for one character.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t'
            | '\n'
            | '\x0b'
            | '\x0c'
            | '\r'
            | '\x1c'..='\x1f'
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// Python's `str.split()` without arguments.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|x| !x.is_empty()).collect()
}

/// Python's `str.isprintable()` for one character, closely enough for repr(): control and format
/// characters, separators other than the space, and private-use characters are not printable.
fn printable(c: char) -> bool {
    let u = c as u32;
    if u < 0x20 || (0x7f..0xa0).contains(&u) {
        return false;
    }
    !matches!(
        c,
        '\u{a0}'
            | '\u{ad}'
            | '\u{600}'..='\u{605}'
            | '\u{61c}'
            | '\u{6dd}'
            | '\u{70f}'
            | '\u{1680}'
            | '\u{180e}'
            | '\u{2000}'..='\u{200f}'
            | '\u{2028}'..='\u{202f}'
            | '\u{205f}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{3000}'
            | '\u{e000}'..='\u{f8ff}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{f0000}'..='\u{10ffff}'
    )
}

/// `repr()` of a Python str.
pub fn repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if printable(c) => out.push(c),
            c => {
                let u = c as u32;
                if u < 0x100 {
                    out.push_str(&format!("\\x{u:02x}"));
                } else if u < 0x10000 {
                    out.push_str(&format!("\\u{u:04x}"));
                } else {
                    out.push_str(&format!("\\U{u:08x}"));
                }
            }
        }
    }
    out.push(q);
    out
}

/// `repr()` of a Python float.
pub fn repr_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let exp = f.abs().log10().floor() as i32;
    // Python's repr switches to exponent notation outside [1e-4, 1e16)
    let e = format!("{f:e}"); // shortest round-trip digits, e.g. "1.5e16"
    let (mant, ex) = e.split_once('e').unwrap_or((&e, "0"));
    let ex: i32 = ex.parse().unwrap_or(exp);
    if !(-4..16).contains(&ex) {
        let sign = if ex < 0 { '-' } else { '+' };
        return format!("{mant}e{sign}{:02}", ex.abs());
    }
    let s = format!("{f}");
    if s.contains('.') {
        s
    } else {
        format!("{s}.0")
    }
}

fn repr_number(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        repr_float(n.as_f64().unwrap_or(f64::NAN))
    }
}

/// `repr()` of a value.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => repr_number(n),
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", repr_str(k), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `str()` of a value (a str is itself; anything else is its repr).
pub fn str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// Python truthiness.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `isinstance(v, int)`: a whole number or a boolean.
pub fn is_int(v: &Value) -> bool {
    matches!(v, Value::Bool(_)) || matches!(v, Value::Number(n) if n.is_i64() || n.is_u64())
}

/// A whole number that is not a boolean.
pub fn is_plain_int(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.is_i64() || n.is_u64())
}

/// The integer value of an int or bool (Python: True == 1).
pub fn int_of(v: &Value) -> Option<i128> {
    match v {
        Value::Bool(b) => Some(*b as i128),
        Value::Number(n) => n
            .as_i64()
            .map(|i| i as i128)
            .or_else(|| n.as_u64().map(|u| u as i128)),
        _ => None,
    }
}

/// A number (int, bool or float) as f64, for ordered comparisons.
pub fn num_of(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(*b as i64 as f64),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Python `==` between two values (1 == True == 1.0; dicts compare without order).
pub fn eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| eq(v, w)))
        }
        _ => match (int_of(a), int_of(b)) {
            (Some(p), Some(q)) => p == q,
            _ => match (num_of(a), num_of(b)) {
                (Some(p), Some(q)) => p == q,
                _ => false,
            },
        },
    }
}

/// `x in [a, b, ...]`.
pub fn contains(list: &[Value], x: &Value) -> bool {
    list.iter().any(|y| eq(y, x))
}

/// `x in ("a", "b")` for a tuple of strings.
pub fn in_strs(x: &Value, strs: &[&str]) -> bool {
    matches!(x, Value::String(s) if strs.contains(&s.as_str()))
}

/// The message a reference run would end with when it hits a Python exception instead of a
/// ManifestError (a traceback). Such inputs are rejected here as well.
pub fn crash(what: impl Into<String>) -> ManifestError {
    ManifestError {
        message: format!("{}{}", CRASH_PREFIX, what.into()),
    }
}

/// Messages of refusals where hydo-build stops with a Python exception (a traceback) rather than
/// a ManifestError start with this.
pub const CRASH_PREFIX: &str = "hydo-build stops here with a Python exception: ";

/// `set(x)` of a value, as the names `_unknown` compares: the keys of a mapping, the characters of
/// a string, the hashable elements of a list. Anything else is a Python TypeError.
pub fn keys_of(v: &Value) -> Result<Vec<Value>, ManifestError> {
    match v {
        Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Array(a) => {
            if a.iter().any(|x| matches!(x, Value::Array(_) | Value::Object(_))) {
                Err(crash("unhashable type in a set"))
            } else {
                Ok(a.clone())
            }
        }
        _ => Err(crash(format!("{} object is not iterable", type_name(v)))),
    }
}

/// `k in v` for the container kinds hydo-build applies it to.
pub fn has(v: &Value, k: &str) -> Result<bool, ManifestError> {
    match v {
        Value::Object(o) => Ok(o.contains_key(k)),
        Value::String(s) => Ok(s.contains(k)),
        Value::Array(a) => Ok(a.iter().any(|x| matches!(x, Value::String(s) if s == k))),
        _ => Err(crash(format!("argument of type {} is not iterable", repr_str(type_name(v))))),
    }
}

/// `v.get(k)` on a mapping; anything else has no `.get` (a Python AttributeError).
pub fn get<'a>(v: &'a Value, k: &str) -> Result<Option<&'a Value>, ManifestError> {
    match v {
        Value::Object(o) => Ok(o.get(k)),
        _ => Err(crash(format!("{} object has no attribute 'get'", repr_str(type_name(v))))),
    }
}

/// `v[k]` on a mapping (KeyError / TypeError otherwise).
pub fn index<'a>(v: &'a Value, k: &str) -> Result<&'a Value, ManifestError> {
    match v {
        Value::Object(o) => o.get(k).ok_or_else(|| crash(format!("KeyError: {}", repr_str(k)))),
        _ => Err(crash(format!("{} object is not subscriptable by a string", type_name(v)))),
    }
}

pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `sorted()` of the names `_unknown` reports: all strings, or all numbers; a mix is a TypeError.
pub fn sorted_names(mut xs: Vec<Value>) -> Result<Vec<Value>, ManifestError> {
    let all_str = xs.iter().all(|x| x.is_string());
    let all_num = xs.iter().all(|x| num_of(x).is_some());
    if all_str {
        xs.sort_by(|a, b| cmp_str(a.as_str().unwrap_or(""), b.as_str().unwrap_or("")));
        Ok(xs)
    } else if all_num {
        xs.sort_by(|a, b| num_of(a).partial_cmp(&num_of(b)).unwrap_or(std::cmp::Ordering::Equal));
        Ok(xs)
    } else if xs.len() <= 1 {
        Ok(xs)
    } else {
        Err(crash("'<' not supported between instances of mixed types"))
    }
}

/// Python orders strings by code point, which is Rust's byte order of UTF-8 as well.
pub fn cmp_str(a: &str, b: &str) -> std::cmp::Ordering {
    a.cmp(b)
}

/// `json.dumps(v)` with Python's defaults: `", "` and `": "` separators, ASCII only.
pub fn json_dumps(v: &Value) -> String {
    let mut out = String::new();
    dump(v, &mut out);
    out
}

fn dump(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                out.push_str(&repr_float(n.as_f64().unwrap_or(f64::NAN)).replace("inf", "Infinity").replace("nan", "NaN"))
            } else {
                out.push_str(&repr_number(n))
            }
        }
        Value::String(s) => dump_str(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump(x, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            out.push('{');
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_str(k, out);
                out.push_str(": ");
                dump(x, out);
            }
            out.push('}');
        }
    }
}

fn dump_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A new JSON object from (key, value) pairs.
pub fn obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    Value::Object(m)
}

/// Python's `%d` of an int value.
pub fn d(v: &Value) -> String {
    int_of(v).map(|i| i.to_string()).unwrap_or_else(|| repr(v))
}

/// Python's `os.path.normpath` for an absolute POSIX path.
pub fn normpath(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let initial = if p.starts_with("//") && !p.starts_with("///") {
        2
    } else if p.starts_with('/') {
        1
    } else {
        0
    };
    let mut comps: Vec<&str> = Vec::new();
    for c in p.split('/') {
        if c.is_empty() || c == "." {
            continue;
        }
        if c != ".." || (initial == 0 && comps.is_empty()) || comps.last() == Some(&"..") {
            comps.push(c);
        } else if !comps.is_empty() {
            comps.pop();
        }
    }
    let mut s = "/".repeat(initial);
    s.push_str(&comps.join("/"));
    if s.is_empty() {
        ".".into()
    } else {
        s
    }
}

/// Python's `os.path.abspath`.
pub fn abspath(p: &str) -> String {
    if p.starts_with('/') {
        normpath(p)
    } else {
        let cwd = std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        normpath(&format!("{cwd}/{p}"))
    }
}

/// Python's `os.path.dirname`.
pub fn dirname(p: &str) -> String {
    let i = p.rfind('/').map(|i| i + 1).unwrap_or(0);
    let head = &p[..i];
    if !head.is_empty() && head.chars().any(|c| c != '/') {
        head.trim_end_matches('/').to_string()
    } else {
        head.to_string()
    }
}

/// Python's `os.path.basename`.
pub fn basename(p: &str) -> String {
    p.rsplit('/').next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reprs() {
        assert_eq!(repr_str("a'b"), "\"a'b\"");
        assert_eq!(repr_str("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr_str("x\n\x01"), "'x\\n\\x01'");
        assert_eq!(repr(&json!([1, true, null, "s"])), "[1, True, None, 's']");
        assert_eq!(repr_float(1.0), "1.0");
        assert_eq!(repr_float(1e16), "1e+16");
        assert_eq!(repr_float(0.5), "0.5");
        assert_eq!(repr_float(1.5e-5), "1.5e-05");
    }

    #[test]
    fn dumps() {
        assert_eq!(json_dumps(&json!([["a", "é"], 5])), "[[\"a\", \"\\u00e9\"], 5]");
    }

    #[test]
    fn paths() {
        assert_eq!(normpath("/a/b/../c/./d/"), "/a/c/d");
        assert_eq!(dirname("/a/b/c.yaml"), "/a/b");
        assert_eq!(dirname("c.yaml"), "");
        assert_eq!(basename("/a/b/c.yaml"), "c.yaml");
    }
}
