//! `/etc/hydo/world.json` (compiled by hydo-build from the manifest's `fakes:` and `proxies:`),
//! route matching, the request view with its extractors, and answer templates.

use crate::gojson::{self, J};
use crate::goish;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub log: String,
    pub sock: String,
    pub fakes: Vec<Endpoint>,
    pub proxies: Vec<Endpoint>,
}

/// One fake service or one fault proxy. Kind "fake" answers itself from its routes; kind "proxy"
/// forwards to `to` (a real service) and only applies the faults.
#[derive(Clone, Debug, Default)]
pub struct Endpoint {
    pub kind: String,
    pub name: String,
    pub addr: String,
    pub port: i64,
    /// http (HTTP/1.1 + cleartext HTTP/2) | grpc | tcp (proxy only)
    pub protocol: String,
    /// extractor of the request's idempotency key
    pub idempotency: String,
    /// default extractor of the business reference
    pub r#ref: String,
    /// proxy: upstream host:port
    pub to: String,
    pub routes: Vec<Route>,
}

#[derive(Clone, Debug, Default)]
pub struct Route {
    pub name: String,
    /// "" = any
    pub method: String,
    /// exact, /prefix/*, or with {name} segments
    pub path: String,
    /// http answer (fake)
    pub status: i64,
    /// grpc status of the answer (fake)
    pub code: i64,
    pub headers: BTreeMap<String, String>,
    /// http answer body (template)
    pub body: String,
    /// grpc answer message, hex protobuf bytes
    pub message: String,
    pub r#ref: String,
    /// does a request on this route have a side effect
    pub effect: Option<bool>,
    pub idempotent: bool,
    pub webhook: Option<Webhook>,
    pub segs: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct Webhook {
    pub url: String,
    pub method: String,
    /// template: {seq} {ref} {idem} {fake} {route} only
    pub body: String,
    pub headers: BTreeMap<String, String>,
    pub delay_ms: i64,
    pub retries: i64,
}

// ------------------------------------------------------------------ loading (Go's json.Unmarshal)

type Fields<'a> = Vec<(&'a str, &'a Value)>;

/// The object's members in document order (`null` = absent), or a type error.
fn object<'a>(v: &'a Value, what: &str) -> Result<Option<Fields<'a>>, String> {
    match v {
        Value::Null => Ok(None),
        Value::Object(m) => Ok(Some(m.iter().map(|(k, v)| (k.as_str(), v)).collect())),
        _ => Err(format!("json: cannot unmarshal {} into Go value of type {what}", jtype(v))),
    }
}

