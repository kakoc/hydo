//! JSON text exactly as Python's `json.dump(obj, f, indent=N)` writes it (ensure_ascii, ", " / ": "
//! separators with an indent, float repr), so files the CLI writes (run.json) stay byte-identical
//! to the Python CLI's. Also Python's `%s` of a JSON value (`str()`), used in printed lines.

use serde_json::Value;

/// `json.dumps(v, indent=indent)`.
pub fn dumps(v: &Value, indent: usize) -> String {
    let mut out = String::new();
    write_value(&mut out, v, indent, 0);
    out
}

fn write_value(out: &mut String, v: &Value, indent: usize, level: usize) {
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

fn newline(out: &mut String, indent: usize, level: usize) {
    out.push('\n');
    for _ in 0..indent * level {
        out.push(' ');
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

/// Python's `repr(float)` for the values hydo writes (finite, not tiny).
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if f.fract() == 0.0 && f.abs() < 1e16 {
        return format!("{f:.1}");
    }
    let a = f.abs();
    if !(1e-4..1e16).contains(&a) {
        // Python switches to exponent notation here: 1e-05, 1e+16
        let s = format!("{f:e}");
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        let e: i32 = e.parse().unwrap_or(0);
        return format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    format!("{f}")
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
            c if (c as u32) < 0x20 || (c as u32) > 0x7f => {
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

/// Python's `str(x)` / `"%s" % x` of a value read from JSON.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => number(n),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python's `s[:n]` (characters, not bytes).
pub fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python's `s[-n:]` (characters).
pub fn tail(s: &str, n: usize) -> String {
    let len = s.chars().count();
    s.chars().skip(len.saturating_sub(n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn like_python() {
        let v = json!({"image": "/x/a.img", "engine_args": [], "secs": 12.0, "n": 3, "v": "é\"", "l": ["a", 1]});
        assert_eq!(
            dumps(&v, 1),
            "{\n \"image\": \"/x/a.img\",\n \"engine_args\": [],\n \"secs\": 12.0,\n \"n\": 3,\n \"v\": \"\\u00e9\\\"\",\n \"l\": [\n  \"a\",\n  1\n ]\n}"
        );
        assert_eq!(float_repr(12.3), "12.3");
        assert_eq!(float_repr(0.0), "0.0");
        assert_eq!(float_repr(1e-5), "1e-05");
        assert_eq!(py_str(&json!(7)), "7");
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(tail("abcdef", 2), "ef");
    }
}
