//! The hints file and the plan (`load.ReadHints`, `load.Build` of the Go generator): the hints
//! are checked against the schema, and the plan is a pure function of both.

use crate::yaml::{self, Any, Node, D};
use hydo_grpc::gofmt::{self, quote};
use hydo_grpc::outcome;
use hydo_grpc::spec::{self, Drive, FieldHint as SpecHint, From, Plan, Record, Retry, Rpc, Scenario, Steps};
use hydo_grpc::valuegen;
use prost_reflect::{DescriptorPool, FieldDescriptor, Kind, MessageDescriptor, MethodDescriptor, OneofDescriptor};
use std::collections::BTreeMap;

/// The user's hints (YAML or JSON). Every key is optional; unknown keys are errors.
#[derive(Clone, Debug, Default)]
pub struct Hints {
    pub target: String,
    pub deadline_ms: i64,
    pub seed: Option<u64>,
    pub services: Vec<String>,
    pub steps: Option<Steps>,
    pub drive: Option<Drive>,
    pub record: Option<Record>,
    pub defaults: Option<DefaultsHint>,
    pub rpcs: BTreeMap<String, RpcHint>,
    pub scenarios: Vec<ScenarioHint>,
}

#[derive(Clone, Debug, Default)]
pub struct DefaultsHint {
    pub string_len: Option<Vec<i64>>,
    pub bytes_len: Option<Vec<i64>>,
    pub repeated_len: Option<Vec<i64>>,
    pub map_len: Option<Vec<i64>>,
    pub depth: Option<i64>,
    pub message_prob: Option<f64>,
    pub optional_prob: Option<f64>,
    pub oneof_none: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct RpcHint {
    pub weight: Option<i64>,
    pub deadline_ms: i64,
    pub target: String,
    pub retry: Option<RetryHint>,
    pub outcome_id: String,
    pub classify: BTreeMap<String, String>,
    pub fields: BTreeMap<String, FieldHint>,
    pub oneofs: BTreeMap<String, Option<Vec<String>>>,
    pub stream_len: Option<Vec<i64>>,
}

#[derive(Clone, Debug, Default)]
pub struct RetryHint {
    pub attempts: i64,
    pub backoff_ms: i64,
    pub at_end: Option<bool>,
    pub end_tries: i64,
}

/// The YAML form of a field hint (values may be numbers, strings or booleans).
#[derive(Clone, Debug, Default)]
pub struct FieldHint {
    pub value: Option<Any>,
    pub choice: Vec<Any>,
    pub range: Option<Vec<Any>>,
    pub len: Option<Vec<i64>>,
    pub unique: bool,
    pub prefix: String,
    pub format: String,
    pub omit: bool,
    pub prob: Option<f64>,
    pub from: Option<Any>,
}

#[derive(Clone, Debug, Default)]
pub struct ScenarioHint {
    pub name: String,
    pub weight: i64,
    pub calls: Vec<String>,
}

fn ints(d: &mut D, n: &Node) -> Option<Vec<i64>> {
    d.seq(n, "[]int", |d, it, v: &mut i64| d.int(it, v))
}

fn strings(d: &mut D, n: &Node) -> Option<Vec<String>> {
    // a null element is dropped (a string cannot hold null), as yaml.v3 does
    d.seq(n, "[]string", |d, it, v: &mut String| {
        let before = d.errs.len();
        d.string(it, v);
        d.errs.len() == before && !is_null(it)
    })
}

fn anys(d: &mut D, n: &Node) -> Option<Vec<Any>> {
    match n {
        Node::Seq(items, _) => Some(items.iter().map(|i| d.any(i)).collect()),
        _ => {
            let a = d.any(n);
            if a == Any::Null {
                return None;
            }
            d.terror(n, "[]interface {}");
            None
        }
    }
}

fn string_map<T>(d: &mut D, n: &Node, ty: &str, mut val: impl FnMut(&mut D, &Node) -> T) -> BTreeMap<String, T> {
    let mut out = BTreeMap::new();
    let Some(pairs) = d.pairs(n, ty) else { return out };
    for (k, v) in pairs {
        if !matches!(k, Node::Scalar(..)) {
            d.terror(k, "string");
            continue;
        }
        let mut key = String::new();
        d.string(k, &mut key);
        out.insert(key, val(d, v));
    }
    out
}

fn decode_hints(d: &mut D, n: &Node) -> Hints {
    let mut h = Hints::default();
    d.strukt(n, "load.Hints", &["target", "deadline_ms", "seed", "services", "steps", "drive", "record", "defaults", "rpcs", "scenarios"], |d, k, v| match k {
        "target" => d.string(v, &mut h.target),
        "deadline_ms" => {
            d.int(v, &mut h.deadline_ms);
        }
        "seed" => d.opt_uint(v, &mut h.seed),
        "services" => h.services = strings(d, v).unwrap_or_default(),
        "steps" => {
            h.steps = if is_null(v) {
                None
            } else {
                let mut s = Steps::default();
                d.strukt(v, "spec.Steps", &["calls", "concurrency"], |d, k, v| {
                    if k == "calls" {
                        d.int(v, &mut s.calls);
                    } else {
                        d.int(v, &mut s.concurrency);
                    }
                });
                Some(s)
            }
        }
        "drive" => {
            h.drive = if is_null(v) {
                None
            } else {
                let mut s = Drive::default();
                d.strukt(v, "spec.Drive", &["calls"], |d, _, v| {
                    d.int(v, &mut s.calls);
                });
                Some(s)
            }
        }
        "record" => {
            h.record = if is_null(v) {
                None
            } else {
                let mut s = Record::default();
                d.strukt(v, "spec.Record", &["console", "history"], |d, k, v| {
                    if k == "console" {
                        d.string(v, &mut s.console)
                    } else {
                        d.string(v, &mut s.history)
                    }
                });
                Some(s)
            }
        }
        "defaults" => {
            h.defaults = if is_null(v) {
                None
            } else {
                let mut s = DefaultsHint::default();
                d.strukt(
                    v,
                    "load.DefaultsHint",
                    &["string_len", "bytes_len", "repeated_len", "map_len", "depth", "message_prob", "optional_prob", "oneof_none"],
                    |d, k, v| match k {
                        "string_len" => s.string_len = ints(d, v),
                        "bytes_len" => s.bytes_len = ints(d, v),
                        "repeated_len" => s.repeated_len = ints(d, v),
                        "map_len" => s.map_len = ints(d, v),
                        "depth" => d.opt_int(v, &mut s.depth),
                        "message_prob" => d.opt_float(v, &mut s.message_prob),
                        "optional_prob" => d.opt_float(v, &mut s.optional_prob),
                        _ => d.opt_float(v, &mut s.oneof_none),
                    },
                );
                Some(s)
            }
        }
        "rpcs" => h.rpcs = string_map(d, v, "map[string]load.RPCHint", decode_rpc),
        _ => {
            h.scenarios = d
                .seq(v, "[]load.ScenarioHint", |d, it, s: &mut ScenarioHint| {
                    d.strukt(it, "load.ScenarioHint", &["name", "weight", "calls"], |d, k, v| match k {
                        "name" => d.string(v, &mut s.name),
                        "weight" => {
                            d.int(v, &mut s.weight);
                        }
                        _ => s.calls = strings(d, v).unwrap_or_default(),
                    });
                    true
                })
                .unwrap_or_default()
        }
    });
    h
}

fn is_null(n: &Node) -> bool {
    matches!(n, Node::Scalar(v, ind, tag, _) if yaml::scalar_of(v, *ind, tag).1 == yaml::R::Null)
}

fn decode_rpc(d: &mut D, n: &Node) -> RpcHint {
    let mut r = RpcHint::default();
    d.strukt(
        n,
        "load.RPCHint",
        &["weight", "deadline_ms", "target", "retry", "outcome_id", "classify", "fields", "oneofs", "stream_len"],
        |d, k, v| match k {
            "weight" => d.opt_int(v, &mut r.weight),
            "deadline_ms" => {
                d.int(v, &mut r.deadline_ms);
            }
            "target" => d.string(v, &mut r.target),
            "retry" => {
                r.retry = if is_null(v) {
                    None
                } else {
                    let mut x = RetryHint::default();
                    d.strukt(v, "load.RetryHint", &["attempts", "backoff_ms", "at_end", "end_tries"], |d, k, v| match k {
                        "attempts" => {
                            d.int(v, &mut x.attempts);
                        }
                        "backoff_ms" => {
                            d.int(v, &mut x.backoff_ms);
                        }
                        "at_end" => d.opt_bool(v, &mut x.at_end),
                        _ => {
                            d.int(v, &mut x.end_tries);
                        }
                    });
                    Some(x)
                }
            }
            "outcome_id" => d.string(v, &mut r.outcome_id),
            "classify" => {
                r.classify = string_map(d, v, "map[string]string", |d, v| {
                    let mut s = String::new();
                    d.string(v, &mut s);
                    s
                })
            }
            "fields" => r.fields = string_map(d, v, "map[string]load.FieldHint", decode_field),
            "oneofs" => r.oneofs = string_map(d, v, "map[string][]string", strings),
            _ => r.stream_len = ints(d, v),
        },
    );
    r
}

fn decode_field(d: &mut D, n: &Node) -> FieldHint {
    let mut f = FieldHint::default();
    d.strukt(
        n,
        "load.FieldHint",
        &["value", "choice", "range", "len", "unique", "prefix", "format", "omit", "prob", "from"],
        |d, k, v| match k {
            "value" => f.value = Some(d.any(v)).filter(|a| *a != Any::Null),
            "choice" => f.choice = anys(d, v).unwrap_or_default(),
            "range" => f.range = anys(d, v),
            "len" => f.len = ints(d, v),
            "unique" => d.bool(v, &mut f.unique),
            "prefix" => d.string(v, &mut f.prefix),
            "format" => d.string(v, &mut f.format),
            "omit" => d.bool(v, &mut f.omit),
            "prob" => d.opt_float(v, &mut f.prob),
            _ => f.from = Some(d.any(v)).filter(|a| *a != Any::Null),
        },
    );
    f
}

/// Parses a hints file ("" = no hints).
pub fn read_hints(path: &str) -> Result<Hints, String> {
    if path.is_empty() {
        return Ok(Hints::default());
    }
    let b = std::fs::read(path).map_err(|e| format!("open {}: {}", path, hydo_grpc::io_msg(&e)))?;
    let text = String::from_utf8_lossy(&b);
    let root = yaml::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    let Some(root) = root else { return Ok(Hints::default()) };
    let mut d = D::default();
    let h = match &root {
        Node::Scalar(..) if is_null(&root) => Hints::default(),
        _ => decode_hints(&mut d, &root),
    };
    if !d.errs.is_empty() {
        return Err(format!("{}: yaml: unmarshal errors:\n  {}", path, d.errs.join("\n  ")));
    }
    Ok(h)
}

fn history_ok(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'-')
}

