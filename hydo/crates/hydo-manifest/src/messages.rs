//! `checks[].type: messages` (docs/design/messaging.md "The declaration"): what a project says its
//! message-passing system promises. hydo knows no system: the declaration alone decides which
//! claims the host's checker (hydo-history's `messages` module) judges.
//!
//! - [`resolve_includes`]: `include:` bundles (YAML files of the project, paths relative to the
//!   manifest) merged in order, then the entry's own keys; a key a later source gives replaces
//!   the earlier one whole, inside `claims:` and `measures:` per key. `hydo build` resolves them,
//!   so the image's compiled manifest carries the resolved declaration and a bundle changed later
//!   does not change what an existing image claims.
//! - [`validate`]: every key and value, and the combinations refused before any run (both keys
//!   named). An entry that still has `include:` (the manifest an image carries, whose bundles are
//!   not in the image) is checked for its own keys only.
//! - [`spec_fields`]: the declaration keys the HYDO-CHECK line carries (as given, resolved).
//!
//! The checker reads the same keys (hydo-history `messages::decl`) and refuses a key it does not
//! know, so a console declaring a newer claim is an ERROR, never a pass.

use crate::py::{self, repr};
use crate::{yaml, ManifestError};
use serde_json::{Map, Value};

type R<T> = Result<T, ManifestError>;

fn err(msg: impl Into<String>) -> ManifestError {
    ManifestError {
        message: msg.into(),
    }
}

/// The check type.
pub const TYPE: &str = "messages";

/// The keys a messages check has beside the other checks' `id`, `type`, `history`, `msg`,
/// `final` and `description`.
pub const KEYS: [&str; 6] = [
    "include",
    "acknowledgement",
    "retention_ms",
    "clock_steps",
    "claims",
    "measures",
];

/// The declaration keys (what a bundle may set, beside `msg` and `description`).
const DECL: [&str; 5] = [
    "acknowledgement",
    "retention_ms",
    "clock_steps",
    "claims",
    "measures",
];

const CLAIMS: [&str; 11] = [
    "delivery",
    "duplicates",
    "after_ack",
    "processing",
    "lease",
    "order",
    "dead_letter",
    "delay",
    "fan_out",
    "members",
    "linearizable",
];

const MEASURES: [&str; 3] = ["windows", "quantiles", "channels"];

fn is_messages(c: &Value) -> bool {
    c.get("type").and_then(Value::as_str) == Some(TYPE)
}

