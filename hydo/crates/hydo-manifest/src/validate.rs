//! hydo-build's `apply_params`, `validate` (with `validate_network`, `validate_addr`,
//! `validate_checks`, `validate_world`, `validate_errors`), `check_ram_arch` and `service_order`,
//! ported with the same acceptance, the same messages and the same changes to the data.

use serde_json::{Map, Value};

use crate::py::{
    self, crash, d, eq, get, has, in_strs, index, is_int, is_plain_int, repr, repr_str, str_of,
    truthy,
};
use crate::pyre;
use crate::rx;
use crate::ManifestError;

pub type R<T> = Result<T, ManifestError>;

pub fn err(msg: impl Into<String>) -> ManifestError {
    ManifestError {
        message: msg.into(),
    }
}

pub const NAME_RE: &str = r"[a-z][a-z0-9_]*";
pub const KNOWN_TOP: &[&str] = &[
    "name",
    "description",
    "guest",
    "root",
    "data",
    "env",
    "shell",
    "services",
    "provision",
    "workload",
    "knobs",
    "invariants",
    "faults",
    "freeze_match",
    "image",
    "params",
    "fakes",
    "proxies",
    "buggify",
    "test",
    "format",
    "hydo_version",
    "checks",
    "network",
    "errors",
];
pub const KNOWN_SVC: &[&str] = &[
    "name",
    "cmd",
    "setup",
    "ready",
    "ready_timeout",
    "port",
    "depends_on",
    "recovery",
    "description",
    "addr",
    "errors",
    "image",
    "image_ref",
    "entrypoint",
    "workdir",
    "user",
    "env",
    "mounts",
    "restart",
    "restart_may_fail",
    "writable",
    "storage",
    "start_after",
    "logs",
];
pub const KNOWN_INV: &[&str] = &[
    "id",
    "type",
    "msg",
    "cmd",
    "every",
    "final",
    "description",
    "within",
    "after",
    "poll",
];
pub const LIVE_ANCHORS: &[&str] = &["heal", "start"];
pub(crate) const KNOWN_WORKLOAD: &[&str] =
    &["steps", "run", "retry", "process", "timeout", "description"];
pub(crate) const KNOWN_DATA: &[&str] = &["size_mib", "mount", "dirs", "description"];
pub(crate) const KNOWN_ROOT: &[&str] = &[
    "size_mib",
    "nix",
    "files",
    "text",
    "dirs",
    "share_images",
    "fix_pyc",
    "description",
];
pub(crate) const KNOWN_GUEST: &[&str] = &[
    "ram_mib",
    "clock_start",
    "clock_after_cut",
    "outcomes_after_cut",
    "cpu",
    "description",
];
pub const KNOWN_KNOB: &[&str] = &[
    "token",
    "name",
    "default",
    "set",
    "values",
    "safe",
    "description",
];
pub const FAULT_KINDS: &[&str] = &[
    "kill",
    "pause",
    "network",
    "oneway",
    "cut",
    "clock",
    "disk",
    "freeze",
    "protocol",
    "corrupt",
    "powerloss",
];
pub const NET_MODES: &[&str] = &["loopback", "bridge"];
pub const NET_BRIDGE: &str = "10.89.0.0/16";
pub const NET_GW: &str = "10.89.0.1";
pub const RAM_MIB_MIN: i128 = 256;
/// Guest RAM at most, in MiB (8 GiB on every engine since Milestone 179; before it 3072 on
/// x86_64 and 4096 on arm64).
pub const RAM_MIB_MAX: i128 = 8192;
pub const RAM_MIB_MAX_X86: i128 = RAM_MIB_MAX;
pub const RAM_MIB_MAX_ARM: i128 = RAM_MIB_MAX;
pub(crate) const KNOWN_ERRORS_TOP: &[&str] = &["level", "max", "services", "description"];
pub(crate) const KNOWN_ERRORS_SVC: &[&str] = &[
    "format",
    "level",
    "match",
    "ignore",
    "files",
    "normalize",
    "off",
    "description",
];
const ERR_FORMATS: &[&str] = &["auto", "redis", "none"];
pub(crate) const KNOWN_FAKE: &[&str] = &[
    "name",
    "port",
    "addr",
    "protocol",
    "idempotency",
    "ref",
    "routes",
    "tls",
    "description",
];
pub(crate) const KNOWN_PROXY: &[&str] = &[
    "name",
    "port",
    "addr",
    "protocol",
    "idempotency",
    "ref",
    "routes",
    "to",
    "service",
    "tls",
    "description",
];
pub(crate) const KNOWN_TLS: &[&str] = &["hosts", "port", "description"];
/// A host name a `tls:` endpoint answers for (lower case, as DNS names are compared).
const HOST_RE: &str = r"[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*";
pub(crate) const KNOWN_ROUTE: &[&str] = &[
    "name",
    "method",
    "path",
    "status",
    "code",
    "headers",
    "body",
    "message",
    "ref",
    "effect",
    "idempotent",
    "webhook",
    "save",
    "load",
    "description",
];
pub(crate) const KNOWN_SAVE: &[&str] = &["key", "fields"];
pub(crate) const KNOWN_SIGN: &[&str] = &["header", "secret", "payload", "value", "encoding"];
pub(crate) const KNOWN_PROXY_ROUTE: &[&str] =
    &["name", "method", "path", "ref", "effect", "description"];
pub(crate) const KNOWN_HOOK: &[&str] = &[
    "url",
    "method",
    "body",
    "headers",
    "delay_ms",
    "retries",
    "sign",
    "description",
];
// `query:` and `form:` names may carry brackets: the nested form encoding that Stripe, Rails and
// PHP use (`metadata[order_id]=o7`); hydo-world matches the decoded key as written.
const EXTRACT_RE: &str = r"(header:[A-Za-z0-9-]+|json:[A-Za-z0-9_.]+|query:[A-Za-z0-9_.\[\]-]+|form:[A-Za-z0-9_.\[\]-]+|path:[A-Za-z0-9_]+|pb:[1-9][0-9]*|body)";
const HOOK_VARS: &[&str] = &["seq", "ref", "idem", "fake", "route"];
pub(crate) const KNOWN_CHECK: &[&str] = &[
    "id",
    "type",
    "history",
    "model",
    "initial",
    "consistency",
    "msg",
    "final",
    "description",
];
const CHECK_TYPES: &[&str] = &["linearizable", "list-append"];
const CHECK_LEVELS: &[&str] = &[
    "strict-serializable",
    "strong-session-serializable",
    "serializable",
    "strong-snapshot-isolation",
    "strong-session-snapshot-isolation",
    "snapshot-isolation",
];

/// Iterating a value as Python does: a list's items, a string's characters, a mapping's keys.
pub fn iter_of(v: &Value) -> R<Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        _ => Err(crash(format!(
            "{} object is not iterable",
            repr_str(py::type_name(v))
        ))),
    }
}

/// `x or []` iterated.
pub fn or_iter(v: Option<&Value>) -> R<Vec<Value>> {
    match v {
        Some(x) if truthy(x) => iter_of(x),
        _ => Ok(Vec::new()),
    }
}

/// `x or {}`.
fn or_map(v: Option<&Value>) -> Value {
    match v {
        Some(x) if truthy(x) => x.clone(),
        _ => Value::Object(Map::new()),
    }
}

fn joined(xs: &[&str]) -> String {
    let mut v: Vec<&str> = xs.to_vec();
    v.sort();
    v.join(", ")
}

