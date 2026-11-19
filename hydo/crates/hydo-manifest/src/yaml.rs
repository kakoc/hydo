//! hydo-build's YAML subset (`parse_yaml`, `_strip_comment`, `_scalar`, `_flow`), ported line for
//! line: block mappings and sequences, `|`/`|-`/`|+` literal and `>`/`>-` folded blocks, flow
//! `[..]`/`{..}`, quoted and plain scalars, comments. Since Milestone 316 also anchors (`&name`
//! before a value), aliases (`*name` as a whole value) and `<<` merge keys, as YAML 1.1 and
//! PyYAML's `safe_load` read them; a manifest without them reads as before. No tags or
//! multi-documents. Indentation and positions are counted in characters, as Python counts them.

use regex::Regex;
use serde_json::{Map, Number, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::py::{self, crash, lstrip, rstrip, strip};
use crate::ManifestError;

fn yerr(msg: String) -> ManifestError {
    ManifestError { message: msg }
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// The text of `line` up to a `#` comment (one that starts the line or follows a blank), with
/// trailing blanks removed. Quotes protect a `#`.
pub fn strip_comment(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut q: Option<char> = None;
    for (i, &c) in chars.iter().enumerate() {
        if let Some(qc) = q {
            if c == '\\' && qc == '"' {
                out.push(c);
                continue;
            }
            if c == qc {
                q = None;
            }
        } else if c == '\'' || c == '"' {
            q = Some(c);
        } else if c == '#' && (i == 0 || chars[i - 1] == ' ' || chars[i - 1] == '\t') {
            break;
        }
        out.push(c);
    }
    rstrip(&out).to_string()
}

fn int_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^-?\d+$").unwrap())
}

/// A key of a mapping as JSON holds it (Python dict keys may be any scalar; json.dumps writes
/// them as strings).
pub fn key_string(k: &Value) -> Result<String, ManifestError> {
    match k {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Ok("null".into()),
        Value::Bool(b) => Ok(if *b { "true" } else { "false" }.into()),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(crash("unhashable type as a mapping key")),
    }
}

/// The anchors a document has defined so far (`&name`), by name.
type Anchors = HashMap<String, Value>;

/// An anchor or alias name: what YAML allows, less the flow indicators, so that a plain value
/// such as a cron line (`*/5 * * * *`) stays a string.
fn re_name() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[A-Za-z0-9_.-]+$").unwrap())
}

/// `&name rest` at the start of a value: (name, rest without its leading blanks).
fn take_anchor(t: &str) -> Option<(String, String)> {
    let r = t.strip_prefix('&')?;
    let end = r.find([' ', '\t']).unwrap_or(r.len());
    let name = &r[..end];
    re_name()
        .is_match(name)
        .then(|| (name.to_string(), strip(&r[end..]).to_string()))
}

/// The name of an alias `*name` that is a whole token.
fn alias_name(t: &str) -> Option<&str> {
    t.strip_prefix('*').filter(|n| re_name().is_match(n))
}

/// An alias's value, or the error for one whose anchor is not defined before it.
fn resolve_alias(name: &str, al: &Anchors, wh: &str) -> Result<Value, ManifestError> {
    al.get(name).cloned().ok_or_else(|| {
        yerr(format!(
            "{wh}: alias *{name} has no anchor &{name} before it"
        ))
    })
}

/// `_scalar`: one scalar (or flow collection) token.
pub fn scalar(tok: &str, wh: &str) -> Result<Value, ManifestError> {
    scalar_in(tok, wh, None)
}