/// Merges `src` into `into`: each key replaces the earlier one whole; `claims:` and `measures:`
/// per key.
fn merge(into: &mut Map<String, Value>, src: &Map<String, Value>) {
    for (k, v) in src {
        match (k.as_str(), into.get_mut(k), v) {
            ("claims" | "measures", Some(Value::Object(a)), Value::Object(b)) => {
                for (k2, v2) in b {
                    a.insert(k2.clone(), v2.clone());
                }
            }
            _ => {
                into.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Resolves the `include:` bundles of every messages check of `m`, with paths relative to `dir`.
pub fn resolve_includes(m: &mut Value, dir: &str) -> R<()> {
    let Some(Value::Array(checks)) = m.get_mut("checks") else {
        return Ok(());
    };
    for (i, c) in checks.iter_mut().enumerate() {
        if !is_messages(c) {
            continue;
        }
        let Some(inc) = c.get("include").cloned() else {
            continue;
        };
        let wh = format!("checks[{i}]");
        let Value::Array(files) = inc else {
            return Err(err(format!(
                "{wh}: include: a list of bundle files (YAML, relative to the manifest)"
            )));
        };
        let mut merged = Map::new();
        for f in &files {
            let Some(rel) = f.as_str().filter(|s| !s.is_empty()) else {
                return Err(err(format!(
                    "{wh}: include: a list of bundle files, not {}",
                    repr(f)
                )));
            };
            let path = if rel.starts_with('/') {
                rel.to_string()
            } else {
                format!("{dir}/{rel}")
            };
            let text = std::fs::read_to_string(&path).map_err(|_| {
                err(format!(
                    "{wh}: include: {rel}: no such bundle file (looked for {path})"
                ))
            })?;
            let b = yaml::parse_yaml(&text, &py::basename(&path))?;
            let Value::Object(b) = b else {
                return Err(err(format!("{wh}: include: {rel}: a bundle is a mapping")));
            };
            for k in b.keys() {
                if !DECL.contains(&k.as_str()) && k != "msg" && k != "description" {
                    return Err(err(format!(
                        "{wh}: include: {rel}: a bundle has the keys of a declaration ({}, msg, description), not {k}",
                        DECL.join(", ")
                    )));
                }
            }
            merge(&mut merged, &b);
        }
        let Value::Object(own) = c else { continue };
        let mut own = own.clone();
        own.remove("include");
        let mut out = Map::new();
        // the check's own place keys first (id, type, history), then the bundles, then its keys
        for k in ["id", "type", "history"] {
            if let Some(v) = own.get(k) {
                out.insert(k.into(), v.clone());
            }
        }
        merge(&mut out, &merged);
        merge(&mut out, &own);
        *c = Value::Object(out);
    }
    Ok(())
}

fn word<'a>(v: &'a Value, wh: &str, allowed: &[&str]) -> R<&'a str> {
    match v.as_str() {
        Some(s) if allowed.contains(&s) => Ok(s),
        _ => Err(err(format!(
            "{wh}: one of {}, not {}",
            allowed.join(", "),
            repr(v)
        ))),
    }
}

fn mapping<'a>(v: &'a Value, wh: &str, known: &[&str]) -> R<&'a Map<String, Value>> {
    let Value::Object(m) = v else {
        return Err(err(format!("{wh}: a mapping, not {}", repr(v))));
    };
    for k in m.keys() {
        if !known.contains(&k.as_str()) {
            return Err(err(format!(
                "{wh}: unknown field {k} (known: {})",
                known.join(", ")
            )));
        }
    }
    Ok(m)
}

fn whole(v: &Value, wh: &str, min: i64) -> R<i64> {
    match v.as_i64() {
        Some(n) if n >= min && !v.is_boolean() => Ok(n),
        _ => Err(err(format!("{wh}: a whole number, {min} or more"))),
    }
}

/// What a validated declaration says, for the refused combinations.
#[derive(Default)]
struct Facts {
    ack: String,
    loss: bool,
    at_most_once: bool,
    lease: bool,
    after_ack: bool,
    processing: bool,
    dead_letter: bool,
    dead_letter_to: bool,
    fifo_blocking: bool,
    fan_out: bool,
    linearizable: bool,
}

/// Validates a messages check (after `include:` is resolved, or with it unresolved: then only
/// the entry's own keys are checked, and the combinations are not).
pub fn validate(c: &Value, wh: &str) -> R<()> {
    for k in ["model", "initial", "consistency"] {
        if c.get(k).is_some() {
            let t = if k == "consistency" {
                "list-append"
            } else {
                "linearizable"
            };
            return Err(err(format!("{wh}: {k}: is for type: {t}")));
        }
    }
    let mut f = Facts {
        ack: "explicit".into(),
        ..Facts::default()
    };
    let unresolved = match c.get("include") {
        None => false,
        Some(Value::Array(a)) if a.iter().all(|x| x.as_str().is_some_and(|s| !s.is_empty())) => {
            true
        }
        Some(_) => {
            return Err(err(format!(
                "{wh}: include: a list of bundle files (YAML, relative to the manifest)"
            )))
        }
    };
    if let Some(v) = c.get("acknowledgement") {
        f.ack = word(
            v,
            &format!("{wh}: acknowledgement"),
            &["explicit", "on-receive", "none"],
        )?
        .to_string();
    }
    if let Some(v) = c.get("retention_ms") {
        whole(v, &format!("{wh}: retention_ms"), 0)?;
    }
    if let Some(v) = c.get("clock_steps") {
        word(v, &format!("{wh}: clock_steps"), &["undecided", "strict"])?;
    }
    if let Some(v) = c.get("measures") {
        let m = mapping(v, &format!("{wh}: measures"), &MEASURES)?;
        for (k, v) in m {
            let w = format!("{wh}: measures.{k}");
            match k.as_str() {
                "windows" => {
                    if !v.is_boolean() {
                        return Err(err(format!("{w}: true or false")));
                    }
                }
                "quantiles" => {
                    let ok = v.as_array().is_some_and(|a| {
                        a.iter()
                            .all(|x| quantile(x).is_some_and(|f| (0.0..=1.0).contains(&f)))
                    });
                    if !ok {
                        return Err(err(format!("{w}: a list of numbers from 0 to 1")));
                    }
                }
                _ => {
                    let ok = v.as_str() == Some("all")
                        || v.as_array()
                            .is_some_and(|a| a.iter().all(|x| x.as_str().is_some()));
                    if !ok {
                        return Err(err(format!("{w}: all, or a list of channel names")));
                    }
                }
            }
        }
    }
    if let Some(v) = c.get("claims") {
        claims(v, &format!("{wh}: claims"), &mut f)?;
    }
    if unresolved {
        return Ok(());
    }
    let has_final = c.get("final").is_some_and(py::truthy);
    if let Some(e) = conflicts(&f, has_final).first() {
        return Err(err(format!("{wh}: {e}")));
    }
    Ok(())
}