/// `_unknown(d, known, where)`.
pub fn unknown(dv: &Value, known: &[&str], wh: &str) -> R<()> {
    let keys = py::keys_of(dv)?;
    let bad: Vec<Value> = keys.into_iter().filter(|k| !in_strs(k, known)).collect();
    // a set: duplicates collapse
    let mut uniq: Vec<Value> = Vec::new();
    for b in bad {
        if !py::contains(&uniq, &b) {
            uniq.push(b);
        }
    }
    let bad = py::sorted_names(uniq)?;
    if !bad.is_empty() {
        return Err(err(format!(
            "{}: unknown field(s) {} (known: {})",
            wh,
            bad.iter().map(str_of).collect::<Vec<_>>().join(", "),
            joined(known)
        )));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Typ {
    Str,
    Int,
}

/// `_need(d, k, where, typ)`: the value (a str field's int/bool value comes back as its str()).
pub fn need(dv: &Value, k: &str, wh: &str, typ: Typ) -> R<Value> {
    let present = has(dv, k)?;
    let v = if present { Some(index(dv, k)?) } else { None };
    match v {
        None | Some(Value::Null) => return Err(err(format!("{wh}: missing required field '{k}'"))),
        Some(Value::String(s)) if s.is_empty() => {
            return Err(err(format!("{wh}: missing required field '{k}'")))
        }
        _ => {}
    }
    let v = v.cloned().unwrap_or(Value::Null);
    match typ {
        Typ::Str => {
            if is_int(&v) {
                return Ok(Value::String(str_of(&v)));
            }
            if !v.is_string() {
                return Err(err(format!("{wh}: field '{k}' must be a str")));
            }
            Ok(v)
        }
        Typ::Int => {
            if !is_int(&v) {
                return Err(err(format!("{wh}: field '{k}' must be a int")));
            }
            Ok(v)
        }
    }
}

fn need_str(dv: &Value, k: &str, wh: &str) -> R<String> {
    Ok(need(dv, k, wh, Typ::Str)?
        .as_str()
        .unwrap_or_default()
        .to_string())
}

fn obj_get<'a>(m: &'a Value, k: &str) -> Option<&'a Value> {
    m.as_object().and_then(|o| o.get(k))
}

fn has_netns(m: &Value) -> bool {
    obj_get(m, "network").is_some_and(|v| eq(v, &Value::String("bridge".into())))
}

fn bridge_addr_ok(a: &str) -> bool {
    match pyre::match_dollar(r"10\.89\.(\d{1,3})\.(\d{1,3})", a) {
        Some(g) => {
            g.iter().all(|x| {
                x.as_deref()
                    .and_then(|s| s.parse::<u32>().ok())
                    .is_some_and(|n| n <= 255)
            }) && !["10.89.0.0", NET_GW, "10.89.255.255"].contains(&a)
        }
        None => false,
    }
}

fn addrs_of(svcs: &[Value]) -> R<Vec<String>> {
    let mut out = Vec::new();
    for x in svcs {
        out.push(str_of(get(x, "addr")?.unwrap_or(&Value::Null)));
    }
    Ok(out)
}

fn validate_addr(s: &Value, svcs: &[Value], wh: &str, bridge: bool) -> R<()> {
    let Some(av) = obj_get(s, "addr") else {
        return Ok(());
    };
    let a = str_of(av);
    if bridge {
        if !bridge_addr_ok(&a) {
            return Err(err(format!(
                "{}: addr {}: with network: bridge, an address in {} other than {} (the bridge's own address) and the network/broadcast addresses",
                wh,
                repr_str(&a),
                NET_BRIDGE,
                NET_GW
            )));
        }
        if addrs_of(svcs)?.iter().filter(|x| **x == a).count() > 1 {
            return Err(err(format!("{wh}: addr {a} is used by two services")));
        }
        return Ok(());
    }
    let ok = match pyre::match_dollar(r"127\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})", &a) {
        Some(g) => g.iter().all(|x| {
            x.as_deref()
                .and_then(|s| s.parse::<u32>().ok())
                .is_some_and(|n| n <= 255)
        }),
        None => false,
    };
    if !ok || a == "127.0.0.1" {
        return Err(err(format!(
            "{}: addr {} must be a loopback address 127.x.y.z other than 127.0.0.1 (127.0.0.1 is the clients' and the init's own source address)",
            wh,
            repr_str(&a)
        )));
    }
    if addrs_of(svcs)?.iter().filter(|x| **x == a).count() > 1 {
        return Err(err(format!("{wh}: addr {a} is used by two services")));
    }
    Ok(())
}

fn int_in(v: &Value, lo: i128, hi: i128) -> bool {
    is_plain_int(v) && py::int_of(v).is_some_and(|i| lo <= i && i <= hi)
}

