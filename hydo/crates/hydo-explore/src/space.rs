//! The fault space of an onboarded project (the port of hydo/explore/manifest_space.py).
//!
//! For `--manifest <hydo.yaml>` (or an `explore-ob-*` image, whose manifest is read from the image
//! itself) the explorer draws faults from the project's services, ports, links, steps and knobs
//! instead of the four-service stack's fixed names ([`Space::apply`]), and every schedule is
//! filtered by [`Space::restrict`] before it is printed: fault kinds the manifest's `faults:` list
//! leaves out are dropped (a lying disk falls back to a plain power cut when only `cut` is allowed),
//! and so are network faults on services without a port. The draws are unchanged, so a manifest
//! with the stack's names and step count gives the stack explorer's schedules.

use crate::gen::Ex;
use crate::sched::{KV, S};
use hydo_manifest::py::{int_of, str_of, truthy};
use serde_json::{json, Map, Value};
use std::path::Path;

pub const FAULT_KINDS: [&str; 7] = ["kill", "pause", "network", "oneway", "cut", "clock", "disk"];
/// Guest tokens with a fixed meaning; a manifest knob may not reuse one.
pub const RESERVED_TOKENS: [&str; 12] = ["K", "P", "N", "O", "C", "T", "Q", "X", "B", "F", "H", "W"];
pub const PF_REQ_KINDS: [&str; 4] = ["status", "delay", "drop", "reset"];
pub const PF_HOOK_KINDS: [&str; 4] = ["dup", "reorder", "late", "lost"];
pub const PF_TCP_KINDS: [&str; 3] = ["delay", "drop", "reset"];

/// An H= target: a fake's route, a proxy, or a proxy's route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PfTarget {
    pub name: String,
    pub req: Vec<String>,
    pub hook: Vec<String>,
    pub proto: String,
}

#[derive(Clone, Debug)]
pub struct Space {
    /// The validated manifest data (as hydo-build holds it).
    pub m: Value,
    pub name: String,
    /// The test composer's plan meta, when the manifest has a plan.
    pub compose: Option<Value>,
    pub svcs: Vec<String>,
    pub ported: Vec<String>,
    pub links: Vec<String>,
    pub fc_pairs: Vec<(String, String)>,
    pub pf_targets: Vec<PfTarget>,
    pub steps: i64,
    pub faults: Vec<String>,
    /// (token, safe value, relaxed values)
    pub knobs: Vec<(String, String, Vec<String>)>,
}

fn arr(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

fn get<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k).filter(|x| !x.is_null())
}

/// Python `str()` of a scalar read from the manifest.
fn pstr(v: &Value) -> String {
    match v {
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(_) => crate::py::py_str(v),
        _ => str_of(v),
    }
}

impl Space {
    /// manifest_space.Space(text...) after hydo-manifest's reading: `m` validated, `compose` the
    /// plan meta (None without a plan).
    pub fn new(m: Value, compose: Option<Value>) -> Result<Space, String> {
        let services = arr(m.get("services"));
        let name_of = |s: &Value| str_of(s.get("name").unwrap_or(&Value::Null));
        let svcs: Vec<String> = services.iter().map(name_of).collect();
        let ported: Vec<String> = services
            .iter()
            .filter(|s| get(s, "port").is_some_and(truthy))
            .map(name_of)
            .collect();
        let addr: Vec<String> = services
            .iter()
            .filter(|s| get(s, "addr").is_some_and(truthy))
            .map(name_of)
            .collect();
        let mut links: Vec<String> = Vec::new();
        if addr.len() >= 2 {
            for (i, a) in addr.iter().enumerate() {
                for b in &addr[i + 1..] {
                    links.push(format!("{a}-{b}"));
                }
            }
            if addr.len() > 2 {
                links.extend(addr.iter().map(|a| format!("{a}-*")));
            }
        }
        if m.get("network").and_then(Value::as_str) == Some("bridge") {
            links = bridge_links(&services);
        }
        let fc_pairs = fc_pairs(&services);
        let pf_targets = pf_targets(&m);
        let steps = m
            .get("workload")
            .and_then(|w| w.get("steps"))
            .and_then(int_of)
            .unwrap_or(0) as i64;
        if steps < 4 {
            return Err(format!(
                "manifest_space: workload.steps={steps}; the generators need at least 4 steps"
            ));
        }
        let faults: Vec<String> = match m.get("faults") {
            Some(v) if truthy(v) => arr(Some(v)).iter().map(str_of).collect(),
            _ => FAULT_KINDS.iter().map(|s| s.to_string()).collect(),
        };
        let mut knobs = Vec::new();
        for k in arr(m.get("knobs")) {
            let safe = pstr(
                k.get("safe")
                    .or_else(|| k.get("default"))
                    .unwrap_or(&Value::Null),
            );
            let vals: Vec<String> = arr(k.get("values"))
                .iter()
                .map(pstr)
                .filter(|v| *v != safe)
                .collect();
            let tok = str_of(k.get("token").unwrap_or(&Value::Null));
            if RESERVED_TOKENS.contains(&tok.as_str()) {
                let mut r = RESERVED_TOKENS.to_vec();
                r.sort();
                return Err(format!(
                    "manifest_space: knob token {} collides with a built-in schedule token \
                     (reserved: {}); rename the knob",
                    crate::py::repr(&tok),
                    r.join(" ")
                ));
            }
            knobs.push((tok, safe, vals));
        }
        let name = m
            .get("name")
            .map(str_of)
            .unwrap_or_else(|| "?".to_string());
        Ok(Space {
            m,
            name,
            compose,
            svcs,
            ported,
            links,
            fc_pairs,
            pf_targets,
            steps,
            faults,
            knobs,
        })
    }