fn claims(v: &Value, wh: &str, f: &mut Facts) -> R<()> {
    let m = mapping(v, wh, &CLAIMS)?;
    for (k, v) in m {
        let w = format!("{wh}.{k}");
        match k.as_str() {
            "delivery" => {
                let s = word(
                    v,
                    &w,
                    &["none", "at-most-once", "at-least-once", "exactly-once"],
                )?;
                f.loss = matches!(s, "at-least-once" | "exactly-once");
                f.at_most_once = matches!(s, "at-most-once" | "exactly-once");
            }
            "duplicates" => {
                if v.is_string() {
                    word(v, &w, &["allowed", "forbidden"])?;
                } else {
                    let d = mapping(v, &w, &["by", "window_ms"])?;
                    word(
                        d.get("by").ok_or_else(|| {
                            err(format!("{w}: by: is required (dedup-id or message)"))
                        })?,
                        &format!("{w}.by"),
                        &["dedup-id", "message"],
                    )?;
                    whole(
                        d.get("window_ms").ok_or_else(|| {
                            err(format!("{w}: window_ms: is required (0 means always)"))
                        })?,
                        &format!("{w}.window_ms"),
                        0,
                    )?;
                }
            }
            "after_ack" => {
                f.after_ack = word(v, &w, &["allowed", "no-redelivery"])? == "no-redelivery";
            }
            "processing" => f.processing = word(v, &w, &["any", "once"])? == "once",
            "lease" => {
                if v.is_string() {
                    word(v, &w, &["none"])?;
                } else {
                    let d = mapping(v, &w, &["ms"])?;
                    let ms = d
                        .get("ms")
                        .ok_or_else(|| err(format!("{w}: ms: is required")))?;
                    if ms.as_str() != Some("per-delivery") {
                        whole(ms, &format!("{w}.ms"), 1)
                            .map_err(|_| err(format!("{w}.ms: a whole number of milliseconds, 1 or more, or per-delivery")))?;
                    }
                    f.lease = true;
                }
            }
            "order" => {
                const KINDS: [&str; 5] = ["none", "fifo", "fifo-blocking", "log", "causal"];
                let kind = if v.is_string() {
                    word(v, &w, &KINDS)?
                } else {
                    let d = mapping(v, &w, &["kind", "scope", "batch"])?;
                    if let Some(s) = d.get("scope") {
                        word(s, &format!("{w}.scope"), &["key", "channel"])?;
                    }
                    if let Some(s) = d.get("batch") {
                        word(s, &format!("{w}.batch"), &["ordered", "unordered"])?;
                    }
                    word(
                        d.get("kind")
                            .ok_or_else(|| err(format!("{w}: kind: is required")))?,
                        &format!("{w}.kind"),
                        &KINDS,
                    )?
                };
                f.fifo_blocking = kind == "fifo-blocking";
            }
            "dead_letter" => {
                if v.is_string() {
                    word(v, &w, &["none"])?;
                } else {
                    let d = mapping(v, &w, &["after", "to"])?;
                    whole(
                        d.get("after")
                            .ok_or_else(|| err(format!("{w}: after: is required")))?,
                        &format!("{w}.after"),
                        1,
                    )?;
                    if let Some(t) = d.get("to") {
                        if t.as_str().is_none_or(|s| s.is_empty()) {
                            return Err(err(format!("{w}.to: a channel name, not {}", repr(t))));
                        }
                        f.dead_letter_to = true;
                    }
                    f.dead_letter = true;
                }
            }
            "delay" => {
                if v.is_string() {
                    word(v, &w, &["none", "per-message"])?;
                } else {
                    let d = mapping(v, &w, &["ms"])?;
                    whole(
                        d.get("ms")
                            .ok_or_else(|| err(format!("{w}: ms: is required")))?,
                        &format!("{w}.ms"),
                        0,
                    )?;
                }
            }
            "fan_out" => f.fan_out = word(v, &w, &["none", "every-group"])? == "every-group",
            "members" => {
                word(v, &w, &["any", "exclusive"])?;
            }
            _ => {
                f.linearizable = word(v, &w, &["none", "fifo", "unordered"])? != "none";
            }
        }
    }
    Ok(())
}

