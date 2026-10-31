//! The hints file as gopkg.in/yaml.v3 (v3.0.1) decodes it into the Go generator's structs with
//! `KnownFields(true)`: YAML 1.2 plain-scalar resolution (`yes` is a string, `0x10` and `010` are
//! numbers, unquoted dates are timestamps), anchors and aliases, unknown keys and type mismatches
//! collected as "line N: ..." errors in document order. The syntax is read by libyaml-safer, a
//! port of the libyaml parser that yaml.v3 is a port of too.

use hydo_grpc::gofmt;
use libyaml_safer::{ErrorKind, EventData, Parser, ScalarStyle};
use std::collections::HashMap;

/// One node of the document.
#[derive(Clone, Debug)]
pub enum Node {
    /// value, indicated string (quoted, literal, folded or tagged !!str), tag, line
    Scalar(String, bool, String, usize),
    Seq(Vec<Node>, usize),
    Map(Vec<(Node, Node)>, usize),
}

impl Node {
    pub fn line(&self) -> usize {
        match self {
            Node::Scalar(_, _, _, l) | Node::Seq(_, l) | Node::Map(_, l) => *l,
        }
    }
}

#[derive(Default)]
struct Builder {
    stack: Vec<Node>,
    anchors: HashMap<String, Node>,
    open_anchor: Vec<Option<String>>,
    root: Option<Node>,
    pending: Vec<Option<Node>>,
}

impl Builder {
    fn attach(&mut self, n: Node) {
        match self.stack.last_mut() {
            None => self.root = Some(n),
            Some(Node::Seq(items, _)) => items.push(n),
            Some(Node::Map(pairs, _)) => {
                let pk = self.pending.last_mut().expect("key slot");
                match pk.take() {
                    None => *pk = Some(n),
                    Some(k) => pairs.push((k, n)),
                }
            }
            Some(Node::Scalar(..)) => {}
        }
    }
}

fn short_tag(t: &str) -> String {
    match t.strip_prefix("tag:yaml.org,2002:") {
        Some(s) => format!("!!{s}"),
        None => t.to_string(),
    }
}

/// Parses one document (an empty stream is None).
pub fn parse(text: &str) -> Result<Option<Node>, String> {
    let mut b = Builder::default();
    let mut input = text.as_bytes();
    let mut p = Parser::new();
    p.set_input_string(&mut input);
    for ev in p {
        let ev = match ev {
            Ok(e) => e,
            Err(e) => {
                let scanner = e.kind() == ErrorKind::Scanner;
                let mut line = 0u64;
                if let Some(m) = e.context_mark().filter(|m| m.line != 0) {
                    line = m.line + u64::from(scanner);
                } else if let Some(m) = e.problem_mark().filter(|m| m.line != 0) {
                    line = m.line + u64::from(scanner);
                }
                let wh = if line != 0 {
                    format!("line {line}: ")
                } else {
                    String::new()
                };
                return Err(format!("yaml: {}{}", wh, e.problem()));
            }
        };
        let line = ev.start_mark.line as usize + 1;
        match ev.data {
            EventData::DocumentStart { .. } if b.root.is_some() => break,
            EventData::Scalar {
                anchor,
                tag,
                value,
                style,
                ..
            } => {
                let tag = tag.map(|t| short_tag(&t)).unwrap_or_default();
                let quoted = matches!(
                    style,
                    ScalarStyle::SingleQuoted
                        | ScalarStyle::DoubleQuoted
                        | ScalarStyle::Literal
                        | ScalarStyle::Folded
                );
                let indicated = tag == "!!str" || ((tag.is_empty() || tag == "!") && quoted);
                let n = Node::Scalar(value, indicated, tag, line);
                if let Some(a) = anchor {
                    b.anchors.insert(a, n.clone());
                }
                b.attach(n);
            }
            EventData::Alias { anchor } => match b.anchors.get(&anchor) {
                Some(n) => {
                    let n = n.clone();
                    b.attach(n)
                }
                None => return Err(format!("yaml: unknown anchor '{anchor}' referenced")),
            },
            EventData::SequenceStart { anchor, .. } => {
                b.stack.push(Node::Seq(Vec::new(), line));
                b.open_anchor.push(anchor);
            }
            EventData::MappingStart { anchor, .. } => {
                b.stack.push(Node::Map(Vec::new(), line));
                b.open_anchor.push(anchor);
                b.pending.push(None);
            }
            EventData::SequenceEnd | EventData::MappingEnd => {
                if matches!(ev.data, EventData::MappingEnd) {
                    b.pending.pop();
                }
                let n = b.stack.pop().expect("open collection");
                if let Some(Some(a)) = b.open_anchor.pop() {
                    b.anchors.insert(a, n.clone());
                }
                b.attach(n);
            }
            _ => {}
        }
    }
    Ok(b.root)
}