/// `validate(m)`: checks and normalises m in place (services/invariants default to lists,
/// `errors: true` becomes a mapping).
pub fn validate(m: &mut Value) -> R<()> {
    if !m.is_object() {
        return Err(err("the manifest must be a mapping"));
    }
    unknown(m, KNOWN_TOP, "top level")?;
    if let Some(b) = obj_get(m, "buggify") {
        if !b.is_boolean() {
            return Err(err("buggify: must be true or false"));
        }
    }
    if obj_get(m, "buggify") == Some(&Value::Bool(true)) {
        for k in or_iter(obj_get(m, "knobs"))? {
            if k.is_object() && str_of(obj_get(&k, "token").unwrap_or(&Value::Null)) == "B" {
                return Err(err(
                    "buggify: true reserves the schedule token B (the BUGGIFY seed); rename the knob B",
                ));
            }
        }
    }
    if obj_get(m, "image").is_some() {
        return Err(err(
            "top level: 'image:' belongs to a service (services[].image: a Docker/OCI image file; MANIFEST.md)",
        ));
    }
    let name = need_str(m, "name", "top level")?;
    if !pyre::fullmatch(r"[a-z][a-z0-9-]*", &name) {
        return Err(err(format!(
            "name: use lowercase letters, digits and '-' (got {})",
            repr_str(&name)
        )));
    }
    let svcs_v = match obj_get(m, "services") {
        Some(x) if truthy(x) => x.clone(),
        _ => Value::Array(Vec::new()),
    };
    let Value::Array(svcs) = svcs_v.clone() else {
        return Err(err(
            "services: must be a list (it may be empty: a project that is only a workload)",
        ));
    };
    m["services"] = svcs_v;
    let net = obj_get(m, "network").cloned().unwrap_or(Value::Null);
    if !(net.is_null() || in_strs(&net, NET_MODES)) {
        return Err(err(format!(
            "network: loopback or bridge (got {})",
            repr(&net)
        )));
    }
    let bridge = has_netns(m);
    let mut seen: Vec<String> = Vec::new();
    for (i, s) in svcs.iter().enumerate() {
        let wh = format!("services[{i}]");
        if !s.is_object() {
            return Err(err(format!("{wh}: must be a mapping")));
        }
        unknown(s, KNOWN_SVC, &wh)?;
        let n = need_str(s, "name", &wh)?;
        if pyre::match_dollar(NAME_RE, &n).is_none() {
            return Err(err(format!(
                "{}: name {} must match [a-z][a-z0-9_]* (it becomes a shell identifier)",
                wh,
                repr_str(&n)
            )));
        }
        if seen.contains(&n) {
            return Err(err(format!(
                "{}: duplicate service name {}",
                wh,
                repr_str(&n)
            )));
        }
        seen.push(n);
        if obj_get(s, "image").is_none() {
            need(s, "cmd", &wh, Typ::Str)?;
        }
        need(s, "ready", &wh, Typ::Str)?;
        if let Some(p) = obj_get(s, "port") {
            if !is_int(p) {
                return Err(err(format!("{wh}: port must be a number")));
            }
        }
        validate_addr(s, &svcs, &wh, bridge)?;
        if let Some(r) = obj_get(s, "restart") {
            crate::restart::Restart::parse(r).map_err(|m| err(format!("{wh}: restart: {m}")))?;
        }
        if let Some(b) = obj_get(s, "restart_may_fail") {
            if !b.is_boolean() {
                return Err(err(format!(
                    "{wh}: restart_may_fail: must be true or false"
                )));
            }
        }
        if let Some(a) = obj_get(s, "start_after") {
            if !in_strs(a, &["ready", "started"]) {
                return Err(err(format!(
                    "{wh}: start_after: ready or started (got {})",
                    repr(a)
                )));
            }
        }
        if let Some(lg) = obj_get(s, "logs") {
            let Value::Array(a) = lg else {
                return Err(err(format!(
                    "{wh}: logs: a list of absolute paths of log files"
                )));
            };
            for x in a {
                if !x
                    .as_str()
                    .is_some_and(|p| p.starts_with('/') && !p.contains(['\n', '\'']))
                {
                    return Err(err(format!(
                        "{wh}: logs: {} is not an absolute path of a log file (for example /data/db/log/*.log)",
                        repr(x)
                    )));
                }
            }
        }
        if let Some(b) = obj_get(s, "writable") {
            if !b.is_boolean() {
                return Err(err(format!("{wh}: writable: must be true or false")));
            }
        }
        if let Some(st) = obj_get(s, "storage") {
            crate::storage::Storage::of(st).map_err(|m| err(format!("{wh}: storage: {m}")))?;
        }
        for dep in or_iter(obj_get(s, "depends_on"))? {
            let mut names = Vec::new();
            for x in &svcs {
                names.push(get(x, "name")?.cloned().unwrap_or(Value::Null));
            }
            if !py::contains(&names, &dep) {
                return Err(err(format!(
                    "{}: depends_on names unknown service {}",
                    wh,
                    repr(&dep)
                )));
            }
        }
    }
    crate::storage::check_distinct(m).map_err(err)?;
    let wl = obj_get(m, "workload").cloned().unwrap_or(Value::Null);
    if !wl.is_object() {
        return Err(err("workload: required (steps + run)"));
    }
    unknown(&wl, KNOWN_WORKLOAD, "workload")?;
    let st = need(&wl, "steps", "workload", Typ::Int)?;
    let sti = py::int_of(&st).unwrap_or(0);
    if !(1..=1000).contains(&sti) {
        return Err(err("workload.steps: between 1 and 1000"));
    }
    need(&wl, "run", "workload", Typ::Str)?;
    let invs = match obj_get(m, "invariants") {
        Some(x) if truthy(x) => x.clone(),
        _ => Value::Array(Vec::new()),
    };
    m["invariants"] = invs.clone();
    if !truthy(&invs) && !obj_get(m, "checks").is_some_and(truthy) {
        return Err(err(
            "invariants: at least one invariant (or one entry in checks:) is required (otherwise nothing is checked)",
        ));
    }
    let mut ids: Vec<String> = Vec::new();
    for (i, v) in iter_of(&invs)?.iter().enumerate() {
        let wh = format!("invariants[{i}]");
        unknown(v, KNOWN_INV, &wh)?;
        let vid = need_str(v, "id", &wh)?;
        if !pyre::fullmatch(r"[A-Za-z0-9_.\-]+", &vid) {
            return Err(err(format!(
                "{}: id {}: letters, digits, '.', '_' and '-' only",
                wh,
                repr_str(&vid)
            )));
        }
        if ids.contains(&vid) {
            return Err(err(format!("{}: duplicate id {}", wh, repr_str(&vid))));
        }
        ids.push(vid);
        need(v, "cmd", &wh, Typ::Str)?;
        let t = get(v, "type")?
            .cloned()
            .unwrap_or(Value::String("always".into()));
        if !in_strs(&t, &["always", "always_or_unreachable", "eventually"]) {
            return Err(err(format!(
                "{wh}: type must be always, always_or_unreachable or eventually"
            )));
        }
        if t == "eventually" {
            for k in ["every", "final"] {
                if has(v, k)? {
                    return Err(err(format!(
                        "{wh}: an eventually invariant has no {k}: (it is polled from its anchor until it holds or its deadline passes)"
                    )));
                }
            }
            let w = need(v, "within", &wh, Typ::Int)?;
            if w.is_boolean() || !int_in(&w, 1, 3600) {
                return Err(err(format!(
                    "{wh}: within is a whole number of seconds of guest time, 1..3600"
                )));
            }
            let after = get(v, "after")?
                .cloned()
                .unwrap_or(Value::String("heal".into()));
            if !in_strs(&after, LIVE_ANCHORS) {
                return Err(err(format!(
                    "{}: after must be one of {}",
                    wh,
                    LIVE_ANCHORS.join(", ")
                )));
            }
            if let Some(p) = obj_get(v, "poll") {
                if !is_plain_int(p) || py::int_of(p).unwrap_or(0) < 1 {
                    return Err(err(format!("{wh}: poll is a whole number of seconds >= 1")));
                }
            }
            continue;
        }
        for k in ["within", "after", "poll"] {
            if has(v, k)? {
                return Err(err(format!("{wh}: {k}: is only for type: eventually")));
            }
        }
        let has_every = has(v, "every")?;
        if has_every {
            let e = index(v, "every")?;
            if !is_int(e) || py::int_of(e).unwrap_or(0) < 1 {
                return Err(err(format!(
                    "{wh}: every is a whole number of seconds >= 1"
                )));
            }
        }
        let fin = match get(v, "final")? {
            Some(f) => truthy(f),
            None => !has_every,
        };
        if !fin && !has_every {
            return Err(err(format!(
                "{wh}: an invariant with final: false needs every:"
            )));
        }
    }
    let mut toks: Vec<String> = Vec::new();
    for (i, k) in or_iter(obj_get(m, "knobs"))?.iter().enumerate() {
        let wh = format!("knobs[{i}]");
        unknown(k, KNOWN_KNOB, &wh)?;
        let t = need_str(k, "token", &wh)?;
        if !pyre::fullmatch("[A-Z]", &t) || "KNCPTOFH".contains(t.as_str()) {
            return Err(err(format!(
                "{wh}: token must be one capital letter other than K N C P T O F H (fault tokens)"
            )));
        }
        if t == "A" {
            return Err(err(format!(
                "{wh}: token A is the schedule's reaction token (A=, M139); rename the knob"
            )));
        }
        if toks.contains(&t) {
            return Err(err(format!("{wh}: duplicate token {t}")));
        }
        toks.push(t);
        need(k, "default", &wh, Typ::Str)?;
        need(k, "set", &wh, Typ::Str)?;
    }
    let faults = or_iter(obj_get(m, "faults"))?;
    for f in &faults {
        if matches!(f, Value::Array(_) | Value::Object(_)) {
            return Err(crash("unhashable type in faults:"));
        }
        if !in_strs(f, FAULT_KINDS) {
            return Err(err(format!(
                "faults: unknown kind {} (known: {})",
                repr(f),
                joined(FAULT_KINDS)
            )));
        }
    }
    if let Some(fm) = obj_get(m, "freeze_match") {
        if !fm.is_null() {
            let lists: Option<Vec<Value>> = match fm {
                Value::Array(_) => Some(vec![fm.clone()]),
                Value::Object(o) => Some(o.values().cloned().collect()),
                _ => None,
            };
            let extra_keys = matches!(fm, Value::Object(o) if o.keys().any(|k| k != "replies" && k != "requests"));
            let bad_list = lists.as_ref().is_some_and(|ls| {
                ls.iter().any(|l| match l {
                    Value::Array(xs) => xs.iter().any(|x| match x {
                        Value::String(s) => s.is_empty() || s.contains(':') || s.contains(' '),
                        _ => true,
                    }),
                    _ => true,
                })
            });
            if lists.is_none() || extra_keys || bad_list {
                return Err(err("freeze_match: a list of payload strings, or a mapping with lists replies: and requests:; each string non-empty, without spaces or colons (%xx escapes a byte)"));
            }
            let any_nonempty = lists.unwrap_or_default().iter().any(truthy);
            if any_nonempty && !py::contains(&faults, &Value::String("freeze".into())) {
                return Err(err("freeze_match: only with `freeze` in faults:"));
            }
        }
    }
    let data = or_map(obj_get(m, "data"));
    unknown(&data, KNOWN_DATA, "data")?;
    let root = or_map(obj_get(m, "root"));
    unknown(&root, KNOWN_ROOT, "root")?;
    if let Some(v) = obj_get(&root, "share_images") {
        if !v.is_boolean() {
            return Err(err("root.share_images: must be true or false"));
        }
    }
    if let Some(v) = obj_get(&root, "fix_pyc") {
        if !v.is_boolean() {
            return Err(err("root.fix_pyc: must be true or false"));
        }
    }
    let guest = or_map(obj_get(m, "guest"));
    unknown(&guest, KNOWN_GUEST, "guest")?;
    if has(&guest, "clock_after_cut")? {
        let c = index(&guest, "clock_after_cut")?;
        if !int_in(c, 1, 10 * 365 * 86400) {
            return Err(err(format!(
                "guest.clock_after_cut: whole seconds in [1, 315360000] (got {})",
                repr(c)
            )));
        }
    }
    if has(&guest, "outcomes_after_cut")? {
        let c = index(&guest, "outcomes_after_cut")?;
        if !c.is_boolean() {
            return Err(err(format!(
                "guest.outcomes_after_cut: true or false (got {})",
                repr(c)
            )));
        }
    }
    if has(&guest, "cpu")? {
        let c = index(&guest, "cpu")?;
        if !in_strs(c, &["v2", "v3"]) {
            return Err(err(format!(
                "guest.cpu: v2 (the default: x86-64-v2, no AVX) or v3 (adds AVX/AVX2, FMA, BMI1/2, F16C, LZCNT, MOVBE, XSAVE; x86_64 only) (got {})",
                repr(c)
            )));
        }
    }
    if has(&guest, "ram_mib")? {
        let r = index(&guest, "ram_mib")?;
        if !int_in(r, RAM_MIB_MIN, RAM_MIB_MAX) {
            return Err(err(format!(
                "guest.ram_mib: a whole number of MiB in [{}, {}] (got {})",
                RAM_MIB_MIN,
                RAM_MIB_MAX,
                repr(r)
            )));
        }
    }
    validate_network(m)?;
    validate_checks(m, &ids)?;
    validate_world(m)?;
    validate_errors(m)?;
    Ok(())
}