    /// manifest_space.load(manifest=path).
    pub fn from_path(path: &Path) -> Result<Space, String> {
        let (m, meta) = hydo_manifest::for_explorer(path).map_err(|e| e.message)?;
        Space::new(m, meta)
    }

    /// Space(text, "hydo.yaml", compose=meta): the manifest an image carries.
    pub fn from_text(text: &str, compose: Option<Value>) -> Result<Space, String> {
        let (man, meta) =
            hydo_manifest::from_embedded_meta(text, compose.as_ref()).map_err(|e| e.message)?;
        Space::new(man.data, meta)
    }

    pub fn has_fault(&self, k: &str) -> bool {
        self.faults.iter().any(|f| f == k)
    }

    /// freeze_match: as (string, direction): '<' replies, '>' requests, None either.
    pub fn fc_match(&self) -> Vec<(String, Option<char>)> {
        match self.m.get("freeze_match") {
            Some(Value::Object(o)) => {
                let mut out: Vec<(String, Option<char>)> = arr(o.get("replies"))
                    .iter()
                    .map(|x| (str_of(x), Some('<')))
                    .collect();
                out.extend(arr(o.get("requests")).iter().map(|x| (str_of(x), Some('>'))));
                out
            }
            Some(v) if truthy(v) => arr(Some(v)).iter().map(|x| (str_of(x), None)).collect(),
            _ => Vec::new(),
        }
    }

    /// True when the workload is the test composer's (the explorer then draws W=).
    pub fn composed(&self) -> bool {
        self.compose
            .as_ref()
            .is_some_and(|c| truthy(c) && c.get("composed").is_some_and(truthy))
    }

    /// `describe()`: the "fault space:" line.
    pub fn describe(&self) -> Value {
        let mut d = Map::new();
        d.insert("project".into(), json!(self.name));
        d.insert("services".into(), json!(self.svcs));
        let mut net = self.ported.clone();
        net.extend(self.links.iter().cloned());
        d.insert("network".into(), json!(net));
        d.insert("steps".into(), json!(self.steps));
        d.insert("faults".into(), json!(self.faults));
        let mut k = Map::new();
        for (t, s, v) in &self.knobs {
            k.insert(t.clone(), json!({"safe": s, "relaxed": v}));
        }
        d.insert("knobs".into(), Value::Object(k));
        if !self.pf_targets.is_empty() {
            d.insert(
                "protocol_targets".into(),
                json!(self
                    .pf_targets
                    .iter()
                    .map(|t| {
                        let mut ks = t.req.clone();
                        ks.extend(t.hook.iter().cloned());
                        format!("{}:{}", t.name, ks.join("/"))
                    })
                    .collect::<Vec<_>>()),
            );
        }
        if self.composed() {
            let c = self.compose.as_ref().expect("composed");
            let ids: Vec<Value> = arr(c.get("commands"))
                .iter()
                .map(|x| x.get("id").cloned().unwrap_or(Value::Null))
                .collect();
            let checks: Vec<Value> = arr(c.get("checks"))
                .iter()
                .map(|x| x.get("name").cloned().unwrap_or(Value::Null))
                .collect();
            d.insert(
                "composer".into(),
                json!({"commands": ids, "checks": checks, "params": c.get("params").cloned().unwrap_or(Value::Null)}),
            );
        }
        if self.has_fault("freeze") {
            d.insert(
                "freeze_pairs".into(),
                json!(self
                    .fc_pairs
                    .iter()
                    .map(|(a, b)| format!("{a}>{b}"))
                    .collect::<Vec<_>>()),
            );
            d.insert(
                "freeze_match".into(),
                json!(self
                    .fc_match()
                    .iter()
                    .map(|(x, d)| format!(
                        "{}{}",
                        match d {
                            Some('<') => "reply:",
                            Some('>') => "request:",
                            _ => "",
                        },
                        x
                    ))
                    .collect::<Vec<_>>()),
            );
        }
        Value::Object(d)
    }

