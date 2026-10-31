//! JSON as Go's encoding/json writes it (`json.Marshal`, `json.MarshalIndent`): keys in struct
//! order, HTML-safe string escapes, Go's float format. The plan file and the assertion records
//! are written with it, so their bytes match the Go programs'.

use crate::gofmt;

/// A JSON value with ordered object keys.
#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

/// encoding/json's string escaping (with its default HTML escaping).
pub fn write_str(out: &mut String, s: &str) {
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
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn scalar(out: &mut String, j: &J) {
    match j {
        J::Null => out.push_str("null"),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        J::Int(i) => out.push_str(&i.to_string()),
        J::Uint(u) => out.push_str(&u.to_string()),
        J::Float(f) => out.push_str(&gofmt::json_float(*f, 64)),
        J::Str(s) => write_str(out, s),
        _ => unreachable!(),
    }
}

/// `json.Marshal`: compact.
pub fn marshal(j: &J) -> String {
    let mut out = String::new();
    compact(&mut out, j);
    out
}

fn compact(out: &mut String, j: &J) {
    match j {
        J::Arr(xs) => {
            out.push('[');
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                compact(out, x);
            }
            out.push(']');
        }
        J::Obj(kv) => {
            out.push('{');
            for (i, (k, v)) in kv.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(out, k);
                out.push(':');
                compact(out, v);
            }
            out.push('}');
        }
        _ => scalar(out, j),
    }
}

/// `json.MarshalIndent(v, "", "  ")`.
pub fn marshal_indent(j: &J) -> String {
    let mut out = String::new();
    indent(&mut out, j, 0);
    out
}

fn nl(out: &mut String, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn indent(out: &mut String, j: &J, depth: usize) {
    match j {
        J::Arr(xs) if xs.is_empty() => out.push_str("[]"),
        J::Obj(kv) if kv.is_empty() => out.push_str("{}"),
        J::Arr(xs) => {
            out.push('[');
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, depth + 1);
                indent(out, x, depth + 1);
            }
            nl(out, depth);
            out.push(']');
        }
        J::Obj(kv) => {
            out.push('{');
            for (i, (k, v)) in kv.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, depth + 1);
                write_str(out, k);
                out.push_str(": ");
                indent(out, v, depth + 1);
            }
            nl(out, depth);
            out.push('}');
        }
        _ => scalar(out, j),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_go() {
        let j = J::Obj(vec![
            ("a".into(), J::Arr(vec![J::Int(1), J::Int(10)])),
            ("b".into(), J::Arr(vec![])),
            ("c".into(), J::Obj(vec![])),
            ("d".into(), J::Str("<x>&\u{1}\u{8}é\u{2028}".into())),
            ("e".into(), J::Float(0.85)),
            ("f".into(), J::Null),
        ]);
        assert_eq!(
            marshal_indent(&j),
            "{\n  \"a\": [\n    1,\n    10\n  ],\n  \"b\": [],\n  \"c\": {},\n  \"d\": \"\\u003cx\\u003e\\u0026\\u0001\\bé\\u2028\",\n  \"e\": 0.85,\n  \"f\": null\n}"
        );
        assert_eq!(marshal(&J::Obj(vec![("t".into(), J::Bool(true))])), "{\"t\":true}");
    }
}