fn validate_network(m: &Value) -> R<()> {
    let Some(n) = obj_get(m, "network") else {
        return Ok(());
    };
    if !in_strs(n, NET_MODES) {
        return Err(err(format!(
            "network: loopback or bridge (got {})",
            repr(n)
        )));
    }
    if has_netns(m) {
        for (i, s) in iter_of(&m["services"])?.iter().enumerate() {
            if !has(s, "addr")? {
                return Err(err(format!(
                    "services[{i}]: network: bridge gives every service its own network namespace and address: addr: required (an address in {NET_BRIDGE} other than {NET_GW})"
                )));
            }
        }
    }
    Ok(())
}

fn validate_checks(m: &Value, inv_ids: &[String]) -> R<()> {
    let Some(cs) = obj_get(m, "checks") else {
        return Ok(());
    };
    if cs.is_null() {
        return Ok(());
    }
    let Value::Array(list) = cs else {
        return Err(err(
            "checks: a non-empty list of history checks (hydo/HISTORIES.md)",
        ));
    };
    if list.is_empty() {
        return Err(err(
            "checks: a non-empty list of history checks (hydo/HISTORIES.md)",
        ));
    }
    let mut ids: Vec<String> = inv_ids.to_vec();
    for (i, c) in list.iter().enumerate() {
        let wh = format!("checks[{i}]");
        if !c.is_object() {
            return Err(err(format!("{wh}: must be a mapping")));
        }
        unknown(c, KNOWN_CHECK, &wh)?;
        let cid = need_str(c, "id", &wh)?;
        if !pyre::fullmatch(r"[A-Za-z0-9_.\-]+", &cid) {
            return Err(err(format!(
                "{}: id {}: letters, digits, '.', '_' and '-' only",
                wh,
                repr_str(&cid)
            )));
        }
        if ids.contains(&cid) {
            return Err(err(format!(
                "{}: id {} is already used by an invariant or another check",
                wh,
                repr_str(&cid)
            )));
        }
        ids.push(cid);
        let t = need_str(c, "type", &wh)?;
        if !CHECK_TYPES.contains(&t.as_str()) {
            return Err(err(format!(
                "{}: type must be one of {}",
                wh,
                CHECK_TYPES.join(", ")
            )));
        }
        let h = need_str(c, "history", &wh)?;
        if !pyre::fullmatch(r"[A-Za-z0-9_.\-]+", &h) {
            return Err(err(format!(
                "{}: history {}: letters, digits, '.', '_' and '-' only",
                wh,
                repr_str(&h)
            )));
        }
        if t == "linearizable" {
            let model = obj_get(c, "model")
                .cloned()
                .unwrap_or(Value::String("register".into()));
            if !in_strs(&model, &["register", "cas"]) {
                return Err(err(format!("{wh}: model must be register or cas")));
            }
            let init = obj_get(c, "initial")
                .cloned()
                .unwrap_or(Value::String("nil".into()));
            if !in_strs(&init, &["nil", "any"]) {
                return Err(err(format!(
                    "{wh}: initial must be nil (keys start absent) or any (unknown)"
                )));
            }
            if obj_get(c, "consistency").is_some() {
                return Err(err(format!("{wh}: consistency: is for type: list-append")));
            }
        } else {
            let lv = obj_get(c, "consistency")
                .cloned()
                .unwrap_or(Value::String("strict-serializable".into()));
            if !in_strs(&lv, CHECK_LEVELS) {
                return Err(err(format!(
                    "{}: consistency must be one of {}",
                    wh,
                    CHECK_LEVELS.join(", ")
                )));
            }
            for k in ["model", "initial"] {
                if obj_get(c, k).is_some() {
                    return Err(err(format!("{wh}: {k}: is for type: linearizable")));
                }
            }
        }
        if let Some(f) = obj_get(c, "final") {
            if !f.is_string() {
                return Err(err(format!("{wh}: final: is a shell command")));
            }
        }
    }
    Ok(())
}

fn world_addr(x: &Value, wh: &str, bridge: bool) -> R<String> {
    if bridge {
        let a = str_of(obj_get(x, "addr").unwrap_or(&Value::String(NET_GW.into())));
        if a != NET_GW && !bridge_addr_ok(&a) {
            return Err(err(format!(
                "{}: addr {}: with network: bridge, an address in {} (default {}, the bridge's own address)",
                wh,
                repr_str(&a),
                NET_BRIDGE,
                NET_GW
            )));
        }
        return Ok(a);
    }
    let a = str_of(obj_get(x, "addr").unwrap_or(&Value::String("127.0.0.1".into())));
    let ok = match pyre::match_dollar(r"127\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})", &a) {
        Some(g) => g.iter().all(|x| {
            x.as_deref()
                .and_then(|s| s.parse::<u32>().ok())
                .is_some_and(|n| n <= 255)
        }),
        None => false,
    };
    if !ok {
        return Err(err(format!(
            "{}: addr {} must be a loopback address 127.x.y.z",
            wh,
            repr_str(&a)
        )));
    }
    Ok(a)
}

