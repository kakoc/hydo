//! JSON text exactly as Python's `json.dumps` writes it: `ensure_ascii`, Python's float repr,
//! `sort_keys` (keys in code-point order) and the separators of an indented (`","` at line ends,
//! `": "`) or a compact (`","`, `":"`) dump. Every JSON file hydo-build puts into an image
//! (world.json, compose.json, an image service's image.json) and the HYDO-CHECK specs are written
//! this way, so their bytes do not change with the port.

use hydo_manifest::py;
use serde_json::Value;

/// How to dump: `json.dumps(v, indent=indent, sort_keys=sort_keys, separators=...)`.
#[derive(Clone, Copy)]
pub struct Style {
    pub indent: Option<usize>,
    pub sort_keys: bool,
    /// `separators=(",", ":")` (only without an indent).
    pub compact: bool,
}

/// `json.dumps(v, indent=1, sort_keys=True)`.
pub fn dumps_indent1_sorted(v: &Value) -> String {
    dumps(
        v,
        Style {
            indent: Some(1),
            sort_keys: true,
            compact: false,
        },
    )
}

/// `json.dumps(v, sort_keys=True, separators=(",", ":"))`.
pub fn dumps_compact_sorted(v: &Value) -> String {
    dumps(
        v,
        Style {
            indent: None,
            sort_keys: true,
            compact: true,
        },
    )
}

pub fn dumps(v: &Value, st: Style) -> String {
    let mut out = String::new();
    write(&mut out, v, st, 0);
    out
}

fn newline(out: &mut String, n: usize) {
    out.push('\n');
    for _ in 0..n {
        out.push(' ');
    }
}

fn sep(out: &mut String, st: Style) {
    out.push(',');
    if st.indent.is_none() && !st.compact {
        out.push(' ');
    }
}

fn write(out: &mut String, v: &Value, st: Style, level: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f.is_nan() {
                    out.push_str("NaN")
                } else if f.is_infinite() {
                    out.push_str(if f > 0.0 { "Infinity" } else { "-Infinity" })
                } else {
                    out.push_str(&py::repr_float(f))
                }
            } else {
                out.push_str(&n.to_string())
            }
        }
        Value::String(s) => write_str(out, s),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    sep(out, st);
                }
                if let Some(ind) = st.indent {
                    newline(out, ind * (level + 1));
                }
                write(out, x, st, level + 1);
            }
            if let Some(ind) = st.indent {
                newline(out, ind * level);
            }
            out.push(']');
        }
        Value::Object(o) => {
            if o.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut items: Vec<(&String, &Value)> = o.iter().collect();
            if st.sort_keys {
                items.sort_by(|a, b| py::cmp_str(a.0, b.0));
            }
            out.push('{');
            for (i, (k, x)) in items.into_iter().enumerate() {
                if i > 0 {
                    sep(out, st);
                }
                if let Some(ind) = st.indent {
                    newline(out, ind * (level + 1));
                }
                write_str(out, k);
                out.push_str(if st.compact { ":" } else { ": " });
                write(out, x, st, level + 1);
            }
            if let Some(ind) = st.indent {
                newline(out, ind * level);
            }
            out.push('}');
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn like_python() {
        let v = json!({"b": [1, 2.5, {"z": null, "a": true}], "a": "é\n", "c": [], "d": {}});
        // python3 -c 'import json; print(json.dumps(v, indent=1, sort_keys=True))'
        assert_eq!(
            dumps_indent1_sorted(&v),
            "{\n \"a\": \"\\u00e9\\n\",\n \"b\": [\n  1,\n  2.5,\n  {\n   \"a\": true,\n   \"z\": null\n  }\n ],\n \"c\": [],\n \"d\": {}\n}"
        );
        assert_eq!(
            dumps_compact_sorted(&v),
            "{\"a\":\"\\u00e9\\n\",\"b\":[1,2.5,{\"a\":true,\"z\":null}],\"c\":[],\"d\":{}}"
        );
    }
}