/// A plain scalar resolved as yaml.v3 resolves it (`resolve`).
#[derive(Clone, Debug, PartialEq)]
pub enum R {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Str(String),
    Time(String),
}

/// `strconv.ParseInt(s, 0, 64)` / `ParseUint(s, 0, 64)` without underscores (yaml.v3 removed them).
fn parse_base0(s: &str) -> Option<(bool, u64)> {
    let (neg, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (base, digits) =
        if let Some(d) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (16, d)
        } else if let Some(d) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            (2, d)
        } else if let Some(d) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
            (8, d)
        } else if body.len() > 1 && body.starts_with('0') {
            (8, &body[1..])
        } else {
            (10, body)
        };
    if digits.is_empty() || digits.starts_with(['+', '-']) {
        return None;
    }
    u64::from_str_radix(digits, base).ok().map(|v| (neg, v))
}

fn yaml_style_float(s: &str) -> bool {
    // ^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let st = i;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let d = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == d {
            return false;
        }
    } else {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == st {
            return false;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        let d = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == d {
            return false;
        }
    }
    i == b.len()
}

/// yaml.v3's parseTimestamp: four digits and a '-', then one of its layouts.
fn timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    let i = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if i != 4 || i == b.len() || b[i] != b'-' {
        return false;
    }
    // 2006-1-2, optionally followed by [Tt ]15:4:5[.999999999][Z07:00]
    let rest = &s[5..];
    let mut parts = rest.splitn(2, ['T', 't', ' ']);
    let date = parts.next().unwrap_or("");
    let dp: Vec<&str> = date.split('-').collect();
    let ok_date = dp.len() == 2
        && dp
            .iter()
            .all(|p| (1..=2).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_digit()));
    if !ok_date {
        return false;
    }
    match parts.next() {
        None => true,
        Some(t) => {
            let hms: Vec<&str> = t.splitn(3, ':').collect();
            hms.len() == 3
                && hms[0].bytes().all(|c| c.is_ascii_digit())
                && hms[1].bytes().all(|c| c.is_ascii_digit())
        }
    }
}

/// yaml.v3's `resolve` for an untagged plain scalar.
pub fn resolve(v: &str) -> R {
    match v {
        "true" | "True" | "TRUE" => return R::Bool(true),
        "false" | "False" | "FALSE" => return R::Bool(false),
        "" | "~" | "null" | "Null" | "NULL" => return R::Null,
        ".nan" | ".NaN" | ".NAN" => return R::Float(f64::NAN),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => return R::Float(f64::INFINITY),
        "-.inf" | "-.Inf" | "-.INF" => return R::Float(f64::NEG_INFINITY),
        _ => {}
    }
    let c = v.as_bytes()[0];
    if c == b'.' {
        if let Ok(f) = gofmt::parse_float(v, 64) {
            return R::Float(f);
        }
        return R::Str(v.into());
    }
    if !(c.is_ascii_digit() || c == b'+' || c == b'-') {
        return R::Str(v.into());
    }
    if timestamp(v) {
        return R::Time(v.into());
    }
    let plain = v.replace('_', "");
    if let Some((neg, u)) = parse_base0(&plain) {
        if neg {
            if u <= 1u64 << 63 {
                return R::Int((u as i64).wrapping_neg());
            }
        } else if u <= i64::MAX as u64 {
            return R::Int(u as i64);
        } else {
            return R::Uint(u);
        }
    }
    if yaml_style_float(&plain) {
        if let Ok(f) = gofmt::parse_float(&plain, 64) {
            return R::Float(f);
        }
    }
    R::Str(v.into())
}