fn validate_world(m: &Value) -> R<()> {
    let fakes = match obj_get(m, "fakes") {
        Some(x) if truthy(x) => x.clone(),
        _ => Value::Array(Vec::new()),
    };
    let proxies = match obj_get(m, "proxies") {
        Some(x) if truthy(x) => x.clone(),
        _ => Value::Array(Vec::new()),
    };
    let (Value::Array(fakes), Value::Array(proxies)) = (fakes, proxies) else {
        return Err(err("fakes: and proxies: are lists"));
    };
    let services = iter_of(&m["services"])?;
    let mut svcs: Map<String, Value> = Map::new();
    for s in &services {
        svcs.insert(str_of(index(s, "name")?), s.clone());
    }
    let bridge = has_netns(m);
    let host_re = if bridge {
        r"10\.89\.\d+\.\d+"
    } else {
        r"127\.\d+\.\d+\.\d+"
    };
    let where_kind = if bridge {
        "bridge address 10.89.x.y"
    } else {
        "loopback address"
    };
    let mut used: Vec<((String, i128), String)> = Vec::new();
    for s in &services {
        if obj_get(s, "port").is_some_and(truthy) {
            let key = (
                str_of(obj_get(s, "addr").unwrap_or(&Value::String("127.0.0.1".into()))),
                py::int_of(&s["port"]).unwrap_or(0),
            );
            let who = format!("service {}", str_of(&s["name"]));
            match used.iter_mut().find(|(k, _)| *k == key) {
                Some(e) => e.1 = who,
                None => used.push((key, who)),
            }
        }
    }
    let svc_addrs: Vec<String> = services
        .iter()
        .map(|s| str_of(obj_get(s, "addr").unwrap_or(&Value::Null)))
        .collect();
    let mut names: Vec<String> = Vec::new();
    // `tls:` listeners: endpoints may share one (address, port), told apart by host name
    let mut tls_lis: Vec<((String, i128), String)> = Vec::new();
    let mut tls_hosts: Vec<String> = Vec::new();
    for (kind, lst, known) in [
        ("fakes", &fakes, KNOWN_FAKE),
        ("proxies", &proxies, KNOWN_PROXY),
    ] {
        let singular = if kind == "fakes" { "fake" } else { "proxy" };
        for (i, e) in lst.iter().enumerate() {
            let wh = format!("{kind}[{i}]");
            if !e.is_object() {
                return Err(err(format!("{wh}: must be a mapping")));
            }
            unknown(e, known, &wh)?;
            let n = need_str(e, "name", &wh)?;
            if pyre::match_dollar(NAME_RE, &n).is_none() {
                return Err(err(format!(
                    "{}: name {} must match [a-z][a-z0-9_]* (fault tokens name it: H={}@...)",
                    wh,
                    repr_str(&n),
                    n
                )));
            }
            if names.contains(&n) || svcs.contains_key(&n) || n == "workload" {
                return Err(err(format!(
                    "{}: name {} is already a service, fake or proxy",
                    wh,
                    repr_str(&n)
                )));
            }
            names.push(n.clone());
            let port = obj_get(e, "port").cloned().unwrap_or(Value::Null);
            if !int_in(&port, 1, 65535) {
                return Err(err(format!("{wh}: port: required, a TCP port number")));
            }
            let pnum = py::int_of(&port).unwrap_or(0);
            let ap = (world_addr(e, &wh, bridge)?, pnum);
            if bridge && svc_addrs.contains(&ap.0) {
                return Err(err(format!(
                    "{}: addr {} is a service's address (that service has its own network namespace); give the {} its own address on the bridge",
                    wh, ap.0, singular
                )));
            }
            if let Some((_, who)) = used.iter().chain(tls_lis.iter()).find(|(k, _)| *k == ap) {
                return Err(err(format!(
                    "{}: {}:{} is already used by {}",
                    wh, ap.0, pnum, who
                )));
            }
            used.push((ap.clone(), format!("{singular} {n}")));
            if let Some(t) = obj_get(e, "tls") {
                let tw = format!("{wh}.tls");
                if !t.is_object() {
                    return Err(err(format!(
                        "{tw}: a mapping {{hosts: [<host name>, ...], port: <default 443>}}"
                    )));
                }
                unknown(t, KNOWN_TLS, &tw)?;
                let hosts = match obj_get(t, "hosts") {
                    Some(Value::Array(a)) if !a.is_empty() => a.clone(),
                    _ => {
                        return Err(err(format!(
                            "{tw}: hosts: a list of the host names the system calls (e.g. [api.stripe.com])"
                        )))
                    }
                };
                for h in &hosts {
                    let hs = match h {
                        Value::String(s) => s.clone(),
                        _ => String::new(),
                    };
                    if hs.len() > 253 || !pyre::fullmatch(HOST_RE, &hs) {
                        return Err(err(format!(
                            "{}: hosts: {} is not a host name (lower-case letters, digits, '-' and '.'; no wildcard)",
                            tw,
                            repr(h)
                        )));
                    }
                    if tls_hosts.contains(&hs) || svcs.contains_key(&hs) || names.contains(&hs) {
                        return Err(err(format!(
                            "{}: hosts: {} is already the name of a service or of another tls: endpoint",
                            tw,
                            repr_str(&hs)
                        )));
                    }
                    tls_hosts.push(hs);
                }
                let tp = obj_get(t, "port").cloned().unwrap_or(Value::from(443));
                if !int_in(&tp, 1, 65535) {
                    return Err(err(format!("{tw}: port: a TCP port number (default 443)")));
                }
                let tap = (ap.0.clone(), py::int_of(&tp).unwrap_or(443));
                if let Some((_, who)) = used.iter().find(|(k, _)| *k == tap) {
                    return Err(err(format!(
                        "{}: {}:{} is already used by {}",
                        tw, tap.0, tap.1, who
                    )));
                }
                tls_lis.push((tap, format!("the tls: listener of {singular} {n}")));
            }
            let proto = obj_get(e, "protocol")
                .cloned()
                .unwrap_or(Value::String("http".into()));
            let protos: &[&str] = if kind == "fakes" {
                &["http", "grpc"]
            } else {
                &["http", "grpc", "tcp"]
            };
            if !in_strs(&proto, protos) {
                return Err(err(format!(
                    "{}: protocol must be {}",
                    wh,
                    if kind == "fakes" {
                        "http or grpc"
                    } else {
                        "http, grpc or tcp"
                    }
                )));
            }
            for k in ["idempotency", "ref"] {
                if let Some(x) = obj_get(e, k) {
                    if pyre::match_dollar(EXTRACT_RE, &str_of(x)).is_none() {
                        return Err(err(format!(
                            "{}: {} {}: one of header:<Name>, json:<field>, query:<name>, form:<name>, path:<name|n>, pb:<field number>, body",
                            wh,
                            k,
                            repr(x)
                        )));
                    }
                }
            }
            if kind == "proxies" {
                let has_to = obj_get(e, "to").is_some();
                let has_svc = obj_get(e, "service").is_some();
                if has_to == has_svc {
                    return Err(err(format!(
                        "{wh}: give exactly one of to: <host:port> or service: <name>"
                    )));
                }
                if has_svc {
                    let sv = &e["service"];
                    if matches!(sv, Value::Array(_) | Value::Object(_)) {
                        return Err(crash("unhashable type as a service name"));
                    }
                    let t = match sv {
                        Value::String(s) => svcs.get(s),
                        _ => None,
                    };
                    if !t.is_some_and(|t| truthy(t) && obj_get(t, "port").is_some_and(truthy)) {
                        return Err(err(format!(
                            "{}: service {} is not a service with a port:",
                            wh,
                            repr(sv)
                        )));
                    }
                } else if !pyre::fullmatch(&format!(r"{host_re}:\d+"), &str_of(&e["to"])) {
                    return Err(err(format!(
                        "{}: to {} must be <{}>:<port>",
                        wh,
                        repr(&e["to"]),
                        where_kind
                    )));
                }
                if proto == "tcp" && obj_get(e, "routes").is_some_and(truthy) {
                    return Err(err(format!(
                        "{wh}: a tcp proxy has no routes (one connection = one request, route conn)"
                    )));
                }
            }
            let routes = or_iter(obj_get(e, "routes"))?;
            if kind == "fakes" && routes.is_empty() {
                return Err(err(format!("{wh}: routes: at least one")));
            }
            let mut rn: Vec<String> = Vec::new();
            for (j, r) in routes.iter().enumerate() {
                let rw = format!("{wh}.routes[{j}]");
                if !r.is_object() {
                    return Err(err(format!("{rw}: must be a mapping")));
                }
                unknown(
                    r,
                    if kind == "fakes" {
                        KNOWN_ROUTE
                    } else {
                        KNOWN_PROXY_ROUTE
                    },
                    &rw,
                )?;
                let x = need_str(r, "name", &rw)?;
                if pyre::match_dollar(NAME_RE, &x).is_none()
                    || rn.contains(&x)
                    || x == "any"
                    || x == "conn"
                {
                    return Err(err(format!(
                        "{}: name {}: [a-z][a-z0-9_]*, unique, not 'any' or 'conn'",
                        rw,
                        repr_str(&x)
                    )));
                }
                rn.push(x);
                if let Some(rf) = obj_get(r, "ref") {
                    if pyre::match_dollar(EXTRACT_RE, &str_of(rf)).is_none() {
                        return Err(err(format!(
                            "{}: ref {} is not an extractor (see MANIFEST.md)",
                            rw,
                            repr(rf)
                        )));
                    }
                }
                for k in ["status", "code"] {
                    if let Some(v) = obj_get(r, k) {
                        if !is_plain_int(v) {
                            return Err(err(format!("{rw}: {k} must be a number")));
                        }
                    }
                }
                if let Some(msg) = obj_get(r, "message") {
                    if !pyre::fullmatch("(?:[0-9a-fA-F]{2})*", &str_of(msg)) {
                        return Err(err(format!("{rw}: message is the protobuf bytes in hex")));
                    }
                }
                for k in ["effect", "idempotent"] {
                    if let Some(v) = obj_get(r, k) {
                        if !v.is_boolean() {
                            return Err(err(format!("{rw}: {k} is true or false")));
                        }
                    }
                }
                if obj_get(r, "idempotent").is_some_and(truthy)
                    && !obj_get(e, "idempotency").is_some_and(truthy)
                {
                    return Err(err(format!(
                        "{rw}: idempotent: true needs the fake's idempotency: (where the key is)"
                    )));
                }
                validate_objects(r, &rw)?;
                let wh_ = obj_get(r, "webhook").cloned().unwrap_or(Value::Null);
                if !wh_.is_null() {
                    let hw = format!("{rw}.webhook");
                    if !wh_.is_object() {
                        return Err(err(format!("{hw}: must be a mapping")));
                    }
                    unknown(&wh_, KNOWN_HOOK, &hw)?;
                    let url = need(&wh_, "url", &hw, Typ::Str)?;
                    if !pyre::fullmatch(&format!(r"http://{host_re}:\d+/\S*"), &str_of(&url)) {
                        return Err(err(format!(
                            "{hw}: url must be http://<{where_kind}>:<port>/<path>"
                        )));
                    }
                    let mut texts: Vec<Value> = vec![
                        obj_get(&wh_, "body")
                            .filter(|b| truthy(b))
                            .cloned()
                            .unwrap_or(Value::String(String::new())),
                        wh_["url"].clone(),
                    ];
                    match obj_get(&wh_, "headers") {
                        Some(h) if truthy(h) => match h {
                            Value::Object(o) => texts.extend(o.values().cloned()),
                            _ => return Err(crash("webhook headers has no .values()")),
                        },
                        _ => {}
                    }
                    for t in &texts {
                        let found = pyre::findall1(
                            r#"\{(seq|ref|idem|fake|route|json|header|query|path|form|pb)(?::[^{}"]*)?\}"#,
                            &str_of(t),
                        );
                        let mut bad: Vec<String> = found
                            .into_iter()
                            .filter(|x| !HOOK_VARS.contains(&x.as_str()))
                            .collect();
                        bad.sort();
                        if let Some(b) = bad.first() {
                            let mut hv: Vec<&str> = HOOK_VARS.to_vec();
                            hv.sort();
                            return Err(err(format!(
                                "{}: {{{}:...}}: a webhook may use only {{{}}} (it can be re-sent after a power cut, without the request)",
                                hw,
                                b,
                                hv.join("} {")
                            )));
                        }
                    }
                    for k in ["delay_ms", "retries"] {
                        if let Some(v) = obj_get(&wh_, k) {
                            if !is_int(v) || py::int_of(v).unwrap_or(0) < 0 {
                                return Err(err(format!("{hw}: {k} is a whole number >= 0")));
                            }
                        }
                    }
                    validate_sign(&wh_, &hw)?;
                }
            }
        }
    }
    Ok(())
}

