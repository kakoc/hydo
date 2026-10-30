//! A YAML loader with PyYAML's `yaml.safe_load` semantics (YAML 1.1), which the Python importer
//! used whenever PyYAML was installed: the YAML 1.1 implicit types of plain scalars (`yes`/`no`/
//! `on`/`off` are booleans, `0755` is octal, `1:30` is base 60, dates are timestamps), anchors
//! and aliases, `<<` merge keys (the merged keys first, the explicit keys win), and a repeated key
//! keeps its first place with the last value. The syntax is read by yaml-rust2's event parser.

use crate::pyval::Y;
use libyaml_safer::{EventData, Parser, ScalarStyle};
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

const TAG: &str = "tag:yaml.org,2002:";

#[derive(Debug)]
enum Node {
    /// value, plain (unquoted), tag
    Scalar(String, bool, Option<String>),
    Seq(Vec<usize>, Option<String>),
    Map(Vec<(usize, usize)>, Option<String>),
}

/// PyYAML's composer: the node graph of the one document, aliases pointing at their anchors.
#[derive(Default)]
struct Builder {
    nodes: Vec<Node>,
    anchors: HashMap<String, usize>,
    stack: Vec<usize>,
    pending_key: Vec<Option<usize>>,
    roots: Vec<usize>,
}

impl Builder {
    fn attach(&mut self, idx: usize) {
        let Some(&top) = self.stack.last() else {
            self.roots.push(idx);
            return;
        };
        match &mut self.nodes[top] {
            Node::Seq(items, _) => items.push(idx),
            Node::Map(pairs, _) => {
                let pk = self.pending_key.last_mut().expect("map key slot");
                match pk.take() {
                    None => *pk = Some(idx),
                    Some(k) => pairs.push((k, idx)),
                }
            }
            Node::Scalar(..) => {}
        }
    }

    fn new_node(&mut self, n: Node, anchor: Option<String>) -> usize {
        self.nodes.push(n);
        let idx = self.nodes.len() - 1;
        if let Some(a) = anchor {
            self.anchors.insert(a, idx);
        }
        idx
    }