/// [`scalar`] inside a document, where a plain `*name` is an alias of `al`.
fn scalar_in(tok: &str, wh: &str, al: Option<&Anchors>) -> Result<Value, ManifestError> {
    let t = strip(tok);
    if t.is_empty() {
        return Ok(Value::Null);
    }
    if let (Some(al), Some(n)) = (al, alias_name(t)) {
        return resolve_alias(n, al, wh);
    }
    let first = t.chars().next().unwrap_or(' ');
    if first == '"' {
        if !t.ends_with('"') || char_len(t) < 2 {
            return Err(yerr(format!("{wh}: unterminated string {t}")));
        }
        return match serde_json::from_str::<Value>(t) {
            Ok(v @ Value::String(_)) => Ok(v),
            Ok(_) => Err(crash("json.loads gave a non-string")),
            Err(e) => Err(crash(format!("json.decoder.JSONDecodeError: {e}"))),
        };
    }
    if first == '\'' {
        if !t.ends_with('\'') || char_len(t) < 2 {
            return Err(yerr(format!("{wh}: unterminated string {t}")));
        }
        let inner: String = {
            let cs: Vec<char> = t.chars().collect();
            cs[1..cs.len() - 1].iter().collect()
        };
        return Ok(Value::String(inner.replace("''", "'")));
    }
    if first == '[' || first == '{' {
        let cs: Vec<char> = t.chars().collect();
        let (v, rest) = flow_in(&cs, 0, wh, al)?;
        let tail: String = cs[rest.min(cs.len())..].iter().collect();
        if !strip(&tail).is_empty() {
            return Err(yerr(format!("{wh}: junk after {t}")));
        }
        return Ok(v);
    }
    match t {
        "true" | "True" | "yes" => return Ok(Value::Bool(true)),
        "false" | "False" | "no" => return Ok(Value::Bool(false)),
        "null" | "~" => return Ok(Value::Null),
        _ => {}
    }
    if int_re().is_match(t) {
        if t.chars().all(|c| c == '-' || c.is_ascii_digit()) {
            if let Ok(i) = t.parse::<i64>() {
                return Ok(Value::Number(i.into()));
            }
            if let Ok(u) = t.parse::<u64>() {
                return Ok(Value::Number(Number::from(u)));
            }
            return Err(crash(format!("the integer {t} is larger than 64 bits")));
        }
        return Err(crash(format!(
            "non-ASCII digits in {t} (Python reads them as a number)"
        )));
    }
    Ok(Value::String(t.to_string()))
}

/// `_flow`: `[a, b]` / `{k: v}` with nesting, starting at `s[i]`; returns (value, next index).
pub fn flow(s: &[char], i: usize, wh: &str) -> Result<(Value, usize), ManifestError> {
    flow_in(s, i, wh, None)
}

/// [`flow`] inside a document: its items may be aliases of `al`, and a `<<` key merges.
fn flow_in(
    s: &[char],
    mut i: usize,
    wh: &str,
    al: Option<&Anchors>,
) -> Result<(Value, usize), ManifestError> {
    let open_c = s[i];
    let close_c = if open_c == '[' { ']' } else { '}' };
    i += 1;
    let mut items: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut depth = 0usize;
    let mut q: Option<char> = None;
    let mut closed = false;
    while i < s.len() {
        let c = s[i];
        if let Some(qc) = q {
            buf.push(c);
            if c == '\\' && qc == '"' {
                match s.get(i + 1) {
                    Some(&n) => buf.push(n),
                    None => return Err(crash("IndexError: string index out of range")),
                }
                i += 2;
                continue;
            }
            if c == qc {
                q = None;
            }
        } else if c == '\'' || c == '"' {
            q = Some(c);
            buf.push(c);
        } else if c == '[' || c == '{' {
            depth += 1;
            buf.push(c);
        } else if (c == ']' || c == '}') && depth > 0 {
            depth -= 1;
            buf.push(c);
        } else if c == close_c {
            if !strip(&buf).is_empty() {
                items.push(std::mem::take(&mut buf));
            }
            closed = true;
            break;
        } else if c == ',' && depth == 0 {
            items.push(std::mem::take(&mut buf));
        } else {
            buf.push(c);
        }
        i += 1;
    }
    if !closed {
        return Err(yerr(format!("{wh}: unterminated {open_c}")));
    }
    if open_c == '[' {
        let mut out = Vec::new();
        for x in &items {
            out.push(scalar_in(x, wh, al)?);
        }
        return Ok((Value::Array(out), i + 1));
    }
    let mut d = Merging::default();
    for x in &items {
        let Some((k, v)) = x.split_once(':') else {
            return Err(yerr(format!(
                "{wh}: flow mapping entry without ':' ({})",
                strip(x)
            )));
        };
        let key = key_string(&scalar(k, wh)?)?;
        let val = scalar_in(v, wh, al)?;
        if al.is_some() && key == "<<" {
            d.merge(val, wh)?;
        } else {
            d.insert(key, val);
        }
    }
    Ok((Value::Object(d.done()), i + 1))
}