/// A fake route's `save:` (key and fields, templates) and `load:` (a template): hydo-world's
/// objects (hydo/crates/hydo-world/src/objects.rs).
fn validate_objects(r: &Value, rw: &str) -> R<()> {
    if let Some(s) = obj_get(r, "save") {
        let sw = format!("{rw}.save");
        if !s.is_object() {
            return Err(err(format!("{sw}: must be a mapping (key: and fields:)")));
        }
        unknown(s, KNOWN_SAVE, &sw)?;
        match obj_get(s, "key") {
            Some(Value::String(k)) if !k.is_empty() => {}
            _ => {
                return Err(err(format!(
                    "{sw}: key: the object's key, a template (e.g. 'pi_{{seq}}' or '{{path:id}}')"
                )))
            }
        }
        if let Some(f) = obj_get(s, "fields") {
            let Value::Object(o) = f else {
                return Err(err(format!("{sw}.fields: a mapping of field: template")));
            };
            for (k, x) in o {
                if pyre::match_dollar(r"[A-Za-z_][A-Za-z0-9_.-]*", k).is_none() {
                    return Err(err(format!(
                        "{sw}.fields: field name {}: letters, digits, _ . -",
                        repr_str(k)
                    )));
                }
                if !(x.is_string() || x.is_i64() || x.is_u64()) {
                    return Err(err(format!("{sw}.fields.{k}: a string (a template)")));
                }
            }
        }
    }
    if let Some(l) = obj_get(r, "load") {
        if !matches!(l, Value::String(s) if !s.is_empty()) {
            return Err(err(format!(
                "{rw}: load: the key of the object the request is about, a template (e.g. '{{path:id}}')"
            )));
        }
    }
    Ok(())
}

/// A webhook's `sign:` (hydo/crates/hydo-world/src/sign.rs).
fn validate_sign(wh: &Value, hw: &str) -> R<()> {
    let Some(s) = obj_get(wh, "sign") else {
        return Ok(());
    };
    let sw = format!("{hw}.sign");
    if !s.is_object() {
        return Err(err(format!("{sw}: must be a mapping (header:, secret:)")));
    }
    unknown(s, KNOWN_SIGN, &sw)?;
    match obj_get(s, "header") {
        Some(Value::String(h)) if pyre::match_dollar("[A-Za-z0-9-]+", h).is_some() => {}
        _ => return Err(err(format!("{sw}: header: the header's name"))),
    }
    match obj_get(s, "secret") {
        Some(Value::String(x)) if !x.is_empty() => {}
        _ => return Err(err(format!("{sw}: secret: the endpoint secret (a string)"))),
    }
    for k in ["payload", "value"] {
        if let Some(x) = obj_get(s, k) {
            if !x.is_string() {
                return Err(err(format!("{sw}: {k}: a string (a template)")));
            }
        }
    }
    if let Some(Value::String(v)) = obj_get(s, "value") {
        if !v.contains("{hmac}") {
            return Err(err(format!("{sw}: value: must contain {{hmac}}")));
        }
    }
    if let Some(e) = obj_get(s, "encoding") {
        if !in_strs(e, &["hex", "base64"]) {
            return Err(err(format!(
                "{sw}: encoding: hex or base64 (got {})",
                repr(e)
            )));
        }
    }
    Ok(())
}

fn ere(v: &Value, wh: &str) -> R<()> {
    match v {
        Value::String(s) if !s.is_empty() && !s.contains('\n') && !s.contains('\t') => {
            if let Err(e) = rx::check(s) {
                return Err(err(format!(
                    "{}: {} is not a regular expression ({}); POSIX ERE, as busybox awk reads it",
                    wh,
                    repr_str(s),
                    e
                )));
            }
            Ok(())
        }
        _ => Err(err(format!(
            "{wh}: a non-empty regular expression on one line, without tabs"
        ))),
    }
}