/// The method name in snake case.
pub fn snake(s: &str) -> String {
    let rs: Vec<char> = s.chars().collect();
    let mut b = String::new();
    for (i, &c) in rs.iter().enumerate() {
        if c.is_uppercase() {
            if i > 0
                && (rs[i - 1].is_lowercase()
                    || rs[i - 1].is_numeric()
                    || (i + 1 < rs.len() && rs[i + 1].is_lowercase() && rs[i - 1].is_uppercase()))
            {
                b.push('_');
            }
            b.extend(c.to_lowercase());
        } else {
            b.push(c);
        }
    }
    b
}

fn pair(xs: &[i64], what: &str, min: i64) -> Result<[i64; 2], String> {
    if xs.len() != 2 || xs[0] < min || xs[1] < xs[0] {
        return Err(format!("{what}: want [lo, hi] with {min} <= lo <= hi"));
    }
    Ok([xs[0], xs[1]])
}

fn skip_service(name: &str) -> bool {
    name.starts_with("grpc.reflection.") || name.starts_with("grpc.health.")
}

/// Turns the schema and the hints into a plan.
pub fn build(pool: &DescriptorPool, h: &Hints, sources: Option<Vec<String>>) -> Result<Plan, String> {
    let mut p = Plan {
        format: spec::FORMAT,
        target: "127.0.0.1:50051".into(),
        deadline_ms: 2000,
        seed: 1,
        steps: Steps { calls: 2, concurrency: 1 },
        drive: Drive { calls: 2 },
        defaults: spec::default_defaults(),
        record: Record { console: "brief".into(), history: "grpc".into() },
        sources,
        ..Default::default()
    };
    if !h.target.is_empty() {
        p.target = h.target.clone();
    }
    if h.deadline_ms > 0 {
        p.deadline_ms = h.deadline_ms;
    }
    if let Some(s) = h.seed {
        p.seed = s;
    }
    if let Some(s) = &h.steps {
        if s.calls < 1 || s.calls > 1000 || s.concurrency < 1 || s.concurrency > 64 {
            return Err("steps: calls must be 1..1000 and concurrency 1..64".into());
        }
        p.steps = s.clone();
    }
    if let Some(s) = &h.drive {
        if s.calls < 1 || s.calls > 1000 {
            return Err("drive: calls must be 1..1000".into());
        }
        p.drive = s.clone();
    }
    if let Some(r) = &h.record {
        if !r.console.is_empty() {
            match r.console.as_str() {
                "off" | "brief" | "full" => p.record.console = r.console.clone(),
                _ => return Err(format!("record.console: {} (off, brief, full)", quote(&r.console))),
            }
        }
        if !r.history.is_empty() {
            if !history_ok(&r.history) {
                return Err(format!("record.history: {} ([A-Za-z0-9_.-]+)", quote(&r.history)));
            }
            p.record.history = r.history.clone();
        }
    }
    apply_defaults(&mut p.defaults, h.defaults.as_ref())?;

    // every service in the schema, in a fixed order
    let mut svcs: Vec<_> = pool.services().collect();
    svcs.sort_by(|a, b| a.full_name().cmp(b.full_name()));
    let mut want: BTreeMap<String, bool> = h.services.iter().map(|s| (s.clone(), true)).collect();
    let mut mds: Vec<MethodDescriptor> = Vec::new();
    for s in &svcs {
        if skip_service(s.full_name()) {
            continue;
        }
        // As in the Go generator, the filter applies while a wanted service is still missing:
        // the services after the last wanted one (in name order) are driven too.
        if !want.is_empty() && !want.contains_key(s.full_name()) && !want.contains_key(s.name()) {
            continue;
        }
        want.remove(s.full_name());
        want.remove(s.name());
        mds.extend(s.methods());
    }
    if !want.is_empty() {
        return Err(format!("services: {} not in the schema", want.keys().cloned().collect::<Vec<_>>().join(", ")));
    }
    if mds.is_empty() {
        return Err("the schema has no services (nothing to drive)".into());
    }
    // command names: the method in snake case, or service_method where two collide
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for m in &mds {
        *count.entry(snake(m.name())).or_default() += 1;
    }
    let mut by_md: BTreeMap<String, MethodDescriptor> = BTreeMap::new();
    for m in &mds {
        let full = format!("{}/{}", m.parent_service().full_name(), m.name());
        let mut short = snake(m.name());
        if count[&short] > 1 {
            short = format!("{}_{}", snake(m.parent_service().name()), short);
        }
        p.rpcs.push(Rpc {
            name: full.clone(),
            path: format!("/{full}"),
            short,
            input: m.input().full_name().to_string(),
            output: m.output().full_name().to_string(),
            client_streaming: m.is_client_streaming(),
            server_streaming: m.is_server_streaming(),
            weight: 1,
            deadline_ms: p.deadline_ms,
            stream_len: [1, 3],
            ..Default::default()
        });
        by_md.insert(full, m.clone());
    }

    let mut used: Vec<String> = Vec::new();
    for (key, rh) in &h.rpcs {
        let i = p.find(key).map_err(|e| format!("rpcs.{key}: {e}"))?;
        let name = p.rpcs[i].name.clone();
        if used.contains(&name) {
            return Err(format!("rpcs.{key}: {name} has hints twice"));
        }
        used.push(name.clone());
        let md = by_md[&name].clone();
        let mut r = p.rpcs[i].clone();
        apply_rpc(&p, &mut r, &md, rh, pool).map_err(|e| format!("rpcs.{key}: {e}"))?;
        p.rpcs[i] = r;
    }
    for sh in &h.scenarios {
        if sh.name.is_empty() || sh.calls.is_empty() {
            return Err("scenarios: every scenario needs a name and calls".into());
        }
        let mut s = Scenario { name: sh.name.clone(), weight: if sh.weight == 0 { 1 } else { sh.weight }, calls: Vec::new() };
        for c in &sh.calls {
            let i = p.find(c).map_err(|e| format!("scenarios.{}: {}", sh.name, e))?;
            s.calls.push(p.rpcs[i].name.clone());
        }
        p.scenarios.push(s);
    }
    Ok(p)
}

