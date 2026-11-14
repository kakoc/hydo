//! `hydo fake from-openapi` (Milestone 249): an OpenAPI 3.0 or 3.1 description of an outside
//! service becomes a `fakes:` entry of hydo.yaml, so that a project does not write every route of
//! a provider that has no ready-made emulator by hand. The design is docs/design/openapi-fakes.md,
//! and `hydo guide outside-services` is the reference for users.
//!
//! What it maps (the table in the design lists each case):
//! - each operation (a method of a path) is one route: `method`, `path` (the first server's base
//!   path, then the operation's path with its `{param}` segments), a name from `operationId`;
//! - the success answer: the lowest 2xx status (`2XX` is 200, a lone `default` is 200) and its
//!   JSON body: the media type's example, else its first `examples:` entry, else a body made
//!   from the response schema. The body never holds a random value. Where the schema allows it,
//!   top-level fields become templates: the resource's `id` is `{seq}` (integers), `{uuid}`
//!   (`format: uuid`) or `<prefix>{seq}` (an example such as `tr_123`) on a create, and
//!   `{path:<param>}` on a route about one resource; a field named as a required field of the
//!   request is `{json:x}` (JSON) or `{form:x}` (form-encoded), one named as a path or required
//!   query parameter `{path:x}` or `{query:x}`;
//! - with `--stateful`, create-then-read pairs (POST /things answers an id; GET, PUT, PATCH,
//!   DELETE /things/{id} and POST /things/{id}/...) get `save:` and `load:`, so a read answers what
//!   the create stored and an unknown id is answered 404;
//! - a header parameter whose name holds `idempotency` gives the fake `idempotency:` and the
//!   routes that declare it `idempotent: true`;
//! - the error answers the spec lists (4xx, 5xx, `4XX`, `5XX`, `default`) go into the route's
//!   `errors:`, which hydo-world answers when an `H=` rule `status=<code>` hits the route.
//!
//! What it refuses, with one line each: Swagger 2.0, an operation with `callbacks:`, a `oneOf`
//! without a discriminator where a body needs it, a binary request or answer body
//! (`application/octet-stream`, `format: binary`, images, `multipart/form-data`), an XML-only
//! answer, a `$ref` to another file, and a path parameter inside a segment (`/files/{name}.json`).

use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

pub const TOOL: &str = "hydo fake";

pub const USAGE: &str =
    "hydo fake from-openapi: write a fake outside service (a `fakes:` entry of hydo.yaml) from an
OpenAPI 3.0 or 3.1 description (YAML or JSON). `hydo guide outside-services` has the details.

    hydo fake from-openapi <spec.yaml|spec.json> [--name NAME] [--port PORT] [--ops OP,...]
                           [--stateful] [--ref EXTRACTOR] [--tls] [--base-path PATH]
                           [-o FILE | --add-to hydo.yaml]

  --name NAME       the fake's name, which H= tokens and checks use (default: from info.title)
  --port PORT       the port the system calls on the fake's address (default 9001)
  --ops OP,...      only these operations (operationId, or the route name it gets)
  --stateful        save: and load: for create-then-read pairs (POST /things, GET /things/{id})
  --ref EXTRACTOR   the business reference of a request, which the checks group by (json:order,
                    form:x, header:X, query:x, path:x)
  --tls             tls: {hosts: [<the first server's host>]}, for an SDK that calls HTTPS
  --base-path PATH  the path before every operation's path (default: the first server's)
  -o FILE           write the entry to FILE (default: print it)
  --add-to FILE     add the entry to the fakes: of that hydo.yaml

One route per operation, with the spec's example or a body made from the response schema (no
random values; {seq}, {uuid}, {path:x}, {json:x}, {form:x} and {obj:x} templates where the schema
allows), the idempotency header the spec declares, and the spec's error answers in errors:, which
an H= rule status=<code> answers. Operations it cannot map are listed on stderr, one line each:
callbacks, a oneOf without a discriminator, binary bodies, XML-only answers, $ref to other files,
path parameters inside a segment.";

/// What `generate` takes besides the spec.
#[derive(Clone, Debug, Default)]
pub struct Opts {
    pub name: Option<String>,
    pub port: Option<i64>,
    /// Only these operations (operationId or route name).
    pub ops: Option<Vec<String>>,
    pub stateful: bool,
    pub tls: bool,
    pub base_path: Option<String>,
    /// The fake's `ref:` (an extractor such as json:order), which the checks group by.
    pub reference: Option<String>,
    /// How the output's first comment names the spec (the file's name and the command's flags).
    pub command: String,
    /// The sha256 of the spec's bytes, for the comment.
    pub sha256: String,
}

/// The generated fake.
#[derive(Clone, Debug)]
pub struct Generated {
    /// The fake as data (what hydo.yaml's `fakes:` list holds).
    pub fake: Value,
    /// The comment lines above the entry (without `# `).
    pub header: Vec<String>,
    /// One line per operation it did not map: `<op> (<METHOD> <path>): <why>`.
    pub refused: Vec<String>,
    /// Operations of `--ops` that it refused or did not find.
    pub refused_named: Vec<String>,
}

// ------------------------------------------------------------------------------- reading

/// The spec's text as data: JSON when it starts with `{`, else YAML (1.2 core schema; anchors,
/// aliases and `<<` merge keys).
pub fn load_spec(text: &str) -> Result<Value, String> {
    if text.trim_start().starts_with('{') {
        return serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"));
    }
    yaml_load(text)
}

enum Frame {
    Seq(Vec<Value>, Option<String>),
    Map(Map<String, Value>, Option<String>, Option<String>),
}

