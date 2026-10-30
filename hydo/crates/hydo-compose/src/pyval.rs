//! The Python values the Compose importer works on (what `yaml.safe_load` and `json.load` give),
//! with the Python semantics the reference relied on: `str()`, `repr()`, truthiness, `==`, dict
//! order (insertion order; a repeated key keeps its first place and takes the last value), and
//! `json.dumps` with its defaults or with `indent=1, sort_keys=True`.

use hydo_manifest::py;

#[derive(Clone, Debug)]
pub enum Y {
    Null,
    Bool(bool),
    Int(i128),
    /// An int too big for i128, as its decimal digits.
    BigInt(String),
    Float(f64),
    Str(String),
    /// A YAML timestamp (datetime.date / datetime.datetime): its `str()`.
    Date(String),
    Seq(Vec<Y>),
    Map(Vec<(Y, Y)>),
}

impl PartialEq for Y {
    fn eq(&self, o: &Y) -> bool {
        use Y::*;
        match (self, o) {
            (Null, Null) => true,
            (Str(a), Str(b)) => a == b,
            (Date(a), Date(b)) => a == b,
            (BigInt(a), BigInt(b)) => a == b,
            (Seq(a), Seq(b)) => a == b,
            (Map(a), Map(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, v)| b.iter().any(|(k2, v2)| k == k2 && v == v2))
            }
            (a, b) => match (a.num(), b.num()) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            },
        }
    }
}

impl Y {
    pub fn s(v: &str) -> Y {
        Y::Str(v.to_string())
    }