fn apply_defaults(d: &mut spec::Defaults, h: Option<&DefaultsHint>) -> Result<(), String> {
    let Some(h) = h else { return Ok(()) };
    for (v, dst, name) in [
        (&h.string_len, &mut d.string_len, "string_len"),
        (&h.bytes_len, &mut d.bytes_len, "bytes_len"),
        (&h.repeated_len, &mut d.repeated_len, "repeated_len"),
        (&h.map_len, &mut d.map_len, "map_len"),
    ] {
        if let Some(v) = v {
            *dst = pair(v, &format!("defaults.{name}"), 0)?;
        }
    }
    if let Some(x) = h.depth {
        if !(0..=32).contains(&x) {
            return Err("defaults.depth: 0..32".into());
        }
        d.depth = x;
    }
    for (v, dst, name) in [
        (h.message_prob, &mut d.message_prob, "message_prob"),
        (h.optional_prob, &mut d.optional_prob, "optional_prob"),
        (h.oneof_none, &mut d.oneof_none, "oneof_none"),
    ] {
        if let Some(v) = v {
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("defaults.{name}: 0..1"));
            }
            *dst = v;
        }
    }
    Ok(())
}

fn apply_rpc(p: &Plan, r: &mut Rpc, md: &MethodDescriptor, h: &RpcHint, pool: &DescriptorPool) -> Result<(), String> {
    if let Some(w) = h.weight {
        if w < 0 {
            return Err("weight: >= 0".into());
        }
        r.weight = w;
    }
    if h.deadline_ms > 0 {
        r.deadline_ms = h.deadline_ms;
    }
    r.target = h.target.clone();
    if let Some(x) = &h.retry {
        if x.attempts < 0 || x.attempts > 100 || x.backoff_ms < 0 || x.end_tries < 0 || x.end_tries > 100 {
            return Err("retry: attempts 0..100, end_tries 0..100, backoff_ms >= 0".into());
        }
        r.retry = Retry { attempts: x.attempts, backoff_ms: x.backoff_ms, at_end: x.at_end.unwrap_or(true), end_tries: x.end_tries };
    }
    if let Some(s) = &h.stream_len {
        r.stream_len = pair(s, "stream_len", 0)?;
    }
    let input = md.input();
    if !h.outcome_id.is_empty() {
        let (fd, rep) = resolve(&input, &h.outcome_id).map_err(|e| format!("outcome_id: {e}"))?;
        if rep || matches!(fd.kind(), Kind::Message(_)) {
            return Err(format!("outcome_id: {} must be a single scalar field", h.outcome_id));
        }
        r.outcome_id = h.outcome_id.clone();
    }
    for (c, v) in &h.classify {
        if outcome::parse_code(c).is_none() {
            return Err(format!("classify: {} is not a gRPC code name ({})", quote(c), outcome::code_names().join(" ")));
        }
        if !outcome::valid_override(v) {
            return Err(format!("classify.{}: {} (ok, fail, info)", c, quote(v)));
        }
    }
    r.classify = h.classify.clone();
    for (path, members) in &h.oneofs {
        let od = resolve_oneof(&input, path).map_err(|e| format!("oneofs.{path}: {e}"))?;
        for m in members.iter().flatten() {
            if !od.fields().any(|f| f.name() == m) {
                return Err(format!("oneofs.{}: {} is not a member of the oneof", path, quote(m)));
            }
        }
    }
    r.oneofs = h.oneofs.clone();
    for (path, fh) in &h.fields {
        let (fd, _) = resolve(&input, path).map_err(|e| format!("fields.{path}: {e}"))?;
        let x = field_hint(p, &fd, fh, pool).map_err(|e| format!("fields.{path}: {e}"))?;
        if let Some(f) = &x.from {
            if f.fallback != "generate" && !r.needs.contains(&f.pool()) {
                r.needs.push(f.pool());
            }
        }
        r.fields.insert(path.clone(), x);
    }
    Ok(())
}