    fn event(&mut self, ev: EventData) -> Result<(), String> {
        match ev {
            EventData::Scalar {
                anchor,
                tag,
                value,
                style,
                ..
            } => {
                let i = self.new_node(
                    Node::Scalar(value, style == ScalarStyle::Plain, tag),
                    anchor,
                );
                self.attach(i);
            }
            EventData::Alias { anchor } => match self.anchors.get(&anchor) {
                Some(&i) => self.attach(i),
                None => return Err(format!("found undefined alias '{anchor}'")),
            },
            EventData::SequenceStart { anchor, tag, .. } => {
                let i = self.new_node(Node::Seq(Vec::new(), tag), anchor);
                self.attach(i);
                self.stack.push(i);
            }
            EventData::MappingStart { anchor, tag, .. } => {
                let i = self.new_node(Node::Map(Vec::new(), tag), anchor);
                self.attach(i);
                self.stack.push(i);
                self.pending_key.push(None);
            }
            EventData::SequenceEnd => {
                self.stack.pop();
            }
            EventData::MappingEnd => {
                self.stack.pop();
                self.pending_key.pop();
            }
            _ => {}
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- PyYAML's resolver

struct Res {
    bool_: Regex,
    float: Regex,
    int: Regex,
    null: Regex,
    ts: Regex,
}

fn res() -> &'static Res {
    static R: OnceLock<Res> = OnceLock::new();
    R.get_or_init(|| Res {
        bool_: Regex::new(r"^(?:yes|Yes|YES|no|No|NO|true|True|TRUE|false|False|FALSE|on|On|ON|off|Off|OFF)$").unwrap(),
        float: Regex::new(r"^(?:[-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?|\.[0-9][0-9_]*(?:[eE][-+][0-9]+)?|[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*|[-+]?\.(?:inf|Inf|INF)|\.(?:nan|NaN|NAN))$").unwrap(),
        int: Regex::new(r"^(?:[-+]?0b[0-1_]+|[-+]?0[0-7_]+|[-+]?(?:0|[1-9][0-9_]*)|[-+]?0x[0-9a-fA-F_]+|[-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+)$").unwrap(),
        null: Regex::new(r"^(?:~|null|Null|NULL|)$").unwrap(),
        ts: Regex::new(r"^(?P<y>[0-9][0-9][0-9][0-9])-(?P<m>[0-9][0-9]?)-(?P<d>[0-9][0-9]?)(?:(?:[Tt]|[ \t]+)(?P<H>[0-9][0-9]?):(?P<M>[0-9][0-9]):(?P<S>[0-9][0-9])(?:\.(?P<f>[0-9]*))?(?:[ \t]*(?P<tz>Z|(?P<ts>[-+])(?P<th>[0-9][0-9]?)(?::(?P<tm>[0-9][0-9]))?))?)?$").unwrap(),
    })
}

/// The implicit tag of a plain scalar (PyYAML's resolvers, in their order per first character).
fn resolve(v: &str) -> &'static str {
    let r = res();
    let c = v.chars().next();
    let has = |set: &str| c.is_some_and(|c| set.contains(c));
    if v.is_empty() {
        return "null";
    }
    if has("yYnNtTfFoO") && r.bool_.is_match(v) {
        return "bool";
    }
    if has("-+0123456789.") && r.float.is_match(v) {
        return "float";
    }
    if has("-+0123456789") && r.int.is_match(v) {
        return "int";
    }
    if v == "<<" {
        return "merge";
    }
    if has("~nN") && r.null.is_match(v) {
        return "null";
    }
    if has("0123456789") && r.ts.is_match(v) {
        return "timestamp";
    }
    if v == "=" {
        return "value";
    }
    "str"
}

fn parse_radix(s: &str, radix: u32) -> Result<Y, String> {
    if s.is_empty() {
        return Err(format!(
            "invalid literal for int() with base {radix}: {}",
            hydo_manifest::py::repr_str(s)
        ));
    }
    match i128::from_str_radix(s, radix) {
        Ok(i) => Ok(Y::Int(i)),
        Err(_) if radix == 10 && s.chars().all(|c| c.is_ascii_digit()) => {
            Ok(Y::BigInt(s.trim_start_matches('0').to_string()))
        }
        Err(e) => Err(format!("int(): {e}")),
    }
}

fn neg(y: Y, sign: i128) -> Y {
    match (y, sign) {
        (Y::Int(i), s) => Y::Int(i * s),
        (Y::BigInt(b), -1) => Y::BigInt(format!("-{b}")),
        (y, _) => y,
    }
}

fn construct_int(v: &str) -> Result<Y, String> {
    let mut v = v.replace('_', "");
    let mut sign = 1i128;
    if v.starts_with('-') {
        sign = -1;
    }
    if v.starts_with('-') || v.starts_with('+') {
        v.remove(0);
    }
    if v == "0" {
        return Ok(Y::Int(0));
    }
    if let Some(b) = v.strip_prefix("0b") {
        return parse_radix(b, 2).map(|y| neg(y, sign));
    }
    if let Some(h) = v.strip_prefix("0x") {
        return parse_radix(h, 16).map(|y| neg(y, sign));
    }
    if v.starts_with('0') {
        return parse_radix(&v, 8).map(|y| neg(y, sign));
    }
    if v.contains(':') {
        let mut total: i128 = 0;
        let mut base: i128 = 1;
        for part in v.split(':').rev() {
            let d: i128 = part.parse().map_err(|e| format!("int(): {e}"))?;
            total += d * base;
            base *= 60;
        }
        return Ok(Y::Int(sign * total));
    }
    parse_radix(&v, 10).map(|y| neg(y, sign))
}

fn construct_float(v: &str) -> Result<Y, String> {
    let mut v = v.replace('_', "").to_lowercase();
    let mut sign = 1.0f64;
    if v.starts_with('-') {
        sign = -1.0;
    }
    if v.starts_with('-') || v.starts_with('+') {
        v.remove(0);
    }
    if v == ".inf" {
        return Ok(Y::Float(sign * f64::INFINITY));
    }
    if v == ".nan" {
        return Ok(Y::Float(f64::NAN));
    }
    if v.contains(':') {
        let mut total = 0.0;
        let mut base = 1.0;
        for part in v.split(':').rev() {
            let d: f64 = part
                .parse()
                .map_err(|_| format!("could not convert string to float: '{part}'"))?;
            total += d * base;
            base *= 60.0;
        }
        return Ok(Y::Float(sign * total));
    }
    v.parse::<f64>()
        .map(|f| Y::Float(sign * f))
        .map_err(|_| format!("could not convert string to float: '{v}'"))
}

/// datetime.date / datetime.datetime as `str()` gives it.
fn construct_timestamp(v: &str) -> Result<Y, String> {
    let c = res()
        .ts
        .captures(v)
        .ok_or_else(|| format!("bad timestamp {v}"))?;
    let n = |k: &str| c.name(k).map(|m| m.as_str().parse::<u32>().unwrap_or(0));
    let (y, mo, d) = (n("y").unwrap(), n("m").unwrap(), n("d").unwrap());
    let mdays = [
        31,
        28 + u32::from(y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&mo) || d < 1 || d > mdays[(mo - 1) as usize] || y < 1 {
        return Err("day is out of range for month".into());
    }
    let date = format!("{y:04}-{mo:02}-{d:02}");
    let Some(h) = n("H") else {
        return Ok(Y::Date(date));
    };
    let (mi, s) = (n("M").unwrap(), n("S").unwrap());
    if h > 23 || mi > 59 || s > 59 {
        return Err("hour/minute/second out of range".into());
    }
    let mut out = format!("{date} {h:02}:{mi:02}:{s:02}");
    if let Some(f) = c.name("f") {
        let mut f: String = f.as_str().chars().take(6).collect();
        while f.len() < 6 {
            f.push('0');
        }
        if f != "000000" {
            out.push('.');
            out.push_str(&f);
        }
    }
    if let Some(tz) = c.name("tz") {
        if tz.as_str() == "Z" {
            out.push_str("+00:00");
        } else {
            let th = n("th").unwrap_or(0);
            let tm = n("tm").unwrap_or(0);
            let sign = c.name("ts").map(|m| m.as_str()).unwrap_or("+");
            let mins = th * 60 + tm;
            let (sg, mins) = if sign == "-" && mins > 0 {
                ("-", mins)
            } else {
                ("+", mins)
            };
            out.push_str(&format!("{sg}{:02}:{:02}", mins / 60, mins % 60));
        }
    }
    Ok(Y::Date(out))
}

struct Ctor<'a> {
    nodes: &'a [Node],
}

impl Ctor<'_> {
    fn is_merge(&self, k: usize) -> bool {
        match &self.nodes[k] {
            Node::Scalar(v, true, None) => v == "<<",
            Node::Scalar(_, _, Some(t)) => t == &format!("{TAG}merge"),
            _ => false,
        }
    }