    /// Rebind the explorer's generator fields (manifest_space.Space.apply).
    pub fn apply(self, ex: &mut Ex) {
        let dash = || vec!["-".to_string()];
        ex.svcs = if self.svcs.is_empty() {
            dash()
        } else {
            self.svcs.clone()
        };
        let mut net = self.ported.clone();
        net.extend(self.links.iter().cloned());
        ex.net_svcs = if !net.is_empty() {
            net
        } else if !self.svcs.is_empty() {
            self.svcs.clone()
        } else {
            dash()
        };
        ex.pause_svcs = ex.svcs.clone();
        ex.norders = self.steps;
        let pairs: Vec<(String, KV)> = self
            .knobs
            .iter()
            .flat_map(|(t, _, vals)| vals.iter().map(move |v| (t.clone(), KV::Str(v.clone()))))
            .collect();
        ex.knobs = if pairs.is_empty() {
            vec![("-".to_string(), KV::Str(String::new()))]
        } else {
            pairs
        };
        ex.knob_tokens = self.knobs.iter().map(|(t, _, _)| t.clone()).collect();
        if self.has_fault("freeze") {
            ex.fc_pairs = self.fc_pairs.clone();
            ex.fc_match = self.fc_match();
        }
        if !self.pf_targets.is_empty() {
            ex.pf_targets = self.pf_targets.clone();
        }
        ex.space = Some(self);
    }

    /// Drop what this project does not allow. Idempotent; never adds anything.
    pub fn restrict(&self, s: &S) -> S {
        let mut s = s.clone();
        if !self.has_fault("kill") {
            s.k = None;
        }
        if !self.has_fault("pause") {
            s.p = None;
        }
        if !self.has_fault("network") {
            s.n = None;
        }
        if !self.has_fault("oneway") {
            s.o = None;
        }
        if !self.has_fault("clock") {
            s.t = None;
        }
        let net_ok = |x: &str| self.ported.iter().any(|p| p == x) || self.links.iter().any(|p| p == x);
        if s.n.as_ref().is_some_and(|n| !net_ok(&n.0)) {
            s.n = None;
        }
        if s.o.as_ref().is_some_and(|o| !net_ok(&o.0)) {
            s.o = None;
        }
        if let Some(f) = &s.f {
            let svc_ok = self.svcs.contains(&f.svc);
            let peer_ok = self.svcs.contains(&f.peer) || f.peer == "workload";
            if !self.has_fault("freeze") || !svc_ok || !peer_ok {
                s.f = None;
            }
        }
        if !s.h.is_empty() {
            s.h.retain(|h| self.pf_targets.iter().any(|t| t.name == h.target));
        }
        if s.k.as_ref().is_some_and(|k| k.0 == "-") {
            s.k = None;
        }
        if s.p.as_ref().is_some_and(|p| p.svc() == "-") {
            s.p = None;
        }
        if s.x.as_ref().is_some_and(|x| x.0 == "-") {
            s.x = None;
        }
        if (s.disk_is("lying") || s.disk_is("ioerr")) && !self.has_fault("disk") {
            s.disk = if s.disk_is("lying") && self.has_fault("cut") {
                "cut".into()
            } else {
                "none".into()
            };
        }
        if s.disk == "cut" && !self.has_fault("cut") {
            s.disk = "none".into();
        }
        if s.disk == "none" || s.disk_is("ioerr") {
            s.c = None;
        }
        // single upper-case keys other than K N C P T O F H: knobs only when the manifest has the
        // token; B only with buggify: true; W only for a composed plan; Q and X never.
        let tokens: Vec<&str> = self.knobs.iter().map(|(t, _, _)| t.as_str()).collect();
        s.knobs
            .retain(|(t, _)| t != "-" && (!is_single_upper(t) || tokens.contains(&t.as_str())));
        if s.b.is_some() && self.m.get("buggify") != Some(&Value::Bool(true)) {
            s.b = None;
        }
        if s.w.is_some() && !self.composed() {
            s.w = None;
        }
        s.cq = None;
        s.x = None;
        s
    }