fn yaml_load(text: &str) -> Result<Value, String> {
    use libyaml_safer::{EventData, Parser, ScalarStyle};
    let mut input = text.as_bytes();
    let mut p = Parser::new();
    p.set_input_string(&mut input);
    let mut anchors: HashMap<String, Value> = HashMap::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut root: Option<Value> = None;
    let mut docs = 0;
    fn put(
        stack: &mut [Frame],
        root: &mut Option<Value>,
        v: Value,
        anchors: &mut HashMap<String, Value>,
        anchor: Option<String>,
    ) -> Result<(), String> {
        if let Some(a) = anchor {
            anchors.insert(a, v.clone());
        }
        match stack.last_mut() {
            None => *root = Some(v),
            Some(Frame::Seq(items, _)) => items.push(v),
            Some(Frame::Map(m, _, key)) => match key.take() {
                None => {
                    *key = Some(match v {
                        Value::String(s) => s,
                        Value::Null => "null".into(),
                        Value::Bool(b) => b.to_string(),
                        Value::Number(n) => n.to_string(),
                        _ => return Err("a mapping key that is not a scalar".into()),
                    })
                }
                Some(k) if k == "<<" => match v {
                    Value::Object(o) => {
                        for (k2, v2) in o {
                            m.entry(k2).or_insert(v2);
                        }
                    }
                    Value::Array(a) => {
                        for x in a {
                            if let Value::Object(o) = x {
                                for (k2, v2) in o {
                                    m.entry(k2).or_insert(v2);
                                }
                            }
                        }
                    }
                    _ => return Err("a << merge key needs a mapping".into()),
                },
                Some(k) => {
                    m.insert(k, v);
                }
            },
        }
        Ok(())
    }
    for ev in p {
        let ev = ev.map_err(|e| format!("not valid YAML: {e}"))?;
        match ev.data {
            EventData::DocumentStart { .. } => {
                docs += 1;
                if docs > 1 {
                    return Err("more than one YAML document".into());
                }
            }
            EventData::Scalar {
                anchor,
                tag,
                value,
                style,
                ..
            } => {
                let numeric = matches!(
                    tag.as_deref(),
                    Some("tag:yaml.org,2002:int") | Some("tag:yaml.org,2002:float")
                );
                let v = if (style == ScalarStyle::Plain && tag.is_none()) || numeric {
                    plain_scalar(&value)
                } else {
                    Value::String(value)
                };
                put(&mut stack, &mut root, v, &mut anchors, anchor)?;
            }
            EventData::Alias { anchor } => {
                let v = anchors
                    .get(&anchor)
                    .cloned()
                    .ok_or_else(|| format!("an alias *{anchor} without its anchor"))?;
                put(&mut stack, &mut root, v, &mut anchors, None)?;
            }
            EventData::SequenceStart { anchor, .. } => stack.push(Frame::Seq(Vec::new(), anchor)),
            EventData::MappingStart { anchor, .. } => {
                stack.push(Frame::Map(Map::new(), anchor, None))
            }
            EventData::SequenceEnd => {
                if let Some(Frame::Seq(items, anchor)) = stack.pop() {
                    put(
                        &mut stack,
                        &mut root,
                        Value::Array(items),
                        &mut anchors,
                        anchor,
                    )?;
                }
            }
            EventData::MappingEnd => {
                if let Some(Frame::Map(m, anchor, _)) = stack.pop() {
                    put(
                        &mut stack,
                        &mut root,
                        Value::Object(m),
                        &mut anchors,
                        anchor,
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok(root.unwrap_or(Value::Null))
}

/// A plain scalar of YAML 1.2's core schema.
fn plain_scalar(s: &str) -> Value {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    let digits = s.strip_prefix(['-', '+']).unwrap_or(s);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(i) = s.parse::<i64>() {
            return Value::Number(i.into());
        }
        if let Ok(u) = s.trim_start_matches('+').parse::<u64>() {
            return Value::Number(u.into());
        }
    }
    if let Some(h) = s.strip_prefix("0x") {
        if let Ok(i) = i64::from_str_radix(h, 16) {
            return Value::Number(i.into());
        }
    }
    let float_like = !digits.is_empty()
        && digits
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'-' | b'+'))
        && digits.bytes().any(|b| b.is_ascii_digit())
        && digits.as_bytes()[0] != b'e'
        && digits.as_bytes()[0] != b'E';
    if float_like {
        if let Ok(f) = s.parse::<f64>() {
            if let Some(n) = Number::from_f64(f) {
                return Value::Number(n);
            }
        }
    }
    Value::String(s.to_string())
}

// ------------------------------------------------------------------------------- the spec

/// Why an operation (or a body of it) is not mapped.
type Refusal = String;

struct Spec<'a> {
    root: &'a Value,
}

impl<'a> Spec<'a> {
    /// `v` with its `$ref` chain followed (local references only).
    fn deref(&self, v: &'a Value) -> Result<&'a Value, Refusal> {
        let mut cur = v;
        for _ in 0..64 {
            let Some(r) = cur.get("$ref").and_then(Value::as_str) else {
                return Ok(cur);
            };
            let Some(ptr) = r.strip_prefix('#') else {
                return Err(format!("a $ref to another file ({r}) is not followed"));
            };
            let mut at = self.root;
            for part in ptr.split('/').skip(1) {
                let key = pct_decode(&part.replace("~1", "/").replace("~0", "~"));
                at = match at {
                    Value::Object(o) => o.get(&key),
                    Value::Array(a) => key.parse::<usize>().ok().and_then(|i| a.get(i)),
                    _ => None,
                }
                .ok_or_else(|| format!("$ref {r} names nothing in the spec"))?;
            }
            cur = at;
        }
        Err("a $ref chain longer than 64".into())
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Where a field of the request comes from, for an echo template.
#[derive(Clone, Debug, PartialEq)]
enum Src {
    Json,
    Form,
    Query,
    Path,
}

/// A scalar the answer can echo: its source, and whether it is written without quotes.
#[derive(Clone, Debug)]
struct Echo {
    src: Src,
    raw: bool,
}

/// One operation as read from the spec.
struct Op {
    id: Option<String>,
    method: String,
    /// the operation's path as the spec writes it
    path: String,
    /// the route's path (base path + path)
    route_path: String,
    summary: String,
    route: Map<String, Value>,
    /// the idempotency header it declares
    idempotency: Option<String>,
    /// the success answer before overlays, and its schema (when it is JSON)
    success: Option<(Value, Option<Value>)>,
    /// the `$ref` of the success answer's schema (two routes that answer the same schema are about
    /// the same kind of resource)
    schema_ref: Option<String>,
    echo: BTreeMap<String, Echo>,
}

fn is_json(mt: &str) -> bool {
    let m = mt
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    m == "application/json" || m.ends_with("+json") || m == "*/*" || m == "application/*"
}

fn is_binary_type(mt: &str) -> bool {
    let m = mt
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    m == "application/octet-stream"
        || m == "application/pdf"
        || m == "application/zip"
        || m.starts_with("image/")
        || m.starts_with("audio/")
        || m.starts_with("video/")
        || m == "multipart/form-data"
        || m == "multipart/mixed"
}

/// The media type to use of a `content:` map: JSON first, then form, then text; binary and XML
/// are refused when nothing else is there.
fn pick_media<'a>(
    content: &'a Map<String, Value>,
    what: &str,
) -> Result<Option<(&'a str, &'a Value)>, Refusal> {
    if content.is_empty() {
        return Ok(None);
    }
    let order: [&dyn Fn(&str) -> bool; 3] = [
        &|m: &str| is_json(m),
        &|m: &str| m.starts_with("application/x-www-form-urlencoded"),
        &|m: &str| m.starts_with("text/"),
    ];
    for f in order {
        if let Some((k, v)) = content.iter().find(|(k, _)| f(&k.to_ascii_lowercase())) {
            return Ok(Some((k.as_str(), v)));
        }
    }
    let first = content.keys().next().map(String::as_str).unwrap_or("");
    if is_binary_type(first) {
        if first.starts_with("multipart/") {
            return Err(format!(
                "{what} is {first} (an upload), which hydo's fakes do not parse"
            ));
        }
        return Err(format!("{what} is binary ({first})"));
    }
    Err(format!("{what} is {first}, not JSON, form-encoded or text"))
}

fn types_of(s: &Value) -> Vec<String> {
    match s.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// The schema's one type that is not null ("" when it names none).
fn main_type(s: &Value) -> String {
    let ts = types_of(s);
    if let Some(t) = ts.iter().find(|t| *t != "null") {
        return t.clone();
    }
    if s.get("properties").is_some() || s.get("additionalProperties").is_some() {
        return "object".into();
    }
    if s.get("items").is_some() {
        return "array".into();
    }
    if ts.iter().any(|t| t == "null") {
        return "null".into();
    }
    String::new()
}

fn is_binary_schema(s: &Value) -> bool {
    main_type(s) == "string"
        && (s.get("format").and_then(Value::as_str) == Some("binary")
            || s.get("contentMediaType")
                .and_then(Value::as_str)
                .is_some_and(is_binary_type))
}

// ------------------------------------------------------------------------------- bodies

/// Makes a value from a schema, the same every time.
struct Maker<'a, 'b> {
    spec: &'b Spec<'a>,
    /// `$ref`s being expanded (a recursive schema stops at its second visit)
    stack: Vec<String>,
}

/// How deep optional properties are still filled in (0: the answer's own fields).
const OPTIONAL_DEPTH: usize = 2;
/// How deep arrays get an item.
const ITEM_DEPTH: usize = 2;

impl<'a, 'b> Maker<'a, 'b> {
    fn value(&mut self, s: &'a Value, depth: usize) -> Result<Value, Refusal> {
        if depth > 12 {
            return Ok(Value::Null);
        }
        let reference = s.get("$ref").and_then(Value::as_str).map(str::to_string);
        if let Some(r) = &reference {
            if self.stack.contains(r) {
                return Ok(Value::Null);
            }
            self.stack.push(r.clone());
        }
        let out = self.value_of(self.spec.deref(s)?, depth);
        if reference.is_some() {
            self.stack.pop();
        }
        out
    }

    fn value_of(&mut self, s: &'a Value, depth: usize) -> Result<Value, Refusal> {
        if let Some(x) = s.get("example") {
            return Ok(x.clone());
        }
        if let Some(Value::Array(a)) = s.get("examples") {
            if let Some(x) = a.first() {
                return Ok(x.clone());
            }
        }
        if let Some(x) = s.get("const") {
            return Ok(x.clone());
        }
        if let Some(Value::Array(e)) = s.get("enum") {
            if let Some(x) = e.iter().find(|x| !x.is_null()).or(e.first()) {
                return Ok(x.clone());
            }
        }
        if let Some(Value::Array(parts)) = s.get("allOf") {
            let mut merged = Map::new();
            let mut other: Option<Value> = None;
            for p in parts {
                match self.value(p, depth)? {
                    Value::Object(o) => {
                        for (k, v) in o {
                            merged.insert(k, v);
                        }
                    }
                    Value::Null => {}
                    v => {
                        other.get_or_insert(v);
                    }
                }
            }
            if let Some(Value::Object(_)) = s.get("properties") {
                if let Value::Object(o) = self.object(s, depth)? {
                    for (k, v) in o {
                        merged.insert(k, v);
                    }
                }
            }
            return Ok(match other {
                Some(v) if merged.is_empty() => v,
                _ => Value::Object(merged),
            });
        }
        for kind in ["oneOf", "anyOf"] {
            let Some(Value::Array(alts)) = s.get(kind) else {
                continue;
            };
            return self.alternative(s, kind, alts, depth);
        }
        if s.get("default").is_some() && main_type(s) != "object" && main_type(s) != "array" {
            return Ok(s["default"].clone());
        }
        match main_type(s).as_str() {
            "object" => self.object(s, depth),
            "array" => {
                let min = s.get("minItems").and_then(Value::as_u64).unwrap_or(0);
                if depth > ITEM_DEPTH && min == 0 {
                    return Ok(Value::Array(Vec::new()));
                }
                match s.get("items") {
                    Some(it) if it.is_object() => {
                        let v = self.value(it, depth + 1)?;
                        Ok(Value::Array(if v.is_null() && min == 0 {
                            Vec::new()
                        } else {
                            vec![v]
                        }))
                    }
                    _ => Ok(Value::Array(Vec::new())),
                }
            }
            "string" => self.string(s),
            "integer" | "number" => {
                let mut n = s.get("minimum").and_then(Value::as_f64).unwrap_or(0.0);
                if let Some(x) = s.get("exclusiveMinimum").and_then(Value::as_f64) {
                    n = n.max(x + 1.0);
                } else if s.get("exclusiveMinimum") == Some(&Value::Bool(true)) {
                    n += 1.0;
                }
                if let Some(mx) = s.get("maximum").and_then(Value::as_f64) {
                    if n > mx {
                        n = mx;
                    }
                }
                if main_type(s) == "integer" || n.fract() == 0.0 {
                    Ok(Value::Number((n as i64).into()))
                } else {
                    Ok(Number::from_f64(n)
                        .map(Value::Number)
                        .unwrap_or(Value::Null))
                }
            }
            "boolean" => Ok(Value::Bool(false)),
            _ => Ok(Value::Null),
        }
    }

    fn alternative(
        &mut self,
        s: &'a Value,
        kind: &str,
        alts: &'a [Value],
        depth: usize,
    ) -> Result<Value, Refusal> {
        let non_null: Vec<&'a Value> = alts
            .iter()
            .filter(|a| {
                self.spec
                    .deref(a)
                    .map(|d| !(main_type(d) == "null" && d.get("properties").is_none()))
                    .unwrap_or(true)
            })
            .collect();
        if let Some(d) = s.get("discriminator") {
            let prop = d
                .get("propertyName")
                .and_then(Value::as_str)
                .unwrap_or("type")
                .to_string();
            // the first mapping entry, else the first alternative named by its schema
            let (val, chosen): (String, &'a Value) = match d.get("mapping") {
                Some(Value::Object(m)) if !m.is_empty() => {
                    let (k, r) = m.iter().next().expect("a mapping entry");
                    let target = r.as_str().unwrap_or("");
                    let chosen = non_null
                        .iter()
                        .copied()
                        .find(|a| a.get("$ref").and_then(Value::as_str) == Some(target))
                        .or(non_null.first().copied())
                        .ok_or_else(|| format!("{kind} without alternatives"))?;
                    (k.clone(), chosen)
                }
                _ => {
                    let chosen = *non_null
                        .first()
                        .ok_or_else(|| format!("{kind} without alternatives"))?;
                    let name = chosen
                        .get("$ref")
                        .and_then(Value::as_str)
                        .and_then(|r| r.rsplit('/').next())
                        .unwrap_or("")
                        .to_string();
                    (name, chosen)
                }
            };
            let mut v = self.value(chosen, depth)?;
            if let Value::Object(o) = &mut v {
                o.insert(prop, Value::String(val));
            }
            return Ok(v);
        }
        if kind == "anyOf" || non_null.len() == 1 {
            return match non_null.first() {
                Some(a) => self.value(a, depth),
                None => Ok(Value::Null),
            };
        }
        Err(format!(
            "a oneOf without a discriminator ({} alternatives), so no one answer is right",
            non_null.len()
        ))
    }

    fn object(&mut self, s: &'a Value, depth: usize) -> Result<Value, Refusal> {
        let required: BTreeSet<&str> = match s.get("required") {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
            _ => BTreeSet::new(),
        };
        let mut o = Map::new();
        if let Some(Value::Object(props)) = s.get("properties") {
            for (k, ps) in props {
                let req = required.contains(k.as_str());
                let d = self.spec.deref(ps)?;
                if d.get("writeOnly") == Some(&Value::Bool(true)) {
                    continue;
                }
                if !req && depth >= OPTIONAL_DEPTH {
                    continue;
                }
                if is_binary_schema(d) {
                    if req {
                        return Err(format!("the field {k} is binary"));
                    }
                    continue;
                }
                match self.value(ps, depth + 1) {
                    Ok(v) => {
                        o.insert(k.clone(), v);
                    }
                    Err(e) if req => return Err(format!("{k}: {e}")),
                    Err(_) => {} // an optional field it cannot make is left out
                }
            }
        }
        Ok(Value::Object(o))
    }

    fn string(&mut self, s: &'a Value) -> Result<Value, Refusal> {
        if is_binary_schema(s) {
            return Err("a binary string (format: binary)".into());
        }
        let f = s.get("format").and_then(Value::as_str).unwrap_or("");
        let v = match f {
            "date-time" => "2026-01-01T00:00:00Z",
            "date" => "2026-01-01",
            "time" => "00:00:00",
            "uuid" => "00000000-0000-4000-8000-000000000000",
            "email" => "user@example.com",
            "uri" | "url" | "uri-reference" | "iri" => "https://example.com/",
            "hostname" => "example.com",
            "ipv4" => "192.0.2.1",
            "ipv6" => "2001:db8::1",
            "byte" => "",
            _ => "string",
        };
        let mut v = v.to_string();
        let min = s.get("minLength").and_then(Value::as_u64).unwrap_or(0) as usize;
        let max = s
            .get("maxLength")
            .and_then(Value::as_u64)
            .map(|x| x as usize);
        while v.len() < min {
            v.push('x');
        }
        if let Some(mx) = max {
            v.truncate(mx.max(min));
        }
        Ok(Value::String(v))
    }
}

// ------------------------------------------------------------------------------- templates

/// A JSON body with templates: literal JSON, or a template written without quotes (a number from
/// the request) or inside quotes (a string).
#[derive(Clone, Debug)]
enum T {
    Lit(Value),
    Raw(String),
    Quoted(String),
    Obj(Vec<(String, T)>),
}

fn render(t: &T, out: &mut String) {
    match t {
        T::Lit(v) => out.push_str(&serde_json::to_string(v).unwrap_or_default()),
        T::Raw(s) => out.push_str(s),
        T::Quoted(s) => {
            out.push('"');
            out.push_str(s);
            out.push('"');
        }
        T::Obj(fields) => {
            out.push('{');
            for (i, (k, v)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                render(v, out);
            }
            out.push('}');
        }
    }
}

/// The template of a field: raw for numbers and booleans, quoted for strings.
fn tmpl(name: &str, raw: bool) -> T {
    if raw {
        T::Raw(name.to_string())
    } else {
        T::Quoted(name.to_string())
    }
}

fn is_raw_type(t: &str) -> bool {
    matches!(t, "integer" | "number" | "boolean")
}

/// The template of a resource's new id, by its schema and example: `{seq}` (an integer), `{uuid}`,
/// `<prefix>{seq}` (an example such as `tr_123` or `pi-9`), else `{seq}` as a string.
fn new_id(schema: Option<&Value>, example: Option<&Value>) -> T {
    let ty = schema.map(main_type).unwrap_or_default();
    if ty == "integer" || ty == "number" || matches!(example, Some(Value::Number(_))) {
        return T::Raw("{seq}".into());
    }
    if schema.and_then(|s| s.get("format")).and_then(Value::as_str) == Some("uuid") {
        return T::Quoted("{uuid}".into());
    }
    if let Some(Value::String(e)) = example.or_else(|| schema.and_then(|s| s.get("example"))) {
        let pre: String = e
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect::<String>();
        let next = e.chars().nth(pre.len());
        if !pre.is_empty() && matches!(next, Some('_') | Some('-')) {
            return T::Quoted(format!("{pre}{}{{seq}}", next.unwrap_or('_')));
        }
        if e.len() == 36 && e.chars().filter(|c| *c == '-').count() == 4 {
            return T::Quoted("{uuid}".into());
        }
    }
    T::Quoted("{seq}".into())
}

/// What an answer's top-level fields become.
#[derive(Default)]
struct Overlay {
    /// the id field and its template
    id: Option<(String, T)>,
    /// field -> (template, raw)
    fields: BTreeMap<String, T>,
}

fn overlay(v: &Value, ov: &Overlay) -> T {
    match v {
        Value::Object(o) => T::Obj(
            o.iter()
                .map(|(k, x)| {
                    if let Some((idk, t)) = &ov.id {
                        if idk == k {
                            return (k.clone(), t.clone());
                        }
                    }
                    match ov.fields.get(k) {
                        Some(t) => (k.clone(), t.clone()),
                        None => (k.clone(), T::Lit(x.clone())),
                    }
                })
                .collect(),
        ),
        _ => T::Lit(v.clone()),
    }
}

// ------------------------------------------------------------------------------- names

/// `createTransfer` -> `create_transfer`; `get-transfers` -> `get_transfers`.
fn snake(s: &str) -> String {
    let mut out = String::new();
    let cs: Vec<char> = s.chars().collect();
    for (i, &c) in cs.iter().enumerate() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() {
                let prev = if i > 0 { cs[i - 1] } else { ' ' };
                let next = cs.get(i + 1).copied().unwrap_or(' ');
                if i > 0
                    && (prev.is_ascii_lowercase()
                        || prev.is_ascii_digit()
                        || (prev.is_ascii_uppercase() && next.is_ascii_lowercase()))
                    && !out.ends_with('_')
                {
                    out.push('_');
                }
            }
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let mut out = out.trim_matches('_').to_string();
    if out.is_empty() || !out.starts_with(|c: char| c.is_ascii_lowercase()) {
        out = format!("op_{out}");
    }
    out
}

/// A route name for an operation without an operationId: `get_pets_by_id`.
fn name_of_path(method: &str, path: &str) -> String {
    let mut parts = vec![method.to_string()];
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        if let Some(p) = seg.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
            parts.push(format!("by_{p}"));
        } else {
            parts.push(seg.to_string());
        }
    }
    snake(&parts.join("_"))
}

// ------------------------------------------------------------------------------- the mapping

/// The success response: (status, the response object), and whether it was `default`.
fn success_of<'a>(responses: &'a Map<String, Value>) -> Option<(i64, &'a Value, &'a str)> {
    let mut best: Option<(i64, &'a Value, &'a str)> = None;
    for (k, v) in responses {
        let code = if k.eq_ignore_ascii_case("2XX") {
            Some(200)
        } else {
            k.parse::<i64>().ok().filter(|c| (200..300).contains(c))
        };
        if let Some(c) = code {
            if best.is_none_or(|b| c < b.0) {
                best = Some((c, v, k.as_str()));
            }
        }
    }
    if best.is_none() {
        if let Some(v) = responses.get("default") {
            return Some((200, v, "default"));
        }
    }
    best
}

fn is_error_key(k: &str) -> bool {
    k == "default"
        || k.eq_ignore_ascii_case("4XX")
        || k.eq_ignore_ascii_case("5XX")
        || k.parse::<i64>().is_ok_and(|c| (400..600).contains(&c))
}

/// The base path of the first server: its URL's path, server variables at their defaults.
fn base_path(spec: &Value) -> String {
    let Some(s) = spec.get("servers").and_then(|x| x.get(0)) else {
        return String::new();
    };
    let mut url = s
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if let Some(Value::Object(vars)) = s.get("variables") {
        for (k, v) in vars {
            let d = v.get("default").map(scalar_text).unwrap_or_default();
            url = url.replace(&format!("{{{k}}}"), &d);
        }
    }
    let path = match url.find("://") {
        Some(i) => {
            let rest = &url[i + 3..];
            rest.find('/').map(|j| &rest[j..]).unwrap_or("")
        }
        None => url.as_str(),
    };
    path.trim_end_matches('/').to_string()
}

/// The first server's host name, for `--tls`.
fn server_host(spec: &Value) -> Option<String> {
    let url = spec
        .get("servers")?
        .get(0)?
        .get("url")?
        .as_str()?
        .to_string();
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', ':']).next()?.to_ascii_lowercase();
    (!host.is_empty() && !host.contains('{')).then_some(host)
}

fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        x => x.to_string(),
    }
}