/// A mapping being read, with its `<<` merge keys (YAML 1.1's merge type, as PyYAML's
/// `flatten_mapping` applies it): the merged pairs come first, a list's mappings in reverse
/// order, then the mapping's own pairs; a later pair wins, at the place its key was first seen.
/// So the mapping's own keys win over merged ones, and an earlier mapping of a merged list wins
/// over a later one.
#[derive(Default)]
struct Merging {
    merged: Vec<(String, Value)>,
    own: Map<String, Value>,
}

impl Merging {
    fn contains_key(&self, k: &str) -> bool {
        self.own.contains_key(k)
    }

    fn insert(&mut self, k: String, v: Value) {
        self.own.insert(k, v);
    }

    fn merge(&mut self, v: Value, wh: &str) -> Result<(), ManifestError> {
        let not_map = || {
            yerr(format!(
                "{wh}: a << merge key takes a mapping or a list of mappings"
            ))
        };
        match v {
            Value::Object(m) => self.merged.extend(m),
            Value::Array(xs) => {
                let mut maps = Vec::new();
                for x in xs {
                    match x {
                        Value::Object(m) => maps.push(m),
                        _ => return Err(not_map()),
                    }
                }
                for m in maps.into_iter().rev() {
                    self.merged.extend(m);
                }
            }
            _ => return Err(not_map()),
        }
        Ok(())
    }

    fn done(self) -> Map<String, Value> {
        if self.merged.is_empty() {
            return self.own;
        }
        let mut out = Map::new();
        for (k, v) in self.merged.into_iter().chain(self.own) {
            out.insert(k, v);
        }
        out
    }
}

struct Line {
    lineno: usize,
    indent: usize,
    content: String,
    raw: usize,
}

struct Parser<'a> {
    name: &'a str,
    raw: Vec<&'a str>,
    lines: Vec<Line>,
    pos: usize,
    anchors: Anchors,
}

fn re_seq_map1() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"^[^'"\[{][^:]*:( |$)"#).unwrap())
}

fn re_seq_map2() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"^"[^"]*":( |$)"#).unwrap())
}

fn re_pair() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"^("(?:[^"\\]|\\.)*"|'[^']*'|[^:]+?):( (.*)|$)"#).unwrap())
}

impl<'a> Parser<'a> {
    fn block_literal(&mut self, start_raw: usize, parent_indent: usize) -> String {
        let mut body: Vec<String> = Vec::new();
        let mut j = start_raw + 1;
        let mut ind: Option<usize> = None;
        while j < self.raw.len() {
            let ln = self.raw[j];
            if strip(ln).is_empty() {
                body.push(String::new());
                j += 1;
                continue;
            }
            let k = char_len(ln) - char_len(lstrip(ln));
            if k <= parent_indent {
                break;
            }
            let ii = *ind.get_or_insert(k);
            if k < ii {
                break;
            }
            body.push(ln.chars().skip(ii).collect());
            j += 1;
        }
        while body.last().is_some_and(|x| x.is_empty()) {
            body.pop();
        }
        while self.pos < self.lines.len() && self.lines[self.pos].raw < j {
            self.pos += 1;
        }
        body.join("\n") + "\n"
    }

    fn value_after(
        &mut self,
        rest: &str,
        indent: usize,
        lineno: usize,
        raw_idx: usize,
    ) -> Result<Value, ManifestError> {
        if let Some((name, after)) = take_anchor(rest) {
            let v = self.value_after(&after, indent, lineno, raw_idx)?;
            self.anchors.insert(name, v.clone());
            return Ok(v);
        }
        if rest == "|" || rest == "|-" || rest == "|+" {
            let v = self.block_literal(raw_idx, indent);
            return Ok(Value::String(if rest == "|-" {
                v.trim_end_matches('\n').to_string()
            } else {
                v
            }));
        }
        if rest == ">" || rest == ">-" {
            let b = self.block_literal(raw_idx, indent);
            let v = b
                .split('\n')
                .filter(|x| !strip(x).is_empty())
                .map(strip)
                .collect::<Vec<_>>()
                .join(" ");
            return Ok(Value::String(if rest == ">-" { v } else { v + "\n" }));
        }
        if rest.is_empty() {
            if self.pos < self.lines.len() && self.lines[self.pos].indent > indent {
                let ni = self.lines[self.pos].indent;
                return self.node(ni);
            }
            if self.pos < self.lines.len()
                && self.lines[self.pos].indent == indent
                && self.lines[self.pos].content.starts_with("- ")
            {
                return self.node(indent);
            }
            return Ok(Value::Null);
        }
        scalar_in(
            rest,
            &format!("{}:{}", self.name, lineno),
            Some(&self.anchors),
        )
    }