    /// PyYAML's flatten_mapping: the pairs of a mapping with `<<` merges applied.
    fn flat(&self, idx: usize, depth: usize) -> Result<Vec<(usize, usize)>, String> {
        if depth > 100 {
            return Err("merge keys nest too deep (a cycle?)".into());
        }
        let Node::Map(pairs, _) = &self.nodes[idx] else {
            return Err("expected a mapping for merging".into());
        };
        let (mut merge, mut rest) = (Vec::new(), Vec::new());
        for &(k, v) in pairs {
            if self.is_merge(k) {
                match &self.nodes[v] {
                    Node::Map(..) => merge.extend(self.flat(v, depth + 1)?),
                    Node::Seq(items, _) => {
                        let mut sub = Vec::new();
                        for &s in items {
                            if !matches!(self.nodes[s], Node::Map(..)) {
                                return Err("while constructing a mapping: expected a mapping for merging".into());
                            }
                            sub.push(self.flat(s, depth + 1)?);
                        }
                        for x in sub.into_iter().rev() {
                            merge.extend(x);
                        }
                    }
                    _ => {
                        return Err("while constructing a mapping: expected a mapping or list of mappings for merging".into())
                    }
                }
            } else {
                rest.push((k, v));
            }
        }
        merge.extend(rest);
        Ok(merge)
    }