/// The field names of a message in declaration order.
fn field_names(md: &MessageDescriptor) -> String {
    valuegen::decl_fields(md).iter().map(|f| f.name().to_string()).collect::<Vec<_>>().join(", ")
}

fn message_of(fd: &FieldDescriptor) -> Option<MessageDescriptor> {
    match fd.kind() {
        Kind::Message(m) => Some(m),
        _ => None,
    }
}

/// Walks a field path ("items.sku", "tags.key") from md; the bool is true if any step is a
/// repeated or map field.
pub fn resolve(md: &MessageDescriptor, path: &str) -> Result<(FieldDescriptor, bool), String> {
    let segs: Vec<&str> = path.split('.').collect();
    let mut cur = Some(md.clone());
    let mut rep = false;
    let mut fd: Option<FieldDescriptor> = None;
    let mut i = 0;
    while i < segs.len() {
        let Some(c) = cur.clone() else {
            return Err(format!("{}: {} is not a message", path, segs[..i].join(".")));
        };
        let Some(f) = c.get_field_by_name(segs[i]) else {
            return Err(format!("{}: no field {} in {} (fields: {})", path, quote(segs[i]), c.full_name(), field_names(&c)));
        };
        let mut f = f;
        if f.is_list() {
            rep = true;
        }
        if f.is_map() {
            rep = true;
            if i + 1 < segs.len() {
                i += 1;
                let entry = message_of(&f).expect("map entry");
                f = match segs[i] {
                    "key" => entry.map_entry_key_field(),
                    "value" => entry.map_entry_value_field(),
                    _ => return Err(format!("{}: after the map field {} comes key or value", path, segs[i - 1])),
                };
            }
        }
        cur = message_of(&f);
        fd = Some(f);
        i += 1;
    }
    Ok((fd.expect("a path has a segment"), rep))
}