fn jtype(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Go matches a member to a field by its exact name, else by a case-insensitive one.
fn is(k: &str, field: &str) -> bool {
    k == field || k.eq_ignore_ascii_case(field)
}

fn set_str(dst: &mut String, v: &Value, f: &str) -> Result<(), String> {
    match v {
        Value::Null => Ok(()),
        Value::String(s) => {
            *dst = s.clone();
            Ok(())
        }
        _ => Err(format!("json: cannot unmarshal {} into Go struct field .{f} of type string", jtype(v))),
    }
}

fn set_int(dst: &mut i64, v: &Value, f: &str) -> Result<(), String> {
    match v {
        Value::Null => Ok(()),
        Value::Number(n) if n.as_i64().is_some() => {
            *dst = n.as_i64().unwrap_or(0);
            Ok(())
        }
        _ => Err(format!("json: cannot unmarshal {v} into Go struct field .{f} of type int")),
    }
}

fn set_bool(dst: &mut bool, v: &Value, f: &str) -> Result<(), String> {
    match v {
        Value::Null => Ok(()),
        Value::Bool(b) => {
            *dst = *b;
            Ok(())
        }
        _ => Err(format!("json: cannot unmarshal {} into Go struct field .{f} of type bool", jtype(v))),
    }
}

fn set_map(dst: &mut BTreeMap<String, String>, v: &Value, f: &str) -> Result<(), String> {
    if let Some(fs) = object(v, "map[string]string")? {
        let mut m = BTreeMap::new();
        for (k, x) in fs {
            let mut s = String::new();
            set_str(&mut s, x, f)?;
            if !x.is_null() {
                m.insert(k.to_string(), s);
            } else {
                m.insert(k.to_string(), String::new());
            }
        }
        *dst = m;
    }
    Ok(())
}

fn webhook(v: &Value) -> Result<Option<Webhook>, String> {
    let Some(fs) = object(v, "main.Webhook")? else {
        return Ok(None);
    };
    let mut w = Webhook::default();
    for (k, x) in fs {
        match () {
            _ if is(k, "url") => set_str(&mut w.url, x, "url")?,
            _ if is(k, "method") => set_str(&mut w.method, x, "method")?,
            _ if is(k, "body") => set_str(&mut w.body, x, "body")?,
            _ if is(k, "headers") => set_map(&mut w.headers, x, "headers")?,
            _ if is(k, "delay_ms") => set_int(&mut w.delay_ms, x, "delay_ms")?,
            _ if is(k, "retries") => set_int(&mut w.retries, x, "retries")?,
            _ => {}
        }
    }
    Ok(Some(w))
}

fn route(v: &Value) -> Result<Route, String> {
    let Some(fs) = object(v, "main.Route")? else {
        return Err("a route is null".into());
    };
    let mut r = Route::default();
    for (k, x) in fs {
        match () {
            _ if is(k, "name") => set_str(&mut r.name, x, "name")?,
            _ if is(k, "method") => set_str(&mut r.method, x, "method")?,
            _ if is(k, "path") => set_str(&mut r.path, x, "path")?,
            _ if is(k, "status") => set_int(&mut r.status, x, "status")?,
            _ if is(k, "code") => set_int(&mut r.code, x, "code")?,
            _ if is(k, "headers") => set_map(&mut r.headers, x, "headers")?,
            _ if is(k, "body") => set_str(&mut r.body, x, "body")?,
            _ if is(k, "message") => set_str(&mut r.message, x, "message")?,
            _ if is(k, "ref") => set_str(&mut r.r#ref, x, "ref")?,
            _ if is(k, "effect") => {
                if x.is_null() {
                    r.effect = None;
                } else {
                    let mut b = false;
                    set_bool(&mut b, x, "effect")?;
                    r.effect = Some(b);
                }
            }
            _ if is(k, "idempotent") => set_bool(&mut r.idempotent, x, "idempotent")?,
            _ if is(k, "webhook") => r.webhook = webhook(x)?,
            _ => {}
        }
    }
    Ok(r)
}

fn endpoint(v: &Value) -> Result<Endpoint, String> {
    let Some(fs) = object(v, "main.Endpoint")? else {
        return Err("an endpoint is null".into());
    };
    let mut e = Endpoint::default();
    for (k, x) in fs {
        match () {
            _ if is(k, "kind") => set_str(&mut e.kind, x, "kind")?,
            _ if is(k, "name") => set_str(&mut e.name, x, "name")?,
            _ if is(k, "addr") => set_str(&mut e.addr, x, "addr")?,
            _ if is(k, "port") => set_int(&mut e.port, x, "port")?,
            _ if is(k, "protocol") => set_str(&mut e.protocol, x, "protocol")?,
            _ if is(k, "idempotency") => set_str(&mut e.idempotency, x, "idempotency")?,
            _ if is(k, "ref") => set_str(&mut e.r#ref, x, "ref")?,
            _ if is(k, "to") => set_str(&mut e.to, x, "to")?,
            _ if is(k, "routes") => match x {
                Value::Null => e.routes.clear(),
                Value::Array(a) => e.routes = a.iter().map(route).collect::<Result<_, _>>()?,
                _ => return Err(format!("json: cannot unmarshal {} into Go struct field .routes", jtype(x))),
            },
            _ => {}
        }
    }
    Ok(e)
}

fn endpoints(v: &Value, f: &str) -> Result<Vec<Endpoint>, String> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(a) => a.iter().map(endpoint).collect(),
        _ => Err(format!("json: cannot unmarshal {} into Go struct field Config.{f}", jtype(v))),
    }
}

impl Config {
    /// `loadConfig`: parse the file, then the defaults (protocol http, addr 127.0.0.1, status
    /// 200, webhook method POST and 5 retries).
    pub fn load(path: &str) -> Result<Config, String> {
        let b = std::fs::read(path).map_err(|e| goish::path_err("open", path, &e))?;
        let v: Value = serde_json::from_slice(&b).map_err(|e| format!("{path}: {e}"))?;
        let mut c = Config::from_value(&v).map_err(|e| format!("{path}: {e}"))?;
        c.defaults();
        Ok(c)
    }

    pub fn from_value(v: &Value) -> Result<Config, String> {
        let mut c = Config::default();
        let Some(fs) = object(v, "main.Config")? else {
            return Ok(c);
        };
        for (k, x) in fs {
            match () {
                _ if is(k, "log") => set_str(&mut c.log, x, "log")?,
                _ if is(k, "sock") => set_str(&mut c.sock, x, "sock")?,
                _ if is(k, "fakes") => c.fakes = endpoints(x, "fakes")?,
                _ if is(k, "proxies") => c.proxies = endpoints(x, "proxies")?,
                _ => {}
            }
        }
        Ok(c)
    }

    pub fn defaults(&mut self) {
        for e in &mut self.fakes {
            e.kind = "fake".into();
        }
        for e in &mut self.proxies {
            e.kind = "proxy".into();
        }
        for e in self.fakes.iter_mut().chain(self.proxies.iter_mut()) {
            if e.protocol.is_empty() {
                e.protocol = "http".into();
            }
            if e.addr.is_empty() {
                e.addr = "127.0.0.1".into();
            }
            for r in &mut e.routes {
                if !r.path.is_empty() && !r.path.ends_with('*') {
                    r.segs = Some(r.path.trim_matches('/').split('/').map(str::to_string).collect());
                }
                if r.status == 0 {
                    r.status = 200;
                }
                if let Some(w) = &mut r.webhook {
                    if w.method.is_empty() {
                        w.method = "POST".into();
                    }
                    if w.retries == 0 {
                        w.retries = 5;
                    }
                }
            }
        }
    }

    /// Every fake, then every proxy.
    pub fn all(&self) -> impl Iterator<Item = &Endpoint> {
        self.fakes.iter().chain(self.proxies.iter())
    }

    pub fn endpoint(&self, name: &str) -> Option<&Endpoint> {
        self.all().find(|e| e.name == name)
    }
}

pub type Vars = Vec<(String, Vec<u8>)>;

impl Endpoint {
    pub fn route(&self, name: &str) -> Option<&Route> {
        self.routes.iter().find(|r| r.name == name)
    }

    /// The first route whose method and path fit; path params go into the vars.
    pub fn matches(&self, method: &str, path: &[u8]) -> (Option<&Route>, Vars) {
        for r in &self.routes {
            if !r.method.is_empty() && !r.method.eq_ignore_ascii_case(method) {
                continue;
            }
            if r.path.is_empty() || r.path == "*" {
                return (Some(r), Vars::new());
            }
            if let Some(pre) = r.path.strip_suffix('*') {
                if path.starts_with(pre.as_bytes()) {
                    return (Some(r), Vars::new());
                }
                continue;
            }
            let ps = path_segs(path);
            let segs = r.segs.as_deref().unwrap_or(&[]);
            if ps.len() != segs.len() {
                continue;
            }
            let mut vars = Vars::new();
            let mut ok = true;
            for (s, p) in segs.iter().zip(&ps) {
                if s.starts_with('{') && s.ends_with('}') && s.len() >= 2 {
                    let name = s[1..s.len() - 1].to_string();
                    match vars.iter_mut().find(|(k, _)| *k == name) {
                        Some(v) => v.1 = p.to_vec(),
                        None => vars.push((name, p.to_vec())),
                    }
                } else if s.as_bytes() != *p {
                    ok = false;
                    break;
                }
            }
            if ok {
                return (Some(r), vars);
            }
        }
        (None, Vars::new())
    }
}

/// `strings.Split(strings.Trim(path, "/"), "/")`
pub fn path_segs(path: &[u8]) -> Vec<&[u8]> {
    let mut a = 0;
    let mut b = path.len();
    while a < b && path[a] == b'/' {
        a += 1;
    }
    while b > a && path[b - 1] == b'/' {
        b -= 1;
    }
    path[a..b].split(|&c| c == b'/').collect()
}

impl Route {
    /// A request on the route has a side effect in the outside world. Default: every method but
    /// GET/HEAD/OPTIONS (gRPC: every call).
    pub fn effectful(r: Option<&Route>, method: &str, proto: &str) -> bool {
        if let Some(e) = r.and_then(|r| r.effect) {
            return e;
        }
        if proto == "grpc" {
            return true;
        }
        !matches!(method.to_ascii_uppercase().as_str(), "GET" | "HEAD" | "OPTIONS")
    }
}

// ------------------------------------------------------------------ request view + extractors

#[derive(Default)]
pub struct ReqView {
    pub method: String,
    pub path: Vec<u8>,
    /// canonical key, value (in arrival order)
    pub header: Vec<(String, Vec<u8>)>,
    pub query: Vec<(Vec<u8>, Vec<u8>)>,
    pub body: Vec<u8>,
    /// grpc: the first message's bytes
    pub msg: Vec<u8>,
    pub vars: Vars,
    js: OnceLock<Option<BTreeMap<String, J>>>,
}

pub fn body_hash(b: &[u8]) -> String {
    let h = Sha256::digest(b);
    h[..6].iter().map(|x| format!("{x:02x}")).collect()
}

/// Go's `strconv.Atoi` for the arguments of extractors and rules.
/// (Rust's integer parsing accepts and refuses the same strings: an optional sign, then digits.)
pub fn atoi(s: &str) -> Option<i64> {
    s.parse::<i64>().ok()
}

impl ReqView {
    fn json(&self) -> Option<&BTreeMap<String, J>> {
        self.js.get_or_init(|| gojson::parse_object(&self.body)).as_ref()
    }

    pub fn header_get(&self, key: &str) -> Option<&[u8]> {
        let k = goish::canonical_key(key);
        self.header.iter().find(|(h, _)| *h == k).map(|(_, v)| v.as_slice())
    }

    /// An extractor: header:<Name> | json:<a.b> | query:<name> | path:<name or 1-based index> |
    /// form:<name> | pb:<field number> | body (hash) | "" (none).
    pub fn extract(&self, spec: &str) -> Vec<u8> {
        if spec.is_empty() {
            return Vec::new();
        }
        let (kind, arg) = spec.split_once(':').unwrap_or((spec, ""));
        match kind {
            "header" => self.header_get(arg).map(<[u8]>::to_vec).unwrap_or_default(),
            "query" => goish::query_get(&self.query, arg.as_bytes())
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
            "form" => {
                let (vals, bad) = goish::parse_query(&self.body);
                if bad {
                    return Vec::new();
                }
                goish::query_get(&vals, arg.as_bytes())
                    .map(<[u8]>::to_vec)
                    .unwrap_or_default()
            }
            "path" => {
                if let Some((_, v)) = self.vars.iter().find(|(k, _)| k == arg) {
                    return v.clone();
                }
                if let Some(n) = atoi(arg) {
                    let ps = path_segs(&self.path);
                    if n >= 1 && n as usize <= ps.len() {
                        return ps[n as usize - 1].to_vec();
                    }
                }
                Vec::new()
            }
            "json" => {
                let Some(m) = self.json() else {
                    return Vec::new();
                };
                let mut keys = arg.split('.');
                let first = keys.next().unwrap_or("");
                let mut cur = m.get(first);
                for k in keys {
                    cur = match cur {
                        Some(J::Obj(o)) => o.get(k),
                        _ => return Vec::new(),
                    };
                }
                match cur {
                    None | Some(J::Null) => Vec::new(),
                    Some(J::Str(s)) => s.as_bytes().to_vec(),
                    Some(J::Num(f)) => gojson::format_f(*f).into_bytes(),
                    Some(J::Bool(b)) => b.to_string().into_bytes(),
                    Some(x) => gojson::marshal(x).into_bytes(),
                }
            }
            "pb" => match atoi(arg) {
                Some(n) => pb_field(&self.msg, n),
                None => Vec::new(),
            },
            "body" => body_hash(&self.body).into_bytes(),
            _ => Vec::new(),
        }
    }
}

/// The first top-level field n of a protobuf message, as its bytes (length-delimited) or a
/// decimal (varint). Enough to name a call by a field of its request.
pub fn pb_field(b: &[u8], n: i64) -> Vec<u8> {
    let mut i = 0usize;
    while i < b.len() {
        let Some((key, k)) = uvarint(&b[i..]) else {
            return Vec::new();
        };
        i += k;
        let f = (key >> 3) as i64;
        match key & 7 {
            0 => {
                let Some((x, k)) = uvarint(b.get(i..).unwrap_or(&[])) else {
                    return Vec::new();
                };
                i += k;
                if f == n {
                    return x.to_string().into_bytes();
                }
            }
            1 => i += 8,
            5 => i += 4,
            2 => {
                let Some((l, k)) = uvarint(b.get(i..).unwrap_or(&[])) else {
                    return Vec::new();
                };
                let Some(end) = usize::try_from(l)
                    .ok()
                    .and_then(|l| (i + k).checked_add(l))
                    .filter(|&e| e <= b.len())
                else {
                    return Vec::new();
                };
                i += k;
                if f == n {
                    return b[i..end].to_vec();
                }
                i = end;
            }
            _ => return Vec::new(),
        }
    }
    Vec::new()
}

fn uvarint(b: &[u8]) -> Option<(u64, usize)> {
    let mut x = 0u64;
    for (i, &c) in b.iter().enumerate() {
        if i == 10 {
            return None;
        }
        x |= ((c & 0x7f) as u64).wrapping_shl(7 * i as u32);
        if c < 0x80 {
            return Some((x, i + 1));
        }
    }
    None
}

const TMPL_NAMES: [&str; 11] = [
    "seq", "ref", "idem", "fake", "route", "json", "header", "query", "path", "form", "pb",
];

pub type Ctx<'a> = [(&'a str, &'a [u8])];

/// Render a template: {seq} {ref} {idem} {fake} {route} plus, with a request, {json:a.b}
/// {header:X} {query:x} {path:x} {form:x} {pb:n}. (Single braces: {{name}} is a build
/// parameter.) The same matches as the regular expression
/// `\{(seq|ref|idem|fake|route|json|header|query|path|form|pb)(?::([^{}"]*))?\}`.
pub fn render(t: &str, ctx: &Ctx, v: Option<&ReqView>) -> Vec<u8> {
    let b = t.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            if let Some((name, arg, end)) = tmpl_at(b, i) {
                match arg {
                    None => {
                        if let Some((_, v)) = ctx.iter().find(|(k, _)| *k == name) {
                            out.extend_from_slice(v);
                        }
                    }
                    Some(a) => {
                        if let Some(v) = v {
                            out.extend(v.extract(&format!("{name}:{a}")));
                        }
                    }
                }
                i = end;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// A template reference at b[i] ('{'): its name, its argument (None when absent or empty) and
/// the index after its '}'.
fn tmpl_at(b: &[u8], i: usize) -> Option<(&'static str, Option<String>, usize)> {
    let rest = &b[i + 1..];
    let name = TMPL_NAMES.iter().find(|n| rest.starts_with(n.as_bytes()))?;
    let j = i + 1 + name.len();
    match b.get(j)? {
        b'}' => Some((name, None, j + 1)),
        b':' => {
            let mut k = j + 1;
            while k < b.len() && !b"{}\"".contains(&b[k]) {
                k += 1;
            }
            if b.get(k) == Some(&b'}') {
                let a = String::from_utf8_lossy(&b[j + 1..k]).into_owned();
                Some((name, if a.is_empty() { None } else { Some(a) }, k + 1))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pb_and_extract() {
        let msg = [0x08, 0x96, 0x01, 0x12, 2, b'o', b'5'];
        assert_eq!(pb_field(&msg, 1), b"150");
        assert_eq!(pb_field(&msg, 2), b"o5");
        assert_eq!(pb_field(&msg, 3), b"");
        let v = ReqView {
            path: b"/v1/orders/42/cancel".to_vec(),
            body: br#"{"order":"o7","n":{"amount":12.5}}"#.to_vec(),
            query: vec![(b"x".to_vec(), b"y".to_vec())],
            vars: vec![("id".into(), b"42".to_vec())],
            ..Default::default()
        };
        for (spec, want) in [
            ("json:order", "o7"),
            ("json:n.amount", "12.5"),
            ("path:id", "42"),
            ("path:4", "cancel"),
            ("query:x", "y"),
            ("", ""),
        ] {
            assert_eq!(String::from_utf8(v.extract(spec)).unwrap(), want, "{spec}");
        }
        let got = render(
            r#"{"id":"ch_{seq}","order":"{ref}","a":{json:n.amount}}"#,
            &[("seq", b"3"), ("ref", b"o7")],
            Some(&v),
        );
        assert_eq!(String::from_utf8(got).unwrap(), r#"{"id":"ch_3","order":"o7","a":12.5}"#);
        // a reference the regular expression does not match stays as it is
        assert_eq!(render(r#"{json:"a"}{x}{seq:}"#, &[("seq", b"9")], Some(&v)), br#"{json:"a"}{x}9"#);
        assert_eq!(render("{json:order}", &[], None), b"");
    }

    #[test]
    fn matching() {
        let mut c = Config {
            fakes: vec![Endpoint {
                name: "p".into(),
                routes: vec![
                    Route { name: "a".into(), method: "post".into(), path: "/v1/charges".into(), ..Default::default() },
                    Route { name: "b".into(), path: "/v1/orders/{id}/cancel".into(), ..Default::default() },
                    Route { name: "c".into(), path: "/files/*".into(), ..Default::default() },
                    Route { name: "d".into(), path: "/".into(), ..Default::default() },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        c.defaults();
        let e = &c.fakes[0];
        assert_eq!(e.matches("POST", b"/v1/charges").0.unwrap().name, "a");
        assert!(e.matches("GET", b"/v1/charges").0.is_none());
        let (r, v) = e.matches("GET", b"/v1/orders/9/cancel");
        assert_eq!((r.unwrap().name.as_str(), v), ("b", vec![("id".to_string(), b"9".to_vec())]));
        assert_eq!(e.matches("GET", b"/files/x/y").0.unwrap().name, "c");
        assert_eq!(e.matches("GET", b"/").0.unwrap().name, "d");
        assert_eq!(e.routes[0].status, 200);
    }

    #[test]
    fn atoi_like_go() {
        assert_eq!(atoi("12"), Some(12));
        assert_eq!(atoi("+3"), Some(3));
        assert_eq!(atoi("-3"), Some(-3));
        assert_eq!(atoi(""), None);
        assert_eq!(atoi("1x"), None);
        assert_eq!(atoi("+-1"), None);
    }
}