fn validate_errors(m: &mut Value) -> R<()> {
    let e = obj_get(m, "errors").cloned().unwrap_or(Value::Null);
    let svcs = iter_of(&m["services"])?;
    if e.is_null() || eq(&e, &Value::Bool(false)) {
        for (i, s) in svcs.iter().enumerate() {
            if has(s, "errors")? {
                return Err(err(format!(
                    "services[{i}].errors: needs the top-level errors: (which turns error logs as findings on; `errors: true` takes every default)"
                )));
            }
        }
        return Ok(());
    }
    let e = if e == Value::Bool(true) {
        m["errors"] = Value::Object(Map::new());
        Value::Object(Map::new())
    } else {
        e
    };
    if !e.is_object() {
        return Err(err("errors: true, or a mapping (level, max, services)"));
    }
    unknown(&e, KNOWN_ERRORS_TOP, "errors")?;
    let lvl = obj_get(&e, "level")
        .cloned()
        .unwrap_or(Value::String("error".into()));
    if !in_strs(&lvl, &["error", "warning"])
        && !str_of(obj_get(&e, "level").unwrap_or(&Value::Null)).contains("{{")
    {
        return Err(err(
            "errors.level: error (the default) or warning (also warning-level lines)",
        ));
    }
    let mx = obj_get(&e, "max")
        .cloned()
        .unwrap_or(Value::Number(40.into()));
    if !int_in(&mx, 1, 200) {
        return Err(err(
            "errors.max: the most error kinds one boot prints, 1..200 (default 40)",
        ));
    }
    let mut names = Vec::new();
    for s in &svcs {
        names.push(index(s, "name")?.clone());
    }
    if let Some(cov) = obj_get(&e, "services") {
        if !cov.is_null() {
            let ok = match cov {
                Value::Array(cs) => cs.iter().all(|c| py::contains(&names, c)),
                _ => false,
            };
            if !ok {
                return Err(err(
                    "errors.services: a list of service names (default: every service)",
                ));
            }
        }
    }
    for (i, s) in svcs.iter().enumerate() {
        let se = obj_get(s, "errors").cloned().unwrap_or(Value::Null);
        if se.is_null() {
            continue;
        }
        let wh = format!("services[{i}].errors");
        if !se.is_object() {
            return Err(err(format!(
                "{wh}: a mapping (format, level, match, ignore, files, normalize, off)"
            )));
        }
        unknown(&se, KNOWN_ERRORS_SVC, &wh)?;
        let fmt = obj_get(&se, "format")
            .cloned()
            .unwrap_or(Value::String("auto".into()));
        if !in_strs(&fmt, ERR_FORMATS) {
            return Err(err(format!(
                "{}: format must be one of {}",
                wh,
                ERR_FORMATS.join(", ")
            )));
        }
        let lv = obj_get(&se, "level")
            .cloned()
            .unwrap_or_else(|| lvl.clone());
        if !in_strs(&lv, &["error", "warning"]) && !str_of(&lv).contains("{{") {
            return Err(err(format!("{wh}: level: error or warning")));
        }
        for k in ["match", "ignore"] {
            if let Some(v) = obj_get(&se, k) {
                ere(v, &format!("{wh}.{k}"))?;
            }
        }
        if obj_get(&se, "format") == Some(&Value::String("none".into()))
            && obj_get(&se, "match").is_none()
        {
            return Err(err(format!(
                "{wh}: format: none needs match: (otherwise no line is an error)"
            )));
        }
        let fs = match obj_get(&se, "files") {
            Some(x) if truthy(x) => x.clone(),
            _ => Value::Array(Vec::new()),
        };
        let files_ok = match &fs {
            Value::Array(xs) => xs.iter().all(|f| match f {
                Value::String(s) => s.starts_with('/') && !pyre::search(r#"[\s'"$`\\]"#, s),
                _ => false,
            }),
            _ => false,
        };
        if !files_ok {
            return Err(err(format!(
                "{wh}.files: absolute paths of log files the service writes (no spaces or quotes)"
            )));
        }
        for (j, r) in or_iter(obj_get(&se, "normalize"))?.iter().enumerate() {
            let w = format!("{wh}.normalize[{j}]");
            match r {
                Value::String(_) => ere(r, &w)?,
                Value::Object(o)
                    if o.keys()
                        .all(|k| ["pattern", "replace", "description"].contains(&k.as_str()))
                        && o.contains_key("pattern") =>
                {
                    ere(&o["pattern"], &format!("{w}.pattern"))?;
                    let rep = str_of(o.get("replace").unwrap_or(&Value::String(String::new())));
                    if rep.contains('\t') || rep.contains('\n') {
                        return Err(err(format!("{w}.replace: one line, no tabs")));
                    }
                }
                _ => {
                    return Err(err(format!(
                        "{w}: a regular expression (replaced by *), or {{pattern, replace}}"
                    )))
                }
            }
        }
        if let Some(off) = obj_get(&se, "off") {
            if !off.is_boolean() {
                return Err(err(format!("{wh}.off: true or false")));
            }
        }
    }
    Ok(())
}

/// `check_ram_arch(m, arch)`.
pub fn check_ram_arch(m: &Value, arch: &str) -> R<()> {
    let guest = or_map(obj_get(m, "guest"));
    let r = get(&guest, "ram_mib")?
        .cloned()
        .unwrap_or(Value::Number(1024.into()));
    let max = match arch {
        "x86_64" => Some(RAM_MIB_MAX_X86),
        "arm64" => Some(RAM_MIB_MAX_ARM),
        _ => None,
    };
    if let Some(max) = max {
        let ri = py::num_of(&r).ok_or_else(|| crash("'>' not supported for guest.ram_mib"))?;
        if ri > max as f64 {
            return Err(err(format!(
                "guest.ram_mib: {} MiB is more than --arch {} runs (at most {})",
                d(&r),
                arch,
                max
            )));
        }
    }
    if get(&guest, "cpu")? == Some(&Value::String("v3".into())) && arch != "x86_64" {
        return Err(err(format!(
            "guest.cpu: v3 (AVX/AVX2) is an x86_64 option; --arch {arch} cannot use it"
        )));
    }
    Ok(())
}

/// `service_order(svcs)`: manifest order, moved only as much as depends_on requires.
pub fn service_order(svcs: &Value) -> R<Vec<String>> {
    let list = iter_of(svcs)?;
    let mut names = Vec::new();
    let mut deps: Vec<(String, Vec<String>)> = Vec::new();
    for s in &list {
        let n = str_of(index(s, "name")?);
        let ds = or_iter(obj_get(s, "depends_on"))?
            .iter()
            .map(str_of)
            .collect();
        names.push(n.clone());
        deps.push((n, ds));
    }
    let mut out: Vec<String> = Vec::new();
    let mut temp: Vec<String> = Vec::new();
    fn visit(
        n: &str,
        deps: &[(String, Vec<String>)],
        out: &mut Vec<String>,
        temp: &mut Vec<String>,
    ) -> R<()> {
        if out.iter().any(|x| x == n) {
            return Ok(());
        }
        if temp.iter().any(|x| x == n) {
            return Err(err(format!(
                "services: depends_on has a cycle through {}",
                repr_str(n)
            )));
        }
        temp.push(n.to_string());
        let ds = deps
            .iter()
            .rev()
            .find(|(k, _)| k == n)
            .map(|(_, d)| d.clone())
            .ok_or_else(|| crash(format!("KeyError: {}", repr_str(n))))?;
        for d in &ds {
            visit(d, deps, out, temp)?;
        }
        temp.retain(|x| x != n);
        out.push(n.to_string());
        Ok(())
    }
    for n in &names {
        visit(n, &deps, &mut out, &mut temp)?;
    }
    Ok(out)
}

/// `apply_params(m, overrides)`: removes `params:` from m, substitutes `{{name}}` in every string
/// value, and returns the effective values in declaration order.
pub fn apply_params(m: &mut Value, overrides: &[(String, String)]) -> R<Vec<(String, String)>> {
    let obj = m
        .as_object_mut()
        .ok_or_else(|| err("the manifest must be a mapping"))?;
    let params = obj.shift_remove("params").unwrap_or(Value::Null);
    let params = if truthy(&params) {
        params
    } else {
        Value::Object(Map::new())
    };
    let Value::Object(pm) = params else {
        return Err(err("params: must be a mapping of name: default value"));
    };
    let mut vals: Vec<(String, String)> = Vec::new();
    for (k, v) in &pm {
        if !pyre::fullmatch("[A-Za-z_][A-Za-z0-9_]*", k) {
            return Err(err(format!(
                "params: name {}: letters, digits and '_' only",
                repr_str(k)
            )));
        }
        let s = if v.is_null() {
            String::new()
        } else {
            str_of(v)
        };
        match vals.iter_mut().find(|(n, _)| n == k) {
            Some(e) => e.1 = s,
            None => vals.push((k.clone(), s)),
        }
    }
    for (k, v) in overrides {
        match vals.iter_mut().find(|(n, _)| n == k) {
            Some(e) => e.1 = v.clone(),
            None => {
                let mut ks: Vec<&str> = vals.iter().map(|(n, _)| n.as_str()).collect();
                ks.sort();
                let list = if ks.is_empty() {
                    "none".to_string()
                } else {
                    ks.join(", ")
                };
                return Err(err(format!(
                    "--set {}: the manifest declares no parameter {} (params: {})",
                    k,
                    repr_str(k),
                    list
                )));
            }
        }
    }
    fn sub(x: &Value, wh: &str, vals: &[(String, String)]) -> R<Value> {
        match x {
            Value::String(s) => {
                let re = regex::Regex::new(r"\{\{([A-Za-z_][A-Za-z0-9_]*)\}\}").unwrap();
                let mut out = String::new();
                let mut last = 0;
                for c in re.captures_iter(s) {
                    let all = c.get(0).unwrap();
                    let name = c.get(1).unwrap().as_str();
                    let Some((_, v)) = vals.iter().find(|(n, _)| n == name) else {
                        return Err(err(format!(
                            "{wh}: {{{{{name}}}}} is not a declared parameter"
                        )));
                    };
                    out.push_str(&s[last..all.start()]);
                    out.push_str(v);
                    last = all.end();
                }
                out.push_str(&s[last..]);
                Ok(Value::String(out))
            }
            Value::Array(a) => {
                let mut out = Vec::new();
                for (i, y) in a.iter().enumerate() {
                    out.push(sub(y, &format!("{wh}[{i}]"), vals)?);
                }
                Ok(Value::Array(out))
            }
            Value::Object(o) => {
                let mut out = Map::new();
                for (k, y) in o {
                    out.insert(k.clone(), sub(y, &format!("{wh}.{k}"), vals)?);
                }
                Ok(Value::Object(out))
            }
            other => Ok(other.clone()),
        }
    }
    let keys: Vec<String> = obj.keys().cloned().collect();
    for k in keys {
        let v = sub(&obj[&k], &k, &vals)?;
        obj.insert(k, v);
    }
    Ok(vals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A bridge project with one service on 10.89.0.11:443, and these fakes and proxies.
    fn project(fakes: Value, proxies: Value, svc: Value) -> Value {
        let mut s = json!({"name": "web", "cmd": "/bin/web", "ready": "true", "port": 443,
                           "addr": "10.89.0.11"});
        if let (Some(o), Value::Object(extra)) = (s.as_object_mut(), svc) {
            o.extend(extra);
        }
        json!({"name": "p", "network": "bridge", "services": [s],
               "workload": {"steps": 1, "run": "true"},
               "invariants": [{"id": "ok", "cmd": "true"}],
               "fakes": fakes, "proxies": proxies})
    }

    fn fake(name: &str, port: i64, tls: Value) -> Value {
        json!({"name": name, "port": port, "tls": tls, "routes": [{"name": "r", "path": "/"}]})
    }

    fn check(m: Value) -> Result<(), String> {
        let mut m = m;
        validate(&mut m).map_err(|e| e.message)
    }

    #[test]
    fn tls_accepted() {
        // two fakes share 10.89.0.1:443 (told apart by name), a third has its own port
        check(project(
            json!([
                fake("a", 9101, json!({"hosts": ["api.a.example"]})),
                fake(
                    "b",
                    9102,
                    json!({"hosts": ["api.b.example", "b.example"], "port": 443})
                ),
                fake("c", 9103, json!({"hosts": ["c.example"], "port": 8443})),
            ]),
            json!([{"name": "front", "port": 9200, "protocol": "tcp", "service": "web",
                    "tls": {"hosts": ["web.example"], "port": 9443}}]),
            json!({}),
        ))
        .unwrap();
    }

    #[test]
    fn tls_refusals() {
        let cases = [
            (
                json!("api.a.example"),
                "fakes[0].tls: a mapping {hosts: [<host name>, ...], port: <default 443>}",
            ),
            (
                json!({"port": 443}),
                "fakes[0].tls: hosts: a list of the host names the system calls (e.g. [api.stripe.com])",
            ),
            (
                json!({"hosts": []}),
                "fakes[0].tls: hosts: a list of the host names the system calls (e.g. [api.stripe.com])",
            ),
            (
                json!({"hosts": ["API.example"]}),
                "fakes[0].tls: hosts: 'API.example' is not a host name (lower-case letters, digits, '-' and '.'; no wildcard)",
            ),
            (
                json!({"hosts": ["*.example"]}),
                "fakes[0].tls: hosts: '*.example' is not a host name (lower-case letters, digits, '-' and '.'; no wildcard)",
            ),
            (
                json!({"hosts": ["web"]}),
                "fakes[0].tls: hosts: 'web' is already the name of a service or of another tls: endpoint",
            ),
            (
                json!({"hosts": ["a.example"], "port": 0}),
                "fakes[0].tls: port: a TCP port number (default 443)",
            ),
            (
                json!({"hosts": ["a.example"], "bogus": 1}),
                "fakes[0].tls: unknown field(s) bogus (known: description, hosts, port)",
            ),
        ];
        for (tls, want) in cases {
            assert_eq!(
                check(project(json!([fake("a", 9101, tls)]), json!([]), json!({}))).unwrap_err(),
                want
            );
        }
        // the same host name twice
        assert_eq!(
            check(project(
                json!([
                    fake("a", 9101, json!({"hosts": ["x.example"]})),
                    fake("b", 9102, json!({"hosts": ["x.example"]})),
                ]),
                json!([]),
                json!({})
            ))
            .unwrap_err(),
            "fakes[1].tls: hosts: 'x.example' is already the name of a service or of another tls: endpoint"
        );
        // a TLS port on another fake's plain port, and a plain port on an earlier TLS listener
        assert_eq!(
            check(project(
                json!([
                    fake("a", 9101, json!({"hosts": ["x.example"]})),
                    fake("b", 9102, json!({"hosts": ["y.example"], "port": 9101})),
                ]),
                json!([]),
                json!({})
            ))
            .unwrap_err(),
            "fakes[1].tls: 10.89.0.1:9101 is already used by fake a"
        );
        assert_eq!(
            check(project(
                json!([
                    fake("a", 9101, json!({"hosts": ["x.example"]})),
                    {"name": "b", "port": 443, "routes": [{"name": "r", "path": "/"}]},
                ]),
                json!([]),
                json!({})
            ))
            .unwrap_err(),
            "fakes[1]: 10.89.0.1:443 is already used by the tls: listener of fake a"
        );
    }

    #[test]
    fn writable_is_a_boolean() {
        check(project(json!([]), json!([]), json!({"writable": false}))).unwrap();
        assert_eq!(
            check(project(json!([]), json!([]), json!({"writable": "yes"}))).unwrap_err(),
            "services[0]: writable: must be true or false"
        );
    }
}