/// The fake's default name, from the spec's title: [a-z][a-z0-9_]*, at most 24 characters.
fn default_name(spec: &Value) -> String {
    let t = spec
        .get("info")
        .and_then(|i| i.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("api");
    let mut n = snake(t);
    if n.starts_with("op_") {
        n = format!("api_{}", &n[3..]);
    }
    n.truncate(24);
    n.trim_end_matches('_').to_string()
}

/// The parameters of an operation, its path's first (an operation's own win by name and place).
fn params<'a>(sp: &Spec<'a>, item: &'a Value, op: &'a Value) -> Result<Vec<&'a Value>, Refusal> {
    let mut out: Vec<&'a Value> = Vec::new();
    for src in [item.get("parameters"), op.get("parameters")] {
        if let Some(Value::Array(a)) = src {
            for p in a {
                let p = sp.deref(p)?;
                let (n, i) = (p.get("name"), p.get("in"));
                out.retain(|q| !(q.get("name") == n && q.get("in") == i));
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// Maps one operation; Err is the refusal.
fn map_op<'a>(
    sp: &Spec<'a>,
    path: &str,
    method: &str,
    item: &'a Value,
    op: &'a Value,
    base: &str,
) -> Result<Op, Refusal> {
    if op
        .get("callbacks")
        .is_some_and(|c| c.as_object().is_some_and(|o| !o.is_empty()))
    {
        return Err(
            "it has callbacks: (the provider calls the system back); give the route a webhook: by hand"
                .into(),
        );
    }
    for seg in path.split('/') {
        if seg.contains('{')
            && !(seg.starts_with('{') && seg.ends_with('}') && seg.matches('{').count() == 1)
        {
            return Err(format!(
                "a path parameter inside the segment {seg}; hydo's routes match whole segments"
            ));
        }
    }
    let route_path = format!("{base}{path}");
    let mut echo: BTreeMap<String, Echo> = BTreeMap::new();
    let mut idempotency = None;
    for p in params(sp, item, op)? {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("");
        let inn = p.get("in").and_then(Value::as_str).unwrap_or("");
        let schema = match p.get("schema") {
            Some(s) => Some(sp.deref(s)?),
            None => None,
        };
        let raw = schema.map(main_type).is_some_and(|t| is_raw_type(&t));
        match inn {
            "header" if name.to_ascii_lowercase().contains("idempotency") => {
                idempotency = Some(name.to_string());
            }
            "path" => {
                echo.insert(
                    name.to_string(),
                    Echo {
                        src: Src::Path,
                        raw,
                    },
                );
            }
            "query" if p.get("required") == Some(&Value::Bool(true)) => {
                echo.entry(name.to_string()).or_insert(Echo {
                    src: Src::Query,
                    raw,
                });
            }
            _ => {}
        }
    }
    // the request body: its required scalar fields can be echoed
    if let Some(rb) = op.get("requestBody") {
        let rb = sp.deref(rb)?;
        if let Some(Value::Object(content)) = rb.get("content") {
            if let Some((mt, media)) = pick_media(content, "the request body")? {
                let src = if is_json(mt) {
                    Some(Src::Json)
                } else if mt.starts_with("application/x-www-form-urlencoded") {
                    Some(Src::Form)
                } else {
                    None
                };
                if let (Some(src), Some(s)) = (src, media.get("schema")) {
                    let s = sp.deref(s)?;
                    if is_binary_schema(s) {
                        return Err(format!("the request body is binary ({mt})"));
                    }
                    let req: BTreeSet<&str> = match s.get("required") {
                        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
                        _ => BTreeSet::new(),
                    };
                    if let Some(Value::Object(props)) = s.get("properties") {
                        for (k, ps) in props {
                            let d = sp.deref(ps)?;
                            let t = main_type(d);
                            if is_binary_schema(d) && req.contains(k.as_str()) {
                                return Err(format!("the request body's field {k} is binary"));
                            }
                            if req.contains(k.as_str())
                                && (t == "string" || is_raw_type(&t))
                                && d.get("format").and_then(Value::as_str) != Some("binary")
                                && !echo.contains_key(k)
                            {
                                echo.insert(
                                    k.clone(),
                                    Echo {
                                        src: src.clone(),
                                        raw: is_raw_type(&t),
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    let Some(Value::Object(responses)) = op.get("responses") else {
        return Err("it has no responses:".into());
    };
    let mut route = Map::new();
    let mut success = None;
    let mut schema_ref = None;
    if let Some((code, resp, key)) = success_of(responses) {
        let resp = sp.deref(resp)?;
        route.insert("status".into(), Value::Number(code.into()));
        if let Some(Value::Object(content)) = resp.get("content") {
            match pick_media(content, "the answer")? {
                Some((mt, media)) if is_json(mt) => {
                    let (v, schema) = body_value(sp, media)?;
                    schema_ref = media
                        .get("schema")
                        .and_then(|s| s.get("$ref"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if !mt.eq_ignore_ascii_case("application/json") && mt != "*/*" {
                        route.insert("headers".into(), content_type(mt));
                    }
                    success = Some((v, schema));
                }
                Some((mt, media)) => {
                    let text = media
                        .get("example")
                        .map(scalar_text)
                        .or_else(|| first_example(sp, media).map(|v| scalar_text(&v)))
                        .unwrap_or_default();
                    route.insert("headers".into(), content_type(mt));
                    route.insert("body".into(), Value::String(text));
                }
                None => {}
            }
        }
        // a lone default is the success answer, not an error answer
        let mut errors = Map::new();
        for (k, v) in responses {
            if !is_error_key(k) || k == key {
                continue;
            }
            let r = sp.deref(v)?;
            let body = match r.get("content") {
                Some(Value::Object(c)) => match pick_media(c, "an error answer") {
                    Ok(Some((mt, media))) if is_json(mt) => match body_value(sp, media) {
                        Ok((v, _)) => serde_json::to_string(&v).unwrap_or_default(),
                        Err(_) => continue,
                    },
                    Ok(Some((_, media))) => {
                        media.get("example").map(scalar_text).unwrap_or_default()
                    }
                    Ok(None) => String::new(),
                    Err(_) => continue,
                },
                _ => String::new(),
            };
            let key = if k == "default" {
                "default".to_string()
            } else {
                k.to_ascii_uppercase()
            };
            errors.insert(key, Value::String(body));
        }
        if !errors.is_empty() {
            route.insert("errors".into(), Value::Object(errors));
        }
    } else {
        return Err("it has no 2xx or default answer".into());
    }
    let summary = op
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(Op {
        id: op
            .get("operationId")
            .and_then(Value::as_str)
            .map(str::to_string),
        method: method.to_ascii_uppercase(),
        path: path.to_string(),
        route_path,
        summary,
        route,
        idempotency,
        success,
        schema_ref,
        echo,
    })
}

fn content_type(mt: &str) -> Value {
    let mut h = Map::new();
    h.insert("Content-Type".into(), Value::String(mt.to_string()));
    Value::Object(h)
}

fn first_example<'a>(sp: &Spec<'a>, media: &'a Value) -> Option<Value> {
    let Some(Value::Object(ex)) = media.get("examples") else {
        return None;
    };
    for e in ex.values() {
        if let Ok(d) = sp.deref(e) {
            if let Some(v) = d.get("value") {
                return Some(v.clone());
            }
        }
    }
    None
}

/// The body of a JSON media type: its example, its first examples: entry, else one made from
/// its schema; and the schema (dereferenced) when there is one.
fn body_value<'a>(sp: &Spec<'a>, media: &'a Value) -> Result<(Value, Option<Value>), Refusal> {
    let schema = match media.get("schema") {
        Some(s) => Some(sp.deref(s)?),
        None => None,
    };
    if let Some(s) = schema {
        if is_binary_schema(s) {
            return Err("the answer is binary (format: binary)".into());
        }
    }
    if let Some(x) = media.get("example") {
        return Ok((x.clone(), schema.cloned()));
    }
    if let Some(x) = first_example(sp, media) {
        return Ok((x, schema.cloned()));
    }
    let Some(s) = media.get("schema") else {
        return Ok((Value::Object(Map::new()), None));
    };
    let mut mk = Maker {
        spec: sp,
        stack: Vec::new(),
    };
    let mut v = mk.value(s, 0)?;
    // a very large answer keeps its optional fields at the top level only (it starts one level
    // down, so its own fields are filled and theirs only when required)
    if serde_json::to_string(&v).map(|t| t.len()).unwrap_or(0) > 16 * 1024 {
        v = mk.value(s, OPTIONAL_DEPTH - 1)?;
    }
    Ok((v, schema.cloned()))
}

/// The top-level properties of an answer schema (allOf parts merged): name -> schema.
fn top_props<'a>(sp: &Spec<'a>, s: &'a Value) -> BTreeMap<String, &'a Value> {
    let mut out = BTreeMap::new();
    let Ok(s) = sp.deref(s) else { return out };
    if let Some(Value::Object(p)) = s.get("properties") {
        for (k, v) in p {
            if let Ok(d) = sp.deref(v) {
                out.insert(k.clone(), d);
            }
        }
    }
    if let Some(Value::Array(parts)) = s.get("allOf") {
        for p in parts {
            for (k, v) in top_props(sp, p) {
                out.entry(k).or_insert(v);
            }
        }
    }
    out
}

/// The segments of a route path, a parameter as None.
fn segs(p: &str) -> Vec<Option<&str>> {
    p.trim_matches('/')
        .split('/')
        .map(|s| {
            if s.starts_with('{') && s.ends_with('}') {
                None
            } else {
                Some(s)
            }
        })
        .collect()
}

fn param_at(p: &str, i: usize) -> Option<String> {
    p.trim_matches('/')
        .split('/')
        .nth(i)
        .and_then(|s| s.strip_prefix('{'))
        .and_then(|s| s.strip_suffix('}'))
        .map(str::to_string)
}

/// Writes the routes of the mapped operations.
pub fn generate(spec: &Value, o: &Opts) -> Result<Generated, String> {
    if spec.get("swagger").is_some() {
        return Err(
            "this is a Swagger 2.0 description; convert it to OpenAPI 3 first (swagger2openapi, or the editor of your API tool)"
                .into(),
        );
    }
    let ver = spec.get("openapi").map(scalar_text).unwrap_or_default();
    if !(ver.starts_with("3.0") || ver.starts_with("3.1")) {
        return Err(format!(
            "openapi: {}: only OpenAPI 3.0 and 3.1 descriptions are read",
            if ver.is_empty() { "missing" } else { &ver }
        ));
    }
    let sp = Spec { root: spec };
    let base = match &o.base_path {
        Some(b) => b.trim_end_matches('/').to_string(),
        None => base_path(spec),
    };
    let name = o.name.clone().unwrap_or_else(|| default_name(spec));
    if !name.starts_with(|c: char| c.is_ascii_lowercase())
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(format!(
            "--name {name}: a fake's name is [a-z][a-z0-9_]* (H= tokens name it)"
        ));
    }
    let Some(Value::Object(paths)) = spec.get("paths") else {
        return Err("the spec has no paths:".into());
    };
    let mut ops: Vec<Op> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    let mut refused_ids: Vec<(Option<String>, String)> = Vec::new();
    let mut used_names: BTreeSet<String> = BTreeSet::new();
    let mut names: Vec<String> = Vec::new();
    for (path, item) in paths {
        let item = match sp.deref(item) {
            Ok(i) => i,
            Err(e) => {
                refused.push(format!("{path}: {e}"));
                continue;
            }
        };
        let Some(obj) = item.as_object() else {
            continue;
        };
        for (m, op) in obj {
            if !METHODS.contains(&m.as_str()) {
                continue;
            }
            let id = op
                .get("operationId")
                .and_then(Value::as_str)
                .map(str::to_string);
            let rname = {
                let base_name = match &id {
                    Some(i) => snake(i),
                    None => name_of_path(m, path),
                };
                let base_name = if base_name == "any" || base_name == "conn" {
                    format!("{base_name}_op")
                } else {
                    base_name
                };
                let mut n = base_name.clone();
                let mut k = 2;
                while used_names.contains(&n) {
                    n = format!("{base_name}_{k}");
                    k += 1;
                }
                n
            };
            used_names.insert(rname.clone());
            if let Some(want) = &o.ops {
                if !want.iter().any(|w| Some(w) == id.as_ref() || *w == rname) {
                    continue;
                }
            }
            let label = id.clone().unwrap_or_else(|| rname.clone());
            match map_op(&sp, path, m, item, op, &base) {
                Ok(x) => {
                    names.push(rname);
                    ops.push(x);
                }
                Err(e) => {
                    refused.push(format!("{label} ({} {path}): {e}", m.to_ascii_uppercase()));
                    refused_ids.push((id, rname));
                }
            }
        }
    }
    let mut refused_named = Vec::new();
    if let Some(want) = &o.ops {
        for w in want {
            let found = names
                .iter()
                .zip(&ops)
                .any(|(n, op)| n == w || op.id.as_ref() == Some(w));
            if !found {
                let was_refused = refused_ids
                    .iter()
                    .any(|(i, n)| i.as_ref() == Some(w) || n == w);
                refused_named.push(if was_refused {
                    format!("{w}: not mapped (above)")
                } else {
                    format!("{w}: no such operation in the spec")
                });
            }
        }
    }
    if ops.is_empty() {
        let mut msg = "no operation could be mapped".to_string();
        if !refused.is_empty() {
            msg.push_str(":\n  ");
            msg.push_str(&refused.join("\n  "));
        }
        return Err(msg);
    }
    // the idempotency header: the one most operations declare (the first on a tie)
    let mut counts: Vec<(String, usize)> = Vec::new();
    for op in &ops {
        if let Some(h) = &op.idempotency {
            match counts.iter_mut().find(|(k, _)| k == h) {
                Some(e) => e.1 += 1,
                None => counts.push((h.clone(), 1)),
            }
        }
    }
    let most = counts.iter().map(|c| c.1).max();
    let idem = counts
        .iter()
        .find(|(_, n)| Some(*n) == most)
        .map(|x| x.0.clone());

    // create-then-read pairs: a POST to a collection whose answer has an id, and the routes about
    // one of its resources (the collection's path, then a parameter)
    let mut saved: BTreeMap<String, Vec<String>> = BTreeMap::new(); // collection -> fields
    let mut collections: BTreeMap<String, String> = BTreeMap::new(); // collection -> id field
    if o.stateful {
        for op in &ops {
            if op.method != "POST" || op.route_path.ends_with('}') {
                continue;
            }
            let Some((v, schema)) = &op.success else {
                continue;
            };
            let props = schema
                .as_ref()
                .map(|s| top_props(&sp, s))
                .unwrap_or_default();
            let has_id = v.get("id").is_some() || props.contains_key("id");
            if !has_id {
                continue;
            }
            let has_item = ops.iter().any(|x| {
                let (a, b) = (segs(&x.route_path), segs(&op.route_path));
                a.len() > b.len() && a[..b.len()] == b[..] && a[b.len()].is_none()
            });
            if !has_item {
                continue;
            }
            let fields: Vec<String> = op
                .echo
                .iter()
                .filter(|(k, e)| {
                    (e.src == Src::Json || e.src == Src::Form)
                        && *k != "id"
                        && (v.get(k.as_str()).is_some() || props.contains_key(k.as_str()))
                })
                .map(|(k, _)| k.clone())
                .collect();
            collections
                .entry(op.route_path.clone())
                .or_insert_with(|| "id".into());
            saved.entry(op.route_path.clone()).or_insert(fields);
        }
    }

    let mut routes: Vec<(Vec<u8>, usize, Value)> = Vec::new();
    for (i, (op, rname)) in ops.iter().zip(&names).enumerate() {
        let mut r = Map::new();
        r.insert("name".into(), Value::String(rname.clone()));
        r.insert("method".into(), Value::String(op.method.clone()));
        r.insert("path".into(), Value::String(op.route_path.clone()));
        let desc = if op.summary.is_empty() {
            format!("{} {}", op.method, op.path)
        } else {
            let mut s = op.summary.clone();
            if s.chars().count() > 100 {
                s = s.chars().take(97).collect::<String>() + "...";
            }
            s
        };
        if let Some(st) = op.route.get("status") {
            r.insert("status".into(), st.clone());
        }
        if let Some(h) = op.route.get("headers") {
            r.insert("headers".into(), h.clone());
        }
        // the collection this route creates in, or the resource it is about
        let my = segs(&op.route_path);
        let create_in = saved
            .keys()
            .find(|c| op.method == "POST" && **c == op.route_path)
            .cloned();
        let about: Option<(String, String)> = saved.keys().find_map(|c| {
            let cs = segs(c);
            if my.len() > cs.len() && my[..cs.len()] == cs[..] && my[cs.len()].is_none() {
                param_at(&op.route_path, cs.len()).map(|p| (c.clone(), p))
            } else {
                None
            }
        });
        let mut save: Option<Value> = None;
        let mut load: Option<String> = None;
        if let Some((v, schema)) = &op.success {
            let props = schema
                .as_ref()
                .map(|s| top_props(&sp, s))
                .unwrap_or_default();
            let mut ov = Overlay::default();
            let has = |k: &str| v.get(k).is_some();
            // the id
            let last_param = param_at(&op.route_path, my.len().saturating_sub(1))
                .filter(|_| my.last().is_some_and(Option::is_none));
            // a route below one resource that answers that resource (POST /things/{id}/cancel
            // answers the schema GET /things/{id} answers): the id is that parameter
            let same_as = || {
                ops.iter().find_map(|x| {
                    let xs = segs(&x.route_path);
                    let fits = x.method == "GET"
                        && x.schema_ref.is_some()
                        && x.schema_ref == op.schema_ref
                        && xs.len() < my.len()
                        && my[..xs.len()] == xs[..]
                        && xs.last().is_some_and(Option::is_none);
                    if fits {
                        param_at(&op.route_path, xs.len() - 1)
                    } else {
                        None
                    }
                })
            };
            if has("id") {
                let idt = if let Some(p) = last_param.clone().or_else(same_as) {
                    let raw = props
                        .get("id")
                        .map(|s| is_raw_type(&main_type(s)))
                        .unwrap_or(false)
                        || matches!(v.get("id"), Some(Value::Number(_)));
                    Some(tmpl(&format!("{{path:{p}}}"), raw))
                } else if op.method == "POST" {
                    Some(new_id(props.get("id").copied(), v.get("id")))
                } else {
                    None
                };
                if let Some(t) = idt {
                    ov.id = Some(("id".into(), t));
                }
            }
            // echoed fields
            for (k, e) in &op.echo {
                if !has(k) || k == "id" {
                    continue;
                }
                let src = match e.src {
                    Src::Json => "json",
                    Src::Form => "form",
                    Src::Query => "query",
                    Src::Path => "path",
                };
                let raw = props
                    .get(k.as_str())
                    .map(|s| is_raw_type(&main_type(s)))
                    .unwrap_or(e.raw);
                ov.fields
                    .insert(k.clone(), tmpl(&format!("{{{src}:{k}}}"), raw));
            }
            // stateful: a create saves, a route about one resource loads (and a PUT saves)
            if let Some(c) = &create_in {
                let fields = saved.get(c).cloned().unwrap_or_default();
                let key = match &ov.id {
                    Some((_, T::Raw(s))) | Some((_, T::Quoted(s))) => s.clone(),
                    _ => "{seq}".into(),
                };
                let mut f = Map::new();
                for k in &fields {
                    let e = &op.echo[k];
                    let src = if e.src == Src::Form { "form" } else { "json" };
                    f.insert(k.clone(), Value::String(format!("{{{src}:{k}}}")));
                }
                let mut s = Map::new();
                s.insert("key".into(), Value::String(key));
                if !f.is_empty() {
                    s.insert("fields".into(), Value::Object(f));
                }
                save = Some(Value::Object(s));
            } else if let Some((c, p)) = &about {
                load = Some(format!("{{path:{p}}}"));
                let fields = saved.get(c).cloned().unwrap_or_default();
                if op.method == "PUT" {
                    let mut f = Map::new();
                    for k in &fields {
                        if let Some(e) = op
                            .echo
                            .get(k)
                            .filter(|e| e.src == Src::Json || e.src == Src::Form)
                        {
                            let src = if e.src == Src::Form { "form" } else { "json" };
                            f.insert(k.clone(), Value::String(format!("{{{src}:{k}}}")));
                        }
                    }
                    if !f.is_empty() {
                        let mut s = Map::new();
                        s.insert("key".into(), Value::String(format!("{{path:{p}}}")));
                        s.insert("fields".into(), Value::Object(f));
                        save = Some(Value::Object(s));
                    }
                }
                for k in &fields {
                    if has(k) {
                        let raw = props
                            .get(k.as_str())
                            .map(|s| is_raw_type(&main_type(s)))
                            .unwrap_or(false);
                        ov.fields
                            .insert(k.clone(), tmpl(&format!("{{obj:{k}}}"), raw));
                    }
                }
            }
            let mut body = String::new();
            render(&overlay(v, &ov), &mut body);
            r.insert("body".into(), Value::String(body));
        } else if let Some(b) = op.route.get("body") {
            r.insert("body".into(), b.clone());
        }
        if load.is_none() {
            if let Some((_, p)) = &about {
                load = Some(format!("{{path:{p}}}"));
            }
        }
        if idem.is_some() && op.idempotency == idem && op.method != "GET" {
            r.insert("idempotent".into(), Value::Bool(true));
        }
        if let Some(s) = save {
            r.insert("save".into(), s);
        }
        if let Some(l) = load {
            r.insert("load".into(), Value::String(l));
        }
        if let Some(e) = op.route.get("errors") {
            r.insert("errors".into(), e.clone());
        }
        r.insert("description".into(), Value::String(desc));
        // literal segments before parameters, so /transfers/approve matches before /transfers/{id}
        let key: Vec<u8> = my.iter().map(|s| u8::from(s.is_none())).collect();
        routes.push((key, i, Value::Object(r)));
    }
    routes.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut fake = Map::new();
    fake.insert("name".into(), Value::String(name.clone()));
    fake.insert("port".into(), Value::Number(o.port.unwrap_or(9001).into()));
    if let Some(h) = &idem {
        fake.insert("idempotency".into(), Value::String(format!("header:{h}")));
    }
    if let Some(r) = &o.reference {
        fake.insert("ref".into(), Value::String(r.clone()));
    }
    if o.tls {
        let host = server_host(spec).ok_or("--tls: the spec's first server names no host")?;
        let mut t = Map::new();
        t.insert("hosts".into(), Value::Array(vec![Value::String(host)]));
        fake.insert("tls".into(), Value::Object(t));
    }
    fake.insert(
        "routes".into(),
        Value::Array(routes.into_iter().map(|x| x.2).collect()),
    );
    let title = spec
        .get("info")
        .and_then(|i| i.get("title"))
        .map(scalar_text)
        .unwrap_or_default();
    let version = spec
        .get("info")
        .and_then(|i| i.get("version"))
        .map(scalar_text)
        .unwrap_or_default();
    let nroutes = ops.len();
    let mut header =
        vec![
            format!("Generated by `{}`", o.command),
            format!(
                "from \"{}\" {} (OpenAPI {}, sha256 {}).",
                one_line(&title),
                one_line(&version),
                ver,
                &o.sha256[..o.sha256.len().min(16)]
            ),
            format!(
            "{nroutes} route{}{}. Edit it freely; `hydo guide outside-services` explains each key.",
            if nroutes == 1 { "" } else { "s" },
            if refused.is_empty() {
                String::new()
            } else {
                format!(", {} operation(s) not mapped (listed when it ran)", refused.len())
            }
        ),
        ];
    if !base.is_empty() {
        header.push(format!(
            "The routes start with the first server's path, {base}."
        ));
    }
    if !o.tls {
        if let Some(h) = server_host(spec) {
            header.push(format!(
                "An SDK that calls https://{h} needs `tls: {{hosts: [{h}]}}` (--tls)."
            ));
        }
    }
    Ok(Generated {
        fake: Value::Object(fake),
        header,
        refused,
        refused_named,
    })
}

fn one_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

// ------------------------------------------------------------------------------- writing YAML

/// A YAML scalar that hydo's YAML subset reads back as the same string.
fn ys(s: &str) -> String {
    let plain_ok = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./-".contains(c))
        && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/' || c == '_')
        && !matches!(
            s,
            "true" | "True" | "yes" | "false" | "False" | "no" | "null" | "~"
        );
    if plain_ok {
        return s.to_string();
    }
    if s.chars().any(|c| c.is_control()) {
        return serde_json::to_string(s).unwrap_or_default();
    }
    format!("'{}'", s.replace('\'', "''"))
}

fn yv(v: &Value) -> String {
    match v {
        Value::String(s) => ys(s),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "null".into(),
        x => serde_json::to_string(x).unwrap_or_default(),
    }
}

fn ymap(m: &Map<String, Value>, ind: usize, out: &mut String) {
    let pad = " ".repeat(ind);
    for (k, v) in m {
        let key = if k
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            && k.starts_with(|c: char| c.is_ascii_alphabetic())
        {
            k.clone()
        } else {
            format!("'{}'", k.replace('\'', "''"))
        };
        match v {
            Value::Object(o) => {
                out.push_str(&format!("{pad}{key}:\n"));
                ymap(o, ind + 2, out);
            }
            Value::Array(a) if a.iter().all(|x| !x.is_object() && !x.is_array()) => {
                let items: Vec<String> = a.iter().map(yv).collect();
                out.push_str(&format!("{pad}{key}: [{}]\n", items.join(", ")));
            }
            Value::Array(a) => {
                out.push_str(&format!("{pad}{key}:\n"));
                for x in a {
                    yitem(x, ind + 2, out);
                }
            }
            x => out.push_str(&format!("{pad}{key}: {}\n", yv(x))),
        }
    }
}

/// One item of a block sequence at indentation `ind` (`- key: ...`).
fn yitem(v: &Value, ind: usize, out: &mut String) {
    let pad = " ".repeat(ind);
    match v {
        Value::Object(o) => {
            let mut inner = String::new();
            ymap(o, ind + 2, &mut inner);
            let mut lines = inner.lines();
            if let Some(l) = lines.next() {
                out.push_str(&format!("{pad}- {}\n", &l[ind + 2..]));
            }
            for l in lines {
                out.push_str(l);
                out.push('\n');
            }
        }
        x => out.push_str(&format!("{pad}- {}\n", yv(x))),
    }
}

/// The fake as YAML: the comment lines, then one sequence item at indentation `ind`.
pub fn entry_yaml(g: &Generated, ind: usize) -> String {
    let pad = " ".repeat(ind);
    let mut out = String::new();
    for h in &g.header {
        out.push_str(&format!("{pad}# {h}\n"));
    }
    yitem(&g.fake, ind, &mut out);
    out
}

/// The whole output of `-o` or of a print: `fakes:` and the entry.
pub fn file_yaml(g: &Generated) -> String {
    format!("fakes:\n{}", entry_yaml(g, 2))
}

/// The fake checked as hydo checks a manifest: inside a minimal project, through the YAML subset,
/// the validator and the project model.
pub fn check(g: &Generated) -> Result<(), String> {
    let text = format!(
        "name: openapi-check\nservices:\n  - name: app\n    cmd: sleep 1\n    ready: 'true'\nworkload:\n  steps: 1\n  run: 'true'\ninvariants:\n  - id: i\n    cmd: 'true'\n{}",
        file_yaml(g)
    );
    let mut v = hydo_manifest::yaml::parse_yaml(&text, "hydo.yaml").map_err(|e| e.to_string())?;
    let back = v
        .get("fakes")
        .and_then(|f| f.get(0))
        .cloned()
        .unwrap_or(Value::Null);
    if back != g.fake {
        return Err(format!(
            "the written YAML reads back as another fake (a bug of hydo fake):\n{}\n{}",
            back, g.fake
        ));
    }
    hydo_manifest::model::from_value(&v).map_err(|e| e.to_string())?;
    hydo_manifest::validate_data(&mut v).map_err(|e| e.to_string())?;
    Ok(())
}

/// The manifest's text with the entry added to its `fakes:` (a new `fakes:` at the end when it
/// has none).
pub fn add_to(text: &str, g: &Generated) -> Result<String, String> {
    let name = g.fake["name"].as_str().unwrap_or("");
    let lines: Vec<&str> = text.split('\n').collect();
    let at = lines.iter().position(|l| l.starts_with("fakes:"));
    let mut out: Vec<String> = Vec::new();
    match at {
        None => {
            let mut t = text.to_string();
            if !t.ends_with('\n') && !t.is_empty() {
                t.push('\n');
            }
            t.push('\n');
            t.push_str(&file_yaml(g));
            return Ok(t);
        }
        Some(i) => {
            let rest = lines[i]["fakes:".len()..].trim();
            let rest = hydo_manifest::yaml::strip_comment(rest);
            let inline_empty = rest.trim() == "[]";
            if !rest.trim().is_empty() && !inline_empty {
                return Err(format!(
                    "line {}: fakes: is written inline; write it as a block list to add to it",
                    i + 1
                ));
            }
            // the block: lines after it that are indented, items at column 0, blank lines and
            // comments; it ends before the next top-level key
            let mut last = i;
            let mut item_ind: Option<usize> = None;
            for (j, l) in lines.iter().enumerate().skip(i + 1) {
                let t = l.trim_start();
                if t.is_empty() || t.starts_with('#') {
                    continue;
                }
                let ind = l.len() - t.len();
                if ind == 0 && !t.starts_with("- ") && t != "-" {
                    break;
                }
                if t.starts_with("- ") && item_ind.is_none() {
                    item_ind = Some(ind);
                }
                last = j;
            }
            let ind = item_ind.unwrap_or(2);
            for (j, l) in lines.iter().enumerate() {
                if j == i && inline_empty {
                    out.push("fakes:".into());
                } else {
                    out.push(l.to_string());
                }
                if j == last {
                    let e = entry_yaml(g, ind);
                    out.extend(e.trim_end_matches('\n').split('\n').map(str::to_string));
                }
            }
        }
    }
    let t = out.join("\n");
    let v = hydo_manifest::yaml::parse_yaml(&t, "hydo.yaml").map_err(|e| e.to_string())?;
    let n = v
        .get("fakes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|f| f.get("name").and_then(Value::as_str) == Some(name))
                .count()
        })
        .unwrap_or(0);
    let found = v
        .get("fakes")
        .and_then(Value::as_array)
        .is_some_and(|a| a.contains(&g.fake));
    if n != 1 || !found {
        return Err(format!(
            "the manifest with the new fake does not read back as one fake {name}; add the entry by hand (-o FILE)"
        ));
    }
    Ok(t)
}

// ------------------------------------------------------------------------------- the command

struct Args {
    spec: String,
    o: Opts,
    out: Option<String>,
    add_to: Option<String>,
}

fn parse(a: &[String]) -> Result<Args, String> {
    let mut spec = None;
    let mut o = Opts::default();
    let mut out = None;
    let mut add_to = None;
    let mut flags: Vec<String> = Vec::new();
    let mut i = 0;
    let val = |i: &mut usize, f: &str| -> Result<String, String> {
        *i += 1;
        a.get(*i).cloned().ok_or(format!("{f} needs a value"))
    };
    while i < a.len() {
        let x = a[i].as_str();
        match x {
            "--name" => {
                let v = val(&mut i, x)?;
                flags.push(format!("--name {v}"));
                o.name = Some(v);
            }
            "--port" => {
                let v = val(&mut i, x)?;
                let p: i64 = v
                    .parse()
                    .ok()
                    .filter(|p| (1..=65535).contains(p))
                    .ok_or(format!("--port {v}: a TCP port number"))?;
                flags.push(format!("--port {p}"));
                o.port = Some(p);
            }
            "--ops" => {
                let v = val(&mut i, x)?;
                flags.push(format!("--ops {v}"));
                o.ops = Some(
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
            }
            "--ref" => {
                let v = val(&mut i, x)?;
                flags.push(format!("--ref {v}"));
                o.reference = Some(v);
            }
            "--base-path" => {
                let v = val(&mut i, x)?;
                flags.push(format!("--base-path {v}"));
                o.base_path = Some(v);
            }
            "--stateful" => {
                flags.push(x.into());
                o.stateful = true;
            }
            "--tls" => {
                flags.push(x.into());
                o.tls = true;
            }
            "-o" | "--out" => out = Some(val(&mut i, x)?),
            "--add-to" => add_to = Some(val(&mut i, x)?),
            _ if x.starts_with('-') && x.len() > 1 => return Err(format!("unknown option {x}")),
            _ => {
                if spec.is_some() {
                    return Err(format!("one spec file, not also {x}"));
                }
                spec = Some(x.to_string());
            }
        }
        i += 1;
    }
    let spec = spec.ok_or("which OpenAPI file? (hydo fake from-openapi <spec.yaml>)")?;
    if out.is_some() && add_to.is_some() {
        return Err("-o and --add-to: give one".into());
    }
    let base = Path::new(&spec)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| spec.clone());
    o.command = std::iter::once(format!("hydo fake from-openapi {base}"))
        .chain(flags)
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Args {
        spec,
        o,
        out,
        add_to,
    })
}

/// The spec's text: a file, or `-` for standard input.
fn read_spec(p: &str) -> Result<Vec<u8>, String> {
    if p == "-" {
        let mut b = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut b).map_err(|e| e.to_string())?;
        return Ok(b);
    }
    std::fs::read(p).map_err(|e| format!("{p}: {e}"))
}

/// Generates from the spec's bytes (the tests' entry).
pub fn generate_bytes(b: &[u8], o: &Opts) -> Result<Generated, String> {
    let text = std::str::from_utf8(b).map_err(|_| "the spec is not UTF-8 text".to_string())?;
    let spec = load_spec(text)?;
    let mut o = o.clone();
    o.sha256 = format!("{:x}", Sha256::digest(b));
    let g = generate(&spec, &o)?;
    check(&g)?;
    Ok(g)
}

/// `hydo fake <sub> ...`.
pub fn main(argv: &[String]) -> i32 {
    match argv.first().map(String::as_str) {
        None | Some("-h") | Some("--help") | Some("help") => {
            println!("{USAGE}");
            if argv.is_empty() {
                2
            } else {
                0
            }
        }
        Some("from-openapi") => from_openapi(&argv[1..]),
        Some(x) => {
            eprintln!("{TOOL}: unknown sub-command {x} (from-openapi)");
            2
        }
    }
}

fn from_openapi(a: &[String]) -> i32 {
    if a.iter().any(|x| x == "-h" || x == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let args = match parse(a) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{TOOL}: {e}");
            return 2;
        }
    };
    let bytes = match read_spec(&args.spec) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{TOOL}: {e}");
            return 1;
        }
    };
    let g = match generate_bytes(&bytes, &args.o) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{TOOL}: {}: {e}", args.spec);
            return 1;
        }
    };
    for r in &g.refused {
        eprintln!("{TOOL}: not mapped: {r}");
    }
    for r in &g.refused_named {
        eprintln!("{TOOL}: --ops {r}");
    }
    let n = g.fake["routes"].as_array().map(Vec::len).unwrap_or(0);
    let name = g.fake["name"].as_str().unwrap_or("").to_string();
    let wrote = if let Some(m) = &args.add_to {
        let text = match std::fs::read_to_string(m) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{TOOL}: {m}: {e}");
                return 1;
            }
        };
        if let Ok(v) = hydo_manifest::parse_text(&text, m, false) {
            let dup = v.get("fakes").and_then(Value::as_array).is_some_and(|a| {
                a.iter()
                    .any(|f| f.get("name").and_then(Value::as_str) == Some(&name))
            });
            if dup {
                eprintln!(
                    "{TOOL}: {m} has a fake named {name} already; remove it first, or write this one with -o FILE"
                );
                return 1;
            }
        }
        match add_to(&text, &g).and_then(|t| std::fs::write(m, t).map_err(|e| e.to_string())) {
            Ok(()) => format!("added to {m}"),
            Err(e) => {
                eprintln!("{TOOL}: {m}: {e}");
                return 1;
            }
        }
    } else if let Some(f) = &args.out {
        if let Err(e) = std::fs::write(f, file_yaml(&g)) {
            eprintln!("{TOOL}: {f}: {e}");
            return 1;
        }
        format!("written to {f}")
    } else {
        print!("{}", file_yaml(&g));
        String::new()
    };
    if !wrote.is_empty() {
        eprintln!(
            "{TOOL}: fake {name} with {n} route{} {wrote}",
            if n == 1 { "" } else { "s" }
        );
    }
    if !g.refused_named.is_empty() {
        return 1;
    }
    0
}

#[cfg(test)]
#[path = "openapi/tests.rs"]
mod tests;