/// The decoder: errors are collected (yaml.v3's terrors).
#[derive(Default)]
pub struct D {
    pub errs: Vec<String>,
}

/// A scalar's yaml.v3 view: its tag and its resolved value.
pub fn scalar_of(value: &str, indicated: bool, tag: &str) -> (String, R) {
    if indicated {
        return ("!!str".into(), R::Str(value.into()));
    }
    let r = resolve(value);
    let t = match &r {
        R::Null => "!!null",
        R::Bool(_) => "!!bool",
        R::Int(_) | R::Uint(_) => "!!int",
        R::Float(_) => "!!float",
        R::Str(_) => "!!str",
        R::Time(_) => "!!timestamp",
    };
    let t = if !tag.is_empty() && tag != "!" {
        tag.to_string()
    } else {
        t.to_string()
    };
    (t, r)
}

impl D {
    /// `terror`: line N: cannot unmarshal !!tag `value` into type
    pub fn terror(&mut self, n: &Node, ty: &str) {
        let (tag, value) = match n {
            Node::Scalar(v, ind, tag, _) => {
                let t = scalar_of(v, *ind, tag).0;
                let shown = if v.len() > 10 {
                    format!(" `{}...`", String::from_utf8_lossy(&v.as_bytes()[..7]))
                } else {
                    format!(" `{v}`")
                };
                (t, shown)
            }
            Node::Seq(..) => ("!!seq".into(), String::new()),
            Node::Map(..) => ("!!map".into(), String::new()),
        };
        self.errs.push(format!(
            "line {}: cannot unmarshal {}{} into {}",
            n.line(),
            tag,
            value,
            ty
        ));
    }

    fn is_null(n: &Node) -> bool {
        matches!(n, Node::Scalar(v, ind, tag, _) if scalar_of(v, *ind, tag).1 == R::Null)
    }

    /// A string field: any scalar's text.
    pub fn string(&mut self, n: &Node, out: &mut String) {
        match n {
            Node::Scalar(v, ind, tag, _) => {
                if scalar_of(v, *ind, tag).1 != R::Null {
                    *out = v.clone();
                }
            }
            _ => self.terror(n, "string"),
        }
    }

    /// An int field (Go `int`); a float is truncated, as yaml.v3 does.
    pub fn int(&mut self, n: &Node, out: &mut i64) -> bool {
        if let Node::Scalar(v, ind, tag, _) = n {
            match scalar_of(v, *ind, tag).1 {
                R::Null => return false,
                R::Int(i) => {
                    *out = i;
                    return true;
                }
                R::Float(f) if f <= i64::MAX as f64 && f >= i64::MIN as f64 && !f.is_nan() => {
                    *out = f as i64;
                    return true;
                }
                _ => {}
            }
        }
        self.terror(n, "int");
        false
    }

    pub fn opt_int(&mut self, n: &Node, out: &mut Option<i64>) {
        if Self::is_null(n) {
            *out = None;
            return;
        }
        let mut v = 0;
        self.int(n, &mut v);
        *out = Some(v);
    }