fn resolve_oneof(md: &MessageDescriptor, path: &str) -> Result<OneofDescriptor, String> {
    let segs: Vec<&str> = path.split('.').collect();
    let mut cur = md.clone();
    if segs.len() > 1 {
        let head = segs[..segs.len() - 1].join(".");
        let (fd, _) = resolve(md, &head)?;
        cur = message_of(&fd).ok_or_else(|| format!("{head} is not a message"))?;
    }
    let last = segs[segs.len() - 1];
    let found = cur.oneofs().find(|o| o.name() == last);
    match found {
        Some(od) if !od.is_synthetic() => Ok(od),
        _ => Err(format!("no oneof {} in {}", quote(last), cur.full_name())),
    }
}

/// `str`: a YAML scalar as the hint's string form.
fn str_of(v: &Any) -> Result<String, String> {
    match v {
        Any::Str(s) => Ok(s.clone()),
        Any::Bool(b) => Ok(b.to_string()),
        Any::Int(i) => Ok(i.to_string()),
        Any::Uint(u) => Ok(u.to_string()),
        Any::Float(f) => Ok(gofmt::format_g(*f, 64)),
        _ => Err(format!("{} ({}) is not a scalar value", v.go_v(), v.go_t())),
    }
}

fn is_msg_kind(k: &Kind) -> bool {
    matches!(k, Kind::Message(_))
}