    /// Add `k: v` to a mapping being read: a `<<` key merges, any other key once.
    fn add_pair(
        &self,
        m: &mut Merging,
        k: String,
        v: Value,
        lineno: usize,
    ) -> Result<(), ManifestError> {
        let wh = format!("{}:{}", self.name, lineno);
        if k == "<<" {
            return m.merge(v, &wh);
        }
        if m.contains_key(&k) {
            return Err(yerr(format!("{wh}: duplicate key {k}")));
        }
        m.insert(k, v);
        Ok(())
    }

    fn node(&mut self, indent: usize) -> Result<Value, ManifestError> {
        let content = self.lines[self.pos].content.clone();
        if content.starts_with("- ") || content == "-" {
            let mut seq = Vec::new();
            while self.pos < self.lines.len()
                && self.lines[self.pos].indent == indent
                && (self.lines[self.pos].content.starts_with("- ")
                    || self.lines[self.pos].content == "-")
            {
                let lineno = self.lines[self.pos].lineno;
                let ridx = self.lines[self.pos].raw;
                let mut item = strip(
                    &self.lines[self.pos]
                        .content
                        .chars()
                        .skip(1)
                        .collect::<String>(),
                )
                .to_string();
                self.pos += 1;
                // `- &name ...`: the item's value is also the anchor's; a mapping that starts on
                // the item's line has its keys at the column of its first key
                let mut anchor = None;
                let mut shift = 0;
                if let Some((n, after)) = take_anchor(&item) {
                    shift = char_len(&item) - char_len(&after);
                    anchor = Some(n);
                    item = after;
                }
                let at = seq.len();
                if item.is_empty() {
                    if self.pos < self.lines.len() && self.lines[self.pos].indent > indent {
                        let ni = self.lines[self.pos].indent;
                        seq.push(self.node(ni)?);
                    } else {
                        seq.push(Value::Null);
                    }
                } else if re_seq_map1().is_match(&item) || re_seq_map2().is_match(&item) {
                    let sub_indent = indent + 2 + shift;
                    let (k, rest) = item.split_once(':').unwrap_or((&item, ""));
                    let key = key_string(&scalar(k, self.name)?)?;
                    let v = self.value_after(strip(rest), sub_indent, lineno, ridx)?;
                    let mut m = Merging::default();
                    self.add_pair(&mut m, key, v, lineno)?;
                    while self.pos < self.lines.len()
                        && self.lines[self.pos].indent == sub_indent
                        && !self.lines[self.pos].content.starts_with("- ")
                    {
                        let (k2, v2) = self.pair()?;
                        let ln = self.lines[self.pos - 1].lineno;
                        self.add_pair(&mut m, k2, v2, ln)?;
                    }
                    seq.push(Value::Object(m.done()));
                } else {
                    seq.push(scalar_in(
                        &item,
                        &format!("{}:{}", self.name, lineno),
                        Some(&self.anchors),
                    )?);
                }
                if let Some(n) = anchor {
                    self.anchors.insert(n, seq[at].clone());
                }
            }
            return Ok(Value::Array(seq));
        }
        let mut m = Merging::default();
        while self.pos < self.lines.len() && self.lines[self.pos].indent == indent {
            if self.lines[self.pos].content.starts_with("- ") {
                return Err(yerr(format!(
                    "{}:{}: a list item where a key was expected",
                    self.name, self.lines[self.pos].lineno
                )));
            }
            let (k, v) = self.pair()?;
            let ln = self.lines[self.pos - 1].lineno;
            self.add_pair(&mut m, k, v, ln)?;
        }
        if self.pos < self.lines.len() && self.lines[self.pos].indent > indent {
            return Err(yerr(format!(
                "{}:{}: unexpected indentation",
                self.name, self.lines[self.pos].lineno
            )));
        }
        Ok(Value::Object(m.done()))
    }