    pub fn opt_uint(&mut self, n: &Node, out: &mut Option<u64>) {
        if Self::is_null(n) {
            *out = None;
            return;
        }
        let mut v = 0u64;
        let ok = match n {
            Node::Scalar(s, ind, tag, _) => match scalar_of(s, *ind, tag).1 {
                R::Int(i) if i >= 0 => {
                    v = i as u64;
                    true
                }
                R::Uint(u) => {
                    v = u;
                    true
                }
                R::Float(f) if f >= 0.0 && f <= u64::MAX as f64 => {
                    v = f as u64;
                    true
                }
                _ => false,
            },
            _ => false,
        };
        if !ok {
            self.terror(n, "uint64");
        }
        *out = Some(v);
    }

    pub fn float(&mut self, n: &Node, out: &mut f64) {
        if let Node::Scalar(v, ind, tag, _) = n {
            match scalar_of(v, *ind, tag).1 {
                R::Null => return,
                R::Int(i) => {
                    *out = i as f64;
                    return;
                }
                R::Uint(u) => {
                    *out = u as f64;
                    return;
                }
                R::Float(f) => {
                    *out = f;
                    return;
                }
                _ => {}
            }
        }
        self.terror(n, "float64");
    }

    pub fn opt_float(&mut self, n: &Node, out: &mut Option<f64>) {
        if Self::is_null(n) {
            *out = None;
            return;
        }
        let mut v = 0.0;
        self.float(n, &mut v);
        *out = Some(v);
    }