fn numeric(k: &Kind) -> bool {
    !matches!(k, Kind::Bool | Kind::String | Kind::Bytes | Kind::Enum(_) | Kind::Message(_))
}

fn integer(k: &Kind) -> bool {
    numeric(k) && !matches!(k, Kind::Float | Kind::Double)
}

/// protoreflect.Kind's String.
fn kind_word(fd: &FieldDescriptor) -> &'static str {
    match fd.kind() {
        Kind::Double => "double",
        Kind::Float => "float",
        Kind::Int64 => "int64",
        Kind::Uint64 => "uint64",
        Kind::Int32 => "int32",
        Kind::Fixed64 => "fixed64",
        Kind::Fixed32 => "fixed32",
        Kind::Bool => "bool",
        Kind::String => "string",
        Kind::Message(_) if fd.is_group() => "group",
        Kind::Message(_) => "message",
        Kind::Bytes => "bytes",
        Kind::Uint32 => "uint32",
        Kind::Enum(_) => "enum",
        Kind::Sfixed32 => "sfixed32",
        Kind::Sfixed64 => "sfixed64",
        Kind::Sint32 => "sint32",
        Kind::Sint64 => "sint64",
    }
}

fn kind_name(fd: &FieldDescriptor) -> String {
    if fd.is_map() {
        return "a map".into();
    }
    let mut s = kind_word(fd).to_string();
    if fd.is_list() {
        s = format!("repeated {s}");
    }
    match fd.kind() {
        Kind::Message(m) => format!("{} {}", s, m.full_name()),
        Kind::Enum(e) => format!("{} {}", s, e.full_name()),
        _ => s,
    }
}