    /// The numeric value of a bool, int or float (Python compares them across types).
    fn num(&self) -> Option<f64> {
        match self {
            Y::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Y::Int(i) => Some(*i as f64),
            Y::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Y::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Y::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn is_map(&self) -> bool {
        matches!(self, Y::Map(_))
    }

    pub fn is_seq(&self) -> bool {
        matches!(self, Y::Seq(_))
    }

    /// `bool(x)`.
    pub fn truthy(&self) -> bool {
        match self {
            Y::Null => false,
            Y::Bool(b) => *b,
            Y::Int(i) => *i != 0,
            Y::BigInt(_) => true,
            Y::Float(f) => *f != 0.0,
            Y::Str(s) => !s.is_empty(),
            Y::Date(_) => true,
            Y::Seq(v) => !v.is_empty(),
            Y::Map(m) => !m.is_empty(),
        }
    }

    /// `d.get(k)` for a mapping (None for anything else, or a missing key).
    pub fn get(&self, k: &str) -> Option<&Y> {
        match self {
            Y::Map(m) => m
                .iter()
                .find(|(x, _)| matches!(x, Y::Str(s) if s == k))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// `d.get(k)` with None for a missing key or a null value collapsed: `d.get(k) or default`
    /// callers use [`Y::truthy`].
    pub fn getn(&self, k: &str) -> &Y {
        static NULL: Y = Y::Null;
        self.get(k).unwrap_or(&NULL)
    }

    /// `k in x`: a key of a mapping, an item of a list, a substring of a str.
    pub fn contains(&self, k: &Y) -> bool {
        match self {
            Y::Map(m) => m.iter().any(|(x, _)| x == k),
            Y::Seq(v) => v.iter().any(|x| x == k),
            Y::Str(s) => k.as_str().is_some_and(|k| s.contains(k)),
            _ => false,
        }
    }

    pub fn has(&self, k: &str) -> bool {
        self.get(k).is_some()
    }

    /// `for k in x`: the keys of a mapping, the items of a list, the characters of a str.
    pub fn iter_keys(&self) -> Vec<Y> {
        match self {
            Y::Map(m) => m.iter().map(|(k, _)| k.clone()).collect(),
            Y::Seq(v) => v.clone(),
            Y::Str(s) => s.chars().map(|c| Y::Str(c.to_string())).collect(),
            _ => Vec::new(),
        }
    }

    /// `d[k] = v`, keeping the first position of an existing key.
    pub fn set(&mut self, k: Y, v: Y) {
        if let Y::Map(m) = self {
            if let Some(e) = m.iter_mut().find(|(x, _)| *x == k) {
                e.1 = v;
            } else {
                m.push((k, v));
            }
        }
    }

    pub fn map_items(&self) -> &[(Y, Y)] {
        match self {
            Y::Map(m) => m,
            _ => &[],
        }
    }
}

/// `as_list(x)`: None -> [], a list as is, anything else -> [x].
pub fn as_list(x: Option<&Y>) -> Vec<Y> {
    match x {
        None | Some(Y::Null) => Vec::new(),
        Some(Y::Seq(v)) => v.clone(),
        Some(o) => vec![o.clone()],
    }
}

/// `str(x)`.
pub fn pstr(v: &Y) -> String {
    match v {
        Y::Str(s) => s.clone(),
        Y::Date(s) => s.clone(),
        other => repr(other),
    }
}

/// `repr(x)`.
pub fn repr(v: &Y) -> String {
    match v {
        Y::Null => "None".into(),
        Y::Bool(b) => if *b { "True" } else { "False" }.into(),
        Y::Int(i) => i.to_string(),
        Y::BigInt(s) => s.clone(),
        Y::Float(f) => py::repr_float(*f),
        Y::Str(s) => py::repr_str(s),
        Y::Date(s) => date_repr(s),
        Y::Seq(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Y::Map(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", repr(k), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `repr()` of a datetime.date / datetime.datetime from its `str()`.
fn date_repr(s: &str) -> String {
    let n = |x: &str| {
        x.trim_start_matches('0')
            .parse::<u32>()
            .unwrap_or(0)
            .to_string()
    };
    if s.len() == 10 {
        let p: Vec<&str> = s.split('-').collect();
        return format!("datetime.date({}, {}, {})", n(p[0]), n(p[1]), n(p[2]));
    }
    format!("datetime.datetime({s})")
}

// ---------------------------------------------------------------- JSON

/// `json.loads` into Python values (object order kept, a repeated key keeps its first place).
pub fn from_json(v: &serde_json::Value) -> Y {
    use serde_json::Value as V;
    match v {
        V::Null => Y::Null,
        V::Bool(b) => Y::Bool(*b),
        V::Number(n) => {
            if let Some(i) = n.as_i64() {
                Y::Int(i as i128)
            } else if let Some(u) = n.as_u64() {
                Y::Int(u as i128)
            } else {
                Y::Float(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        V::String(s) => Y::Str(s.clone()),
        V::Array(a) => Y::Seq(a.iter().map(from_json).collect()),
        V::Object(o) => Y::Map(o.iter().map(|(k, v)| (Y::s(k), from_json(v))).collect()),
    }
}

pub fn json_str(s: &str, out: &mut String) {
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
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `json.dumps(s)` of a str.
pub fn dumps_str(s: &str) -> String {
    let mut o = String::new();
    json_str(s, &mut o);
    o
}

fn json_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        }
    } else {
        py::repr_float(f)
    }
}

fn json_key(k: &Y) -> String {
    match k {
        Y::Str(s) => s.clone(),
        Y::Bool(b) => if *b { "true" } else { "false" }.into(),
        Y::Null => "null".into(),
        Y::Float(f) => json_float(*f),
        other => pstr(other),
    }
}

/// `json.dumps(v)` with the default separators.
pub fn dumps(v: &Y) -> String {
    let mut o = String::new();
    dump_flat(v, &mut o);
    o
}

fn dump_flat(v: &Y, o: &mut String) {
    match v {
        Y::Null => o.push_str("null"),
        Y::Bool(b) => o.push_str(if *b { "true" } else { "false" }),
        Y::Int(i) => o.push_str(&i.to_string()),
        Y::BigInt(s) => o.push_str(s),
        Y::Float(f) => o.push_str(&json_float(*f)),
        Y::Str(s) | Y::Date(s) => json_str(s, o),
        Y::Seq(a) => {
            o.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    o.push_str(", ");
                }
                dump_flat(x, o);
            }
            o.push(']');
        }
        Y::Map(m) => {
            o.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    o.push_str(", ");
                }
                json_str(&json_key(k), o);
                o.push_str(": ");
                dump_flat(x, o);
            }
            o.push('}');
        }
    }
}

/// `json.dump(v, f, indent=1, sort_keys=True)`.
pub fn dumps_indent1_sorted(v: &Y) -> String {
    let mut o = String::new();
    dump_ind(v, 0, &mut o);
    o
}

fn dump_ind(v: &Y, lvl: usize, o: &mut String) {
    match v {
        Y::Seq(a) if !a.is_empty() => {
            o.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                o.push('\n');
                o.push_str(&" ".repeat(lvl + 1));
                dump_ind(x, lvl + 1, o);
            }
            o.push('\n');
            o.push_str(&" ".repeat(lvl));
            o.push(']');
        }
        Y::Map(m) if !m.is_empty() => {
            let mut items: Vec<(String, &Y)> = m.iter().map(|(k, x)| (json_key(k), x)).collect();
            items.sort_by(|a, b| a.0.cmp(&b.0));
            o.push('{');
            for (i, (k, x)) in items.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                o.push('\n');
                o.push_str(&" ".repeat(lvl + 1));
                json_str(k, o);
                o.push_str(": ");
                dump_ind(x, lvl + 1, o);
            }
            o.push('\n');
            o.push_str(&" ".repeat(lvl));
            o.push('}');
        }
        other => dump_flat(other, o),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_forms() {
        let v = Y::Map(vec![
            (Y::s("b"), Y::Seq(vec![Y::Int(1), Y::s("é")])),
            (Y::s("a"), Y::Map(vec![])),
            (Y::s("c"), Y::Null),
        ]);
        assert_eq!(
            dumps_indent1_sorted(&v),
            "{\n \"a\": {},\n \"b\": [\n  1,\n  \"\\u00e9\"\n ],\n \"c\": null\n}"
        );
        assert_eq!(
            dumps(&v),
            "{\"b\": [1, \"\\u00e9\"], \"a\": {}, \"c\": null}"
        );
        assert_eq!(repr(&v), "{'b': [1, 'é'], 'a': {}, 'c': None}");
    }
}
