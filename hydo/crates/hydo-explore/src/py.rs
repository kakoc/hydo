//! Python's semantics where the explorer's output depends on them: integer `%` and `//` (the sign
//! of the divisor), `int()` of a decimal string, `repr()` of a string, `str()` of a float, and
//! `json.dumps` with and without an indent (ensure_ascii, ", " / ": " separators).

use serde_json::{Map, Value};

/// Python's `a % n`: the result has the sign of `n`. Panics on `n == 0` as Python raises.
pub fn pmod(a: i128, n: i128) -> i128 {
    assert!(n != 0, "integer modulo by zero");
    let r = a % n;
    if r != 0 && ((r < 0) != (n < 0)) {
        r + n
    } else {
        r
    }
}

/// Python's `a // n` (floor division).
pub fn fdiv(a: i64, n: i64) -> i64 {
    let q = a / n;
    if (a % n != 0) && ((a < 0) != (n < 0)) {
        q - 1
    } else {
        q
    }
}

/// Python's `int(text)` for a base-10 string (surrounding whitespace, a sign, `_` between digits).
pub fn py_int(text: &str) -> Option<i64> {
    let t = text.trim();
    let (neg, body) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return None;
    }
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let v: i64 = digits.parse().ok()?;
    Some(if neg { -v } else { v })
}

/// Python's `int(text, 16)` for the hex fields of a schedule (no `0x` in our own output).
pub fn py_hex(text: &str) -> Option<u64> {
    let t = text.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    let digits: String = t.chars().filter(|c| *c != '_').collect();
    u64::from_str_radix(&digits, 16).ok()
}

/// `str.isdigit()` for ASCII text (non-empty, every character a digit).
pub fn isdigit(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// Python's `repr(str)`.
pub fn repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// Python's `repr(float)` / `str(float)` for finite values.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if f.fract() == 0.0 && f.abs() < 1e16 {
        return format!("{f:.1}");
    }
    let a = f.abs();
    if !(1e-4..1e16).contains(&a) {
        let s = format!("{f:e}");
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        let e: i32 = e.parse().unwrap_or(0);
        return format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    format!("{f}")
}

/// Python's `round(x, 1)` as a float value.
pub fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}

/// Python's `str()` of a JSON value (`"%s" % v`).
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => number(n),
        Value::String(s) => s.clone(),
        other => dumps_line(other),
    }
}

/// `s[:n]` in characters.
pub fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `json.dumps(v)`: one line, ", " and ": " separators.
pub fn dumps_line(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v, None, 0);
    out
}

/// `json.dumps(v, indent=n)`.
pub fn dumps(v: &Value, indent: usize) -> String {
    let mut out = String::new();
    write_value(&mut out, v, Some(indent), 0);
    out
}

fn write_value(out: &mut String, v: &Value, indent: Option<usize>, level: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number(n)),
        Value::String(s) => write_str(out, s),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if indent.is_none() {
                        out.push(' ');
                    }
                }
                newline(out, indent, level + 1);
                write_value(out, x, indent, level + 1);
            }
            newline(out, indent, level);
            out.push(']');
        }
        Value::Object(m) => {
            if m.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if indent.is_none() {
                        out.push(' ');
                    }
                }
                newline(out, indent, level + 1);
                write_str(out, k);
                out.push_str(": ");
                write_value(out, x, indent, level + 1);
            }
            newline(out, indent, level);
            out.push('}');
        }
    }
}

fn newline(out: &mut String, indent: Option<usize>, level: usize) {
    if let Some(n) = indent {
        out.push('\n');
        for _ in 0..n * level {
            out.push(' ');
        }
    }
}

fn number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    float_repr(n.as_f64().unwrap_or(0.0))
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A float JSON value (serde_json keeps it a float, so it prints as `3.0`, not `3`).
pub fn fl(x: f64) -> Value {
    serde_json::Number::from_f64(x)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// An empty ordered map.
pub fn obj() -> Map<String, Value> {
    Map::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn like_python() {
        assert_eq!(pmod(7, 3), 1);
        assert_eq!(pmod(7, -2), -1);
        assert_eq!(pmod(-7, 2), 1);
        assert_eq!(fdiv(-3, 2), -2);
        assert_eq!(fdiv(7, 2), 3);
        assert_eq!(repr("a'b"), "\"a'b\"");
        assert_eq!(repr("x\n"), "'x\\n'");
        assert_eq!(repr(""), "''");
        assert_eq!(float_repr(12.0), "12.0");
        assert_eq!(float_repr(0.1), "0.1");
        assert_eq!(
            dumps_line(&json!({"a": [1, "é"], "b": 2.5})),
            "{\"a\": [1, \"\\u00e9\"], \"b\": 2.5}"
        );
        assert_eq!(dumps(&json!({"a": []}), 1), "{\n \"a\": []\n}");
        assert_eq!(py_int(" 12 "), Some(12));
        assert_eq!(py_int("1_0"), Some(10));
        assert_eq!(py_int("x"), None);
        assert_eq!(round1(2.25), 2.2);
    }
}