fn compatible(src: &FieldDescriptor, dst: &FieldDescriptor) -> bool {
    let (sk, dk) = (src.kind(), dst.kind());
    if src.is_map() || dst.is_map() {
        return false;
    }
    match &dk {
        Kind::String => !matches!(sk, Kind::Message(_) | Kind::Bytes),
        Kind::Enum(de) => matches!(&sk, Kind::Enum(se) if se.full_name() == de.full_name()),
        k if integer(k) => integer(&sk),
        k if numeric(k) => numeric(&sk),
        _ => kind_word(src) == kind_word(dst) && std::mem::discriminant(&sk) == std::mem::discriminant(&dk),
    }
}

fn field_hint(p: &Plan, fd: &FieldDescriptor, h: &FieldHint, pool: &DescriptorPool) -> Result<SpecHint, String> {
    let mut out = SpecHint::default();
    let k = fd.kind();
    let is_msg = is_msg_kind(&k);
    let scalar_only = |what: &str| -> Result<(), String> {
        if is_msg || fd.is_map() {
            return Err(format!("{} needs a scalar or enum field, {} is {}", what, fd.name(), kind_name(fd)));
        }
        Ok(())
    };
    if let Some(v) = &h.value {
        scalar_only("value")?;
        let s = str_of(v).map_err(|e| format!("value: {e}"))?;
        valuegen::parse(fd, &s)?;
        out.value = Some(s);
    }
    for c in &h.choice {
        scalar_only("choice")?;
        let s = str_of(c).map_err(|e| format!("choice: {e}"))?;
        valuegen::parse(fd, &s)?;
        out.choice.push(s);
    }
    if let Some(range) = &h.range {
        if !numeric(&k) || fd.is_map() {
            return Err(format!("range needs a number field, {} is {}", fd.name(), kind_name(fd)));
        }
        if range.len() != 2 {
            return Err("range: want [lo, hi]".into());
        }
        for v in range {
            let s = str_of(v).map_err(|e| format!("range: {e}"))?;
            valuegen::parse(fd, &s).map_err(|e| format!("range: {e}"))?;
            out.range.push(s);
        }
        let (a, b) = (&out.range[0], &out.range[1]);
        let bad = if integer(&k) {
            if a.starts_with('-') || b.starts_with('-') {
                gofmt::parse_int(a, 64).unwrap_or(0) > gofmt::parse_int(b, 64).unwrap_or(0)
            } else {
                gofmt::parse_uint(a, 64).unwrap_or(0) > gofmt::parse_uint(b, 64).unwrap_or(0)
            }
        } else {
            gofmt::parse_float(a, 64).unwrap_or(0.0) > gofmt::parse_float(b, 64).unwrap_or(0.0)
        };
        if bad {
            return Err("range: lo > hi".into());
        }
    }
    if let Some(l) = &h.len {
        if !(fd.is_list() || fd.is_map() || matches!(k, Kind::String | Kind::Bytes)) {
            return Err(format!("len needs a string, bytes, repeated or map field, {} is {}", fd.name(), kind_name(fd)));
        }
        let pr = pair(l, "len", 0)?;
        out.len = vec![pr[0], pr[1]];
    }
    if h.unique {
        if !(matches!(k, Kind::String | Kind::Bytes) || integer(&k)) || fd.is_map() {
            return Err(format!("unique needs a string, bytes or integer field, {} is {}", fd.name(), kind_name(fd)));
        }
        out.unique = true;
    }
    if !h.prefix.is_empty() {
        if !matches!(k, Kind::String | Kind::Bytes) {
            return Err("prefix needs a string field".into());
        }
        out.prefix = h.prefix.clone();
    }
    if !h.format.is_empty() {
        if !valuegen::FORMATS.contains(&h.format.as_str()) || !matches!(k, Kind::String) {
            return Err(format!("format {}: a string field and one of alnum lower digits hex uuid", quote(&h.format)));
        }
        out.format = h.format.clone();
    }
    out.omit = h.omit;
    if let Some(pr) = h.prob {
        if !(0.0..=1.0).contains(&pr) {
            return Err("prob: 0..1".into());
        }
        out.prob = Some(pr);
    }
    if let Some(fv) = &h.from {
        scalar_only("from")?;
        let mut f = parse_from(fv).map_err(|e| format!("from: {e}"))?;
        let si = p.find(&f.rpc).map_err(|e| format!("from: {e}"))?;
        let src = &p.rpcs[si];
        f.rpc = src.name.clone();
        let md_name = if f.side == "request" { &src.input } else { &src.output };
        let d = pool.get_message_by_name(md_name).ok_or_else(|| format!("from: {md_name}: not found"))?;
        let (sfd, _) = resolve(&d, &f.path).map_err(|e| format!("from: {e}"))?;
        if !compatible(&sfd, fd) {
            return Err(format!("from: {} {}.{} is {}, this field is {}", src.name, f.side, f.path, kind_name(&sfd), kind_name(fd)));
        }
        out.from = Some(f);
    }
    Ok(out)
}