    pub fn bool(&mut self, n: &Node, out: &mut bool) {
        if let Node::Scalar(v, ind, tag, _) = n {
            match scalar_of(v, *ind, tag).1 {
                R::Null => return,
                R::Bool(b) => {
                    *out = b;
                    return;
                }
                R::Str(s) => match s.as_str() {
                    "y" | "Y" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" => {
                        *out = true;
                        return;
                    }
                    "n" | "N" | "no" | "No" | "NO" | "off" | "Off" | "OFF" => {
                        *out = false;
                        return;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        self.terror(n, "bool");
    }

    pub fn opt_bool(&mut self, n: &Node, out: &mut Option<bool>) {
        if Self::is_null(n) {
            *out = None;
            return;
        }
        let mut v = false;
        self.bool(n, &mut v);
        *out = Some(v);
    }

    /// A slice: None for a null node (Go's nil slice).
    pub fn seq<T: Default>(
        &mut self,
        n: &Node,
        ty: &str,
        mut each: impl FnMut(&mut D, &Node, &mut T) -> bool,
    ) -> Option<Vec<T>> {
        match n {
            Node::Seq(items, _) => {
                let mut out = Vec::new();
                for it in items {
                    let mut v = T::default();
                    if each(self, it, &mut v) {
                        out.push(v);
                    }
                }
                Some(out)
            }
            _ if Self::is_null(n) => None,
            _ => {
                self.terror(n, ty);
                None
            }
        }
    }

    /// The pairs of a mapping, after yaml.v3's duplicate-key check; None (and an error) when the
    /// node is not a mapping. A null node gives Some(empty).
    pub fn pairs<'n>(&mut self, n: &'n Node, ty: &str) -> Option<Vec<(&'n Node, &'n Node)>> {
        match n {
            Node::Map(pairs, _) => {
                let before = self.errs.len();
                for (i, (ki, _)) in pairs.iter().enumerate() {
                    for (kj, _) in &pairs[i + 1..] {
                        if let (Node::Scalar(a, ..), Node::Scalar(b, ..)) = (ki, kj) {
                            if a == b {
                                self.errs.push(format!(
                                    "line {}: mapping key {} already defined at line {}",
                                    kj.line(),
                                    gofmt::quote(b),
                                    ki.line()
                                ));
                            }
                        }
                    }
                }
                if self.errs.len() > before {
                    return None;
                }
                Some(pairs.iter().map(|(k, v)| (k, v)).collect())
            }
            _ if Self::is_null(n) => Some(Vec::new()),
            _ => {
                self.terror(n, ty);
                None
            }
        }
    }

    /// A struct with the keys `names`: calls `field(name, value)` for every known key; unknown
    /// keys are errors (KnownFields); a key seen twice is "already set".
    pub fn strukt(
        &mut self,
        n: &Node,
        ty: &str,
        names: &[&str],
        mut field: impl FnMut(&mut D, &str, &Node),
    ) {
        let Some(pairs) = self.pairs(n, ty) else {
            return;
        };
        let mut seen: Vec<String> = Vec::new();
        for (k, v) in pairs {
            if !matches!(k, Node::Scalar(..)) {
                self.terror(k, "string");
                continue;
            }
            let mut name = String::new();
            self.string(k, &mut name);
            if !names.contains(&name.as_str()) {
                self.errs.push(format!(
                    "line {}: field {} not found in type {}",
                    k.line(),
                    name,
                    ty
                ));
                continue;
            }
            if seen.contains(&name) {
                self.errs.push(format!(
                    "line {}: field {} already set in type {}",
                    k.line(),
                    name,
                    ty
                ));
                continue;
            }
            seen.push(name.clone());
            field(self, &name, v);
        }
    }

    /// A value decoded into Go's `interface{}`.
    pub fn any(n: &Node) -> Any {
        match n {
            Node::Scalar(v, ind, tag, _) => match scalar_of(v, *ind, tag).1 {
                R::Null => Any::Null,
                R::Bool(b) => Any::Bool(b),
                R::Int(i) => Any::Int(i),
                R::Uint(u) => Any::Uint(u),
                R::Float(f) => Any::Float(f),
                R::Str(s) => Any::Str(s),
                R::Time(t) => Any::Time(t),
            },
            Node::Seq(items, _) => Any::Seq(items.iter().map(D::any).collect()),
            Node::Map(pairs, _) => {
                let string_keys = pairs.iter().all(|(k, _)| matches!(k, Node::Scalar(v, ind, tag, _) if scalar_of(v, *ind, tag).0 == "!!str"));
                Any::Map(
                    pairs.iter().map(|(k, v)| (D::any(k), D::any(v))).collect(),
                    string_keys,
                )
            }
        }
    }
}

/// A YAML timestamp as Go's `%v` of the time.Time yaml.v3 made of it
/// ("2026-01-01 00:00:00 +0000 UTC").
fn go_time(t: &str) -> String {
    let (date, rest) = match t.find(['T', 't', ' ']) {
        Some(i) => (&t[..i], t[i + 1..].trim_start()),
        None => (t, ""),
    };
    let d: Vec<u32> = date.split('-').filter_map(|x| x.parse().ok()).collect();
    if d.len() != 3 {
        return t.to_string();
    }
    let (mut clock, mut zone) = (rest.to_string(), String::new());
    if let Some(z) = clock.strip_suffix(['Z', 'z']) {
        clock = z.trim_end().to_string();
    } else if let Some(i) = clock.rfind(['+', '-']) {
        zone = clock[i..].replace(':', "");
        clock = clock[..i].trim_end().to_string();
    }
    let mut hms = [0u32; 3];
    let mut frac = String::new();
    if !clock.is_empty() {
        let (c, f) = clock.split_once('.').unwrap_or((&clock, ""));
        for (i, p) in c.split(':').enumerate().take(3) {
            hms[i] = p.parse().unwrap_or(0);
        }
        frac = f.trim_end_matches('0').to_string();
    }
    let frac = if frac.is_empty() {
        String::new()
    } else {
        format!(".{frac}")
    };
    let zone = if zone.is_empty() || zone == "+00" || zone == "+0000" {
        "+0000 UTC".to_string()
    } else {
        let z = if zone.len() == 3 {
            format!("{zone}00")
        } else {
            zone
        };
        format!("{z} {z}")
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}{} {}",
        d[0], d[1], d[2], hms[0], hms[1], hms[2], frac, zone
    )
}

/// Go's `interface{}` values from YAML.
#[derive(Clone, Debug, PartialEq)]
pub enum Any {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Str(String),
    Time(String),
    Seq(Vec<Any>),
    /// pairs; true when every key is a string (map[string]interface{})
    Map(Vec<(Any, Any)>, bool),
}

impl Any {
    /// Go's `%v`.
    pub fn go_v(&self) -> String {
        match self {
            Any::Null => "<nil>".into(),
            Any::Bool(b) => b.to_string(),
            Any::Int(i) => i.to_string(),
            Any::Uint(u) => u.to_string(),
            Any::Float(f) => gofmt::format_g(*f, 64),
            Any::Str(s) => s.clone(),
            Any::Time(t) => go_time(t),
            Any::Seq(v) => format!(
                "[{}]",
                v.iter().map(|x| x.go_v()).collect::<Vec<_>>().join(" ")
            ),
            Any::Map(p, _) => {
                let mut kv: Vec<(String, String)> =
                    p.iter().map(|(k, v)| (k.go_v(), v.go_v())).collect();
                kv.sort();
                format!(
                    "map[{}]",
                    kv.iter()
                        .map(|(k, v)| format!("{k}:{v}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            }
        }
    }

    /// Go's `%T`.
    pub fn go_t(&self) -> &'static str {
        match self {
            Any::Null => "<nil>",
            Any::Bool(_) => "bool",
            Any::Int(_) => "int",
            Any::Uint(_) => "uint64",
            Any::Float(_) => "float64",
            Any::Str(_) => "string",
            Any::Time(_) => "time.Time",
            Any::Seq(_) => "[]interface {}",
            Any::Map(_, true) => "map[string]interface {}",
            Any::Map(_, false) => "map[interface {}]interface {}",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_like_yaml_v3() {
        assert_eq!(resolve("yes"), R::Str("yes".into()));
        assert_eq!(resolve("0x10"), R::Int(16));
        assert_eq!(resolve("010"), R::Int(8));
        assert_eq!(resolve("08"), R::Float(8.0));
        assert_eq!(resolve("1_000"), R::Int(1000));
        assert_eq!(resolve("-5"), R::Int(-5));
        assert_eq!(resolve("18446744073709551615"), R::Uint(u64::MAX));
        assert_eq!(resolve("1.5"), R::Float(1.5));
        assert_eq!(resolve("1e3"), R::Float(1000.0));
        assert_eq!(resolve(".5"), R::Float(0.5));
        assert_eq!(resolve("2026-01-01"), R::Time("2026-01-01".into()));
        assert_eq!(resolve("1.2.3"), R::Str("1.2.3".into()));
        assert_eq!(resolve("~"), R::Null);
        assert_eq!(resolve("0b101"), R::Int(5));
        assert_eq!(resolve("0o17"), R::Int(15));
        assert_eq!(resolve("+1"), R::Int(1));
    }

    #[test]
    fn errors_like_yaml_v3() {
        let n = parse("a: 1\nb: x\n").unwrap().unwrap();
        let mut d = D::default();
        let mut a = 0;
        let mut b = 0;
        d.strukt(&n, "load.T", &["a", "b"], |d, k, v| {
            if k == "a" {
                d.int(v, &mut a);
            } else {
                d.int(v, &mut b);
            }
        });
        assert_eq!(a, 1);
        assert_eq!(d.errs, vec!["line 2: cannot unmarshal !!str `x` into int"]);
        let n = parse("a: 1\nwieght: 3\na: 2\n").unwrap().unwrap();
        let mut d = D::default();
        d.strukt(&n, "load.T", &["a"], |_, _, _| {});
        assert_eq!(
            d.errs,
            vec!["line 3: mapping key \"a\" already defined at line 1"]
        );
        assert!(parse("a: [1,\n").unwrap_err().starts_with("yaml: line"));
    }
}