/// The combinations refused before any run, each naming both keys (the same rules as
/// hydo-history's `messages::decl::conflicts`).
fn conflicts(f: &Facts, has_final: bool) -> Vec<String> {
    let mut out = Vec::new();
    if f.linearizable && f.ack != "on-receive" {
        out.push(
            "claims.linearizable needs acknowledgement: on-receive (a queue with \
             acknowledgements redelivers, so it is not one linearizable queue)"
                .to_string(),
        );
    }
    if f.ack != "explicit" {
        for (on, k) in [
            (f.processing, "claims.processing"),
            (f.after_ack, "claims.after_ack"),
            (f.lease, "claims.lease"),
            (f.dead_letter, "claims.dead_letter"),
            (f.fifo_blocking, "claims.order: fifo-blocking"),
        ] {
            if on {
                out.push(format!(
                    "{k} is about acknowledgements, and acknowledgement: {} has none",
                    f.ack
                ));
            }
        }
    }
    if f.at_most_once && f.lease {
        out.push(
            "claims.delivery: at-most-once (or exactly-once) with claims.lease: a lease exists to \
             allow redelivery, which at-most-once forbids"
                .to_string(),
        );
    }
    if !has_final {
        for (on, k) in [
            (f.loss, "claims.delivery"),
            (f.fan_out, "claims.fan_out"),
            (f.processing, "claims.processing"),
            (f.dead_letter_to, "claims.dead_letter (with to:)"),
        ] {
            if on {
                out.push(format!(
                    "{k} reads the end of the history, and the check has no final: (the drain that \
                     reads every message at the end)"
                ));
            }
        }
    }
    out
}

/// The declaration keys of a (resolved) messages check, as given: what its HYDO-CHECK line
/// carries beside `id`, `type`, `history` and `msg`.
pub fn spec_fields(c: &Value, sp: &mut Map<String, Value>) {
    for k in DECL {
        if let Some(v) = c.get(k) {
            let mut v = v.clone();
            // quantiles as numbers (hydo's YAML reads 0.5 as text)
            if let Some(Value::Array(q)) = v.get_mut("quantiles") {
                for x in q.iter_mut() {
                    if let (Value::String(_), Some(f)) = (&*x, quantile(x)) {
                        *x = serde_json::Number::from_f64(f)
                            .map(Value::Number)
                            .unwrap_or(Value::Null);
                    }
                }
            }
            sp.insert(k.to_string(), v);
        }
    }
}

/// A quantile's value: a number, or a number's text.
fn quantile(v: &Value) -> Option<f64> {
    match v {
        Value::String(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => v.as_f64(),
    }
}