/// `parseFrom`: "Create.id", "Create.request.request_id", "pkg.S/M.response.id", or a mapping.
pub fn parse_from(v: &Any) -> Result<From, String> {
    let mut f = From { side: "response".into(), pick: "random".into(), fallback: "skip".into(), ..Default::default() };
    match v {
        Any::Str(x) => {
            let cut = x.find('/').unwrap_or(0);
            let Some(j) = x[cut..].find('.') else {
                return Err(format!("{}: want <rpc>.<field path>", quote(x)));
            };
            f.rpc = x[..cut + j].to_string();
            f.path = x[cut + j + 1..].to_string();
            for side in ["response", "request"] {
                if let Some(rest) = f.path.strip_prefix(&format!("{side}.")) {
                    f.side = side.into();
                    f.path = rest.to_string();
                }
            }
        }
        Any::Map(pairs, true) => {
            let mut kv: Vec<(String, &Any)> = pairs.iter().map(|(k, v)| (str_of(k).unwrap_or_default(), v)).collect();
            kv.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, val) in kv {
                let s = str_of(val).map_err(|e| format!("{k}: {e}"))?;
                match k.as_str() {
                    "rpc" => f.rpc = s,
                    "path" => f.path = s,
                    "side" => f.side = s,
                    "pick" => f.pick = s,
                    "fallback" => f.fallback = s,
                    _ => return Err(format!("unknown key {} (rpc, path, side, pick, fallback)", quote(&k))),
                }
            }
        }
        _ => return Err("want a string or a mapping".into()),
    }
    if f.rpc.is_empty() || f.path.is_empty() {
        return Err("needs an rpc and a field path".into());
    }
    if f.side != "response" && f.side != "request" {
        return Err(format!("side {} (request, response)", quote(&f.side)));
    }
    if f.pick != "random" && f.pick != "latest" {
        return Err(format!("pick {} (random, latest)", quote(&f.pick)));
    }
    if f.fallback != "skip" && f.fallback != "generate" {
        return Err(format!("fallback {} (skip, generate)", quote(&f.fallback)));
    }
    Ok(f)
}