    pub fn check_sweep(&self, name: &str) -> Result<(), String> {
        let kinds: &[&str] = match name {
            "kill" => &["kill"],
            "cut" => &["cut"],
            "net" => &["network"],
            "killcut" => &["kill", "cut"],
            "pause" => &["pause"],
            "clock" => &["clock"],
            "oneway" => &["oneway"],
            "freeze" => &["freeze"],
            _ => &[],
        };
        let need: Vec<&&str> = kinds.iter().filter(|k| !self.has_fault(k)).collect();
        if name == "protocol" && self.pf_targets.is_empty() {
            return Err("manifest_space: sweep protocol needs fakes: or proxies: (and `protocol` in faults:, if listed)".into());
        }
        if !need.is_empty()
            || ((name == "net" || name == "oneway") && self.ported.is_empty() && self.links.is_empty())
        {
            let listed = match name {
                "kill" | "cut" | "net" | "killcut" | "pause" | "clock" | "oneway" | "freeze"
                | "protocol" => format!(
                    "[{}]",
                    kinds
                        .iter()
                        .map(|k| crate::py::repr(k))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                _ => "None".into(),
            };
            return Err(format!(
                "manifest_space: sweep {} needs fault kinds {} and a service with a port; {} allows [{}]",
                crate::py::repr(name),
                listed,
                self.name,
                self.faults
                    .iter()
                    .map(|k| crate::py::repr(k))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Ok(())
    }

    pub fn unsafe_knob(&self, s: &S) -> bool {
        self.knobs
            .iter()
            .any(|(t, safe, _)| s.knob(t).is_some_and(|v| v.to_string() != *safe))
    }
}

fn is_single_upper(t: &str) -> bool {
    let mut c = t.chars();
    matches!((c.next(), c.next()), (Some(ch), None) if ch.is_uppercase())
}

/// network: bridge: links only between services that talk (depends_on), see manifest_space._links.
fn bridge_links(services: &[Value]) -> Vec<String> {
    let names: Vec<String> = services
        .iter()
        .filter(|s| get(s, "addr").is_some_and(truthy))
        .map(|s| str_of(s.get("name").unwrap_or(&Value::Null)))
        .collect();
    let deps = |n: &str| -> Vec<String> {
        services
            .iter()
            .find(|s| s.get("name").map(str_of).as_deref() == Some(n))
            .map(|s| arr(s.get("depends_on")).iter().map(str_of).collect())
            .unwrap_or_default()
    };
    let talk = |a: &str, b: &str| deps(a).iter().any(|x| x == b) || deps(b).iter().any(|x| x == a);
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (i, a) in names.iter().enumerate() {
        for b in &names[i + 1..] {
            if talk(a, b) {
                pairs.push((a.clone(), b.clone()));
            }
        }
    }
    let iso: Vec<String> = if pairs.is_empty() {
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                pairs.push((a.clone(), b.clone()));
            }
        }
        if names.len() > 2 {
            names.clone()
        } else {
            Vec::new()
        }
    } else {
        names
            .iter()
            .filter(|a| pairs.iter().filter(|(x, y)| x == *a || y == *a).count() >= 2)
            .cloned()
            .collect()
    };
    let mut out: Vec<String> = pairs.iter().map(|(a, b)| format!("{a}-{b}")).collect();
    out.extend(iso.iter().map(|a| format!("{a}-*")));
    out
}

/// (svc, peer) pairs for F= (manifest_space._fc_pairs).
fn fc_pairs(services: &[Value]) -> Vec<(String, String)> {
    let names: Vec<String> = services
        .iter()
        .map(|s| str_of(s.get("name").unwrap_or(&Value::Null)))
        .collect();
    let by = |n: &str| services.iter().find(|s| s.get("name").map(str_of).as_deref() == Some(n));
    let ported = |n: &str| by(n).is_some_and(|s| get(s, "port").is_some_and(truthy));
    let ok = |a: &str, b: &str| a != b && ported(b);
    let closure = |a: &str| -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        let mut todo: Vec<String> = by(a)
            .map(|s| arr(s.get("depends_on")).iter().map(str_of).collect())
            .unwrap_or_default();
        while !todo.is_empty() {
            let d = todo.remove(0);
            if let Some(sd) = by(&d) {
                if !seen.contains(&d) {
                    seen.push(d.clone());
                    todo.extend(arr(sd.get("depends_on")).iter().map(str_of));
                }
            }
        }
        seen
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    for a in &names {
        for b in closure(a) {
            if ok(a, &b) {
                pairs.push((a.clone(), b));
            }
        }
    }
    if pairs.is_empty() {
        for a in &names {
            for b in &names {
                if ok(a, b) {
                    pairs.push((a.clone(), b.clone()));
                }
            }
        }
    }
    if pairs.is_empty() {
        for a in &names {
            if ported(a) {
                pairs.push((a.clone(), "workload".into()));
            }
        }
    }
    pairs
}

/// H= targets (manifest_space._pf_targets).
fn pf_targets(m: &Value) -> Vec<PfTarget> {
    if let Some(f) = m.as_object().and_then(|o| o.get("faults")) {
        let listed = arr(Some(f));
        if !listed.iter().any(|x| x.as_str() == Some("protocol")) {
            return Vec::new();
        }
    }
    let svec = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut out = Vec::new();
    for e in arr(m.get("fakes")) {
        let proto = e
            .get("protocol")
            .map(str_of)
            .unwrap_or_else(|| "http".into());
        for r in arr(e.get("routes")) {
            out.push(PfTarget {
                name: format!(
                    "{}.{}",
                    str_of(e.get("name").unwrap_or(&Value::Null)),
                    str_of(r.get("name").unwrap_or(&Value::Null))
                ),
                req: svec(&PF_REQ_KINDS),
                hook: if get(&r, "webhook").is_some_and(truthy) {
                    svec(&PF_HOOK_KINDS)
                } else {
                    Vec::new()
                },
                proto: proto.clone(),
            });
        }
    }
    for e in arr(m.get("proxies")) {
        let proto = e
            .get("protocol")
            .map(str_of)
            .unwrap_or_else(|| "http".into());
        let name = str_of(e.get("name").unwrap_or(&Value::Null));
        if proto == "tcp" {
            out.push(PfTarget {
                name,
                req: svec(&PF_TCP_KINDS),
                hook: Vec::new(),
                proto: "tcp".into(),
            });
            continue;
        }
        out.push(PfTarget {
            name: name.clone(),
            req: svec(&PF_REQ_KINDS),
            hook: Vec::new(),
            proto: proto.clone(),
        });
        for r in arr(e.get("routes")) {
            out.push(PfTarget {
                name: format!(
                    "{}.{}",
                    name,
                    str_of(r.get("name").unwrap_or(&Value::Null))
                ),
                req: svec(&PF_REQ_KINDS),
                hook: Vec::new(),
                proto: proto.clone(),
            });
        }
    }
    out
}

/// grpc_faults.targets_of(m): (target, method paths) of the manifest's gRPC proxies.
pub fn grpc_targets(m: Option<&Value>) -> Vec<(String, Vec<String>)> {
    let Some(m) = m else { return Vec::new() };
    if let Some(f) = m.as_object().and_then(|o| o.get("faults")) {
        if !arr(Some(f)).iter().any(|x| x.as_str() == Some("protocol")) {
            return Vec::new();
        }
    }
    let mut out = Vec::new();
    for e in arr(m.get("proxies")) {
        let proto = e
            .get("protocol")
            .map(str_of)
            .unwrap_or_else(|| "http".into());
        if proto != "grpc" {
            continue;
        }
        let name = str_of(e.get("name").unwrap_or(&Value::Null));
        let routes = arr(e.get("routes"));
        let paths: Vec<String> = routes
            .iter()
            .filter_map(|r| get(r, "path").filter(|p| truthy(p)).map(str_of))
            .filter(|p| !p.contains('*'))
            .collect();
        out.push((name.clone(), paths));
        for r in &routes {
            out.push((
                format!("{}.{}", name, str_of(r.get("name").unwrap_or(&Value::Null))),
                Vec::new(),
            ));
        }
    }
    out
}