    fn scalar(&self, v: &str, plain: bool, tag: Option<&str>, key: bool) -> Result<Y, String> {
        let t = match tag {
            None | Some("!") if plain => resolve(v).to_string(),
            None | Some("!") => "str".into(),
            Some(t) => match t.strip_prefix(TAG) {
                Some(x) => x.to_string(),
                None => {
                    return Err(format!(
                        "could not determine a constructor for the tag '{t}'"
                    ))
                }
            },
        };
        match t.as_str() {
            "str" => Ok(Y::s(v)),
            "null" => Ok(Y::Null),
            "bool" => match v.to_lowercase().as_str() {
                "yes" | "true" | "on" => Ok(Y::Bool(true)),
                "no" | "false" | "off" => Ok(Y::Bool(false)),
                _ => Err(format!("KeyError: '{}'", v.to_lowercase())),
            },
            "int" => construct_int(v),
            "float" => construct_float(v),
            "timestamp" => construct_timestamp(v),
            "value" if key => Ok(Y::s(v)),
            other => Err(format!(
                "could not determine a constructor for the tag 'tag:yaml.org,2002:{other}'"
            )),
        }
    }

    fn build(&self, idx: usize, depth: usize, key: bool) -> Result<Y, String> {
        if depth > 200 {
            return Err("the document nests too deep (a recursive alias?)".into());
        }
        match &self.nodes[idx] {
            Node::Scalar(v, plain, tag) => self.scalar(v, *plain, tag.as_deref(), key),
            Node::Seq(items, tag) => {
                if let Some(t) = tag {
                    if t != &format!("{TAG}seq") && t != "!" {
                        return Err(format!(
                            "could not determine a constructor for the tag '{t}'"
                        ));
                    }
                }
                items
                    .iter()
                    .map(|&i| self.build(i, depth + 1, false))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Y::Seq)
            }
            Node::Map(_, tag) => {
                if let Some(t) = tag {
                    if t != &format!("{TAG}map") && t != "!" {
                        return Err(format!(
                            "could not determine a constructor for the tag '{t}'"
                        ));
                    }
                }
                let mut m = Y::Map(Vec::new());
                for (k, v) in self.flat(idx, 0)? {
                    let ky = self.build(k, depth + 1, true)?;
                    if ky.is_map() || ky.is_seq() {
                        return Err("while constructing a mapping: found unhashable key".into());
                    }
                    let vy = self.build(v, depth + 1, false)?;
                    m.set(ky, vy);
                }
                Ok(m)
            }
        }
    }
}

/// `yaml.safe_load(text)`.
pub fn safe_load(text: &str) -> Result<Y, String> {
    let mut b = Builder::default();
    let mut input = text.as_bytes();
    let mut p = Parser::new();
    p.set_input_string(&mut input);
    let mut docs = 0;
    for ev in p {
        let ev = ev.map_err(|e| e.to_string())?;
        if matches!(ev.data, EventData::DocumentStart { .. }) {
            docs += 1;
            if docs > 1 {
                return Err(
                    "expected a single document in the stream, but found another document".into(),
                );
            }
        }
        b.event(ev.data)?;
    }
    let Some(&root) = b.roots.first() else {
        return Ok(Y::Null);
    };
    Ctor { nodes: &b.nodes }.build(root, 0, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyval::repr;

    fn r(t: &str) -> String {
        repr(&safe_load(t).unwrap())
    }

    #[test]
    fn yaml11_scalars() {
        assert_eq!(
            r("a: yes\nb: off\nc: 0755\nd: 1:30\ne: 1.5\nf: ~\ng:\nh: 1e5\ni: 1.0e+5\nj: 0x1f\nk: '7'\nl: 2024-01-02\nm: Null\nn: n\n"),
            "{'a': True, 'b': False, 'c': 493, 'd': 90, 'e': 1.5, 'f': None, 'g': None, 'h': '1e5', 'i': 100000.0, 'j': 31, 'k': '7', 'l': datetime.date(2024, 1, 2), 'm': None, 'n': 'n'}"
        );
        assert_eq!(r("p: 22:22\nq: 8080:80\n"), "{'p': 1342, 'q': '8080:80'}");
    }

    #[test]
    fn merges() {
        let t = "x: &a {k: 1, m: 2}\ny: &b {k: 3, z: 4}\ns:\n  <<: [*a, *b]\n  m: 9\n  q: 0\n";
        assert_eq!(
            r(t),
            "{'x': {'k': 1, 'm': 2}, 'y': {'k': 3, 'z': 4}, 's': {'k': 1, 'z': 4, 'm': 9, 'q': 0}}"
        );
        assert_eq!(r("a: 1\na: 2\nb: 3\n"), "{'a': 2, 'b': 3}");
    }
}