    fn pair(&mut self) -> Result<(String, Value), ManifestError> {
        let lineno = self.lines[self.pos].lineno;
        let indent = self.lines[self.pos].indent;
        let ridx = self.lines[self.pos].raw;
        let content = self.lines[self.pos].content.clone();
        let Some(mm) = re_pair().captures(&content) else {
            return Err(yerr(format!(
                "{}:{}: expected 'key: value', got {}",
                self.name,
                lineno,
                py::repr_str(&content)
            )));
        };
        let wh = format!("{}:{}", self.name, lineno);
        let key = key_string(&scalar(mm.get(1).map(|x| x.as_str()).unwrap_or(""), &wh)?)?;
        let rest = strip(mm.get(3).map(|x| x.as_str()).unwrap_or("")).to_string();
        self.pos += 1;
        let v = self.value_after(&rest, indent, lineno, ridx)?;
        Ok((key, v))
    }
}

/// `parse_yaml(text, name)`.
pub fn parse_yaml(text: &str, name: &str) -> Result<Value, ManifestError> {
    let raw: Vec<&str> = text.split('\n').collect();
    let mut lines = Vec::new();
    for (i, ln) in raw.iter().enumerate() {
        let lead_len = char_len(ln) - char_len(lstrip(ln));
        if ln.chars().take(lead_len).any(|c| c == '\t') {
            return Err(yerr(format!("{}:{}: tab in indentation", name, i + 1)));
        }
        let c = strip_comment(ln);
        if !strip(&c).is_empty() {
            lines.push(Line {
                lineno: i + 1,
                indent: char_len(&c) - char_len(lstrip(&c)),
                content: strip(&c).to_string(),
                raw: i,
            });
        }
    }
    if lines.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let first = lines[0].indent;
    let mut p = Parser {
        name,
        raw,
        lines,
        pos: 0,
        anchors: Anchors::new(),
    };
    let v = p.node(first)?;
    if p.pos != p.lines.len() {
        return Err(yerr(format!(
            "{}:{}: could not parse from here (indentation?)",
            name, p.lines[p.pos].lineno
        )));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn y(t: &str) -> Result<Value, String> {
        parse_yaml(t, "t.yaml").map_err(|e| e.message)
    }

    #[test]
    fn mappings_and_sequences() {
        assert_eq!(
            y("a: 1\nb:\n  - x\n  - y: 2\n    z: no\nc: [1, 'q', \"r\"]\n").unwrap(),
            json!({"a": 1, "b": ["x", {"y": 2, "z": false}], "c": [1, "q", "r"]})
        );
        assert_eq!(y("k:\n- a\n- b\n").unwrap(), json!({"k": ["a", "b"]}));
        assert_eq!(y("").unwrap(), json!({}));
        assert_eq!(y("# only\n\n").unwrap(), json!({}));
    }

    #[test]
    fn comments_and_quotes() {
        assert_eq!(
            y("a: x # c\nb: 'y # z'\nc: x#y\n").unwrap(),
            json!({"a": "x", "b": "y # z", "c": "x#y"})
        );
        assert_eq!(
            y("a: 'it''s'\nb: \"tab\\tx\"\n").unwrap(),
            json!({"a": "it's", "b": "tab\tx"})
        );
        assert_eq!(
            y("a: ~\nb: null\nc: -12\nd: 1.5\n").unwrap(),
            json!({"a": null, "b": null, "c": -12, "d": "1.5"})
        );
    }

    #[test]
    fn blocks() {
        assert_eq!(
            y("a: |\n  x\n   y\n\nb: 1\n").unwrap(),
            json!({"a": "x\n y\n", "b": 1})
        );
        assert_eq!(y("a: |-\n  x\n  y\n").unwrap(), json!({"a": "x\ny"}));
        assert_eq!(y("a: >\n  x\n  y\n").unwrap(), json!({"a": "x y\n"}));
        assert_eq!(y("a: >-\n  x\n\n  y\n").unwrap(), json!({"a": "x y"}));
    }

    #[test]
    fn flows() {
        assert_eq!(
            y("a: {x: 1, y: [2, 3], 'z': \"w\"}\n").unwrap(),
            json!({"a": {"x": 1, "y": [2, 3], "z": "w"}})
        );
        assert_eq!(y("a: []\nb: {}\n").unwrap(), json!({"a": [], "b": {}}));
    }

    #[test]
    fn refusals() {
        assert_eq!(y("a: 1\na: 2\n").unwrap_err(), "t.yaml:2: duplicate key a");
        assert_eq!(
            y("a:\n\tb: 1\n").unwrap_err(),
            "t.yaml:2: tab in indentation"
        );
        assert_eq!(
            y("a: 'x\n").unwrap_err(),
            "t.yaml:1: unterminated string 'x"
        );
        assert_eq!(y("a: [1, 2\n").unwrap_err(), "t.yaml:1: unterminated [");
        assert_eq!(
            y("a: {x}\n").unwrap_err(),
            "t.yaml:1: flow mapping entry without ':' (x)"
        );
        assert_eq!(y("a: [1] x\n").unwrap_err(), "t.yaml:1: junk after [1] x");
        assert_eq!(
            y("a: 1\n  b: 2\n").unwrap_err(),
            "t.yaml:2: unexpected indentation"
        );
        assert_eq!(
            y("a: 1\n- b\n").unwrap_err(),
            "t.yaml:2: a list item where a key was expected"
        );
        assert_eq!(
            y("just text\n").unwrap_err(),
            "t.yaml:1: expected 'key: value', got 'just text'"
        );
        assert_eq!(
            y("  a: 1\nb: 2\n").unwrap_err(),
            "t.yaml:2: could not parse from here (indentation?)"
        );
    }

    /// Milestone 316: anchors, aliases and `<<` merge keys, as PyYAML's `safe_load` reads them.
    #[test]
    fn anchors_aliases_and_merge_keys() {
        // a top-level extension field as a Compose file has it, merged into a service
        let t = "x-common: &common\n  image: app:1\n  restart: always\n  env:\n    - A=1\n\
                 services:\n  - name: api\n    <<: *common\n    env:\n      - A=2\n  \
                 - name: worker\n    <<: *common\n";
        assert_eq!(
            y(t).unwrap(),
            json!({
                "x-common": {"image": "app:1", "restart": "always", "env": ["A=1"]},
                "services": [
                    {"image": "app:1", "restart": "always", "env": ["A=2"], "name": "api"},
                    {"image": "app:1", "restart": "always", "env": ["A=1"], "name": "worker"}
                ]
            })
        );
        // scalars, a block literal and a list item as anchors; aliases in flows
        assert_eq!(
            y("a: &n 5\nb: *n\nc: &s |\n  x\nd: *s\ne:\n  - &i one\n  - *i\nf: [*n, *i]\ng: {k: *n}\n")
                .unwrap(),
            json!({"a": 5, "b": 5, "c": "x\n", "d": "x\n", "e": ["one", "one"],
                   "f": [5, "one"], "g": {"k": 5}})
        );
        // a list of merged mappings: the earlier one wins, the mapping's own keys win over both,
        // and the keys keep the place where they were first seen
        assert_eq!(
            y("a: &a {x: 1, y: 1}\nb: &b {y: 2, z: 2}\nc:\n  w: 0\n  <<: [*a, *b]\n  x: 3\n")
                .unwrap()["c"],
            json!({"y": 1, "z": 2, "x": 3, "w": 0})
        );
        // an anchored mapping that starts on a list item's line
        assert_eq!(
            y("l:\n  - &m k: 1\n       j: 2\n  - *m\n").unwrap(),
            json!({"l": [{"k": 1, "j": 2}, {"k": 1, "j": 2}]})
        );
        // what is not an alias stays the text it was: a cron line, a quoted star
        assert_eq!(
            y("a: */5 * * * *\nb: '*x'\nc: a*b\n").unwrap(),
            json!({"a": "*/5 * * * *", "b": "*x", "c": "a*b"})
        );
        assert_eq!(
            y("a: *nope\n").unwrap_err(),
            "t.yaml:1: alias *nope has no anchor &nope before it"
        );
        assert_eq!(
            y("a: &s 1\nb:\n  <<: *s\n").unwrap_err(),
            "t.yaml:3: a << merge key takes a mapping or a list of mappings"
        );
        assert_eq!(
            y("a: &m {x: 1}\nb:\n  <<: *m\n  y: 1\n  y: 2\n").unwrap_err(),
            "t.yaml:5: duplicate key y"
        );
        // the scalar reader outside a document is unchanged
        assert_eq!(scalar("*x", "w").unwrap(), json!("*x"));
    }
}
