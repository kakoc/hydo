//! Typed views of a validated manifest. They are built from the data `validate` accepted, so
//! every required field is present; free-form parts (env, root files, data, shell) stay JSON.

use serde_json::Value;

use crate::py::{int_of, str_of, truthy};

fn s_of(v: Option<&Value>) -> Option<String> {
    v.filter(|x| !x.is_null()).map(str_of)
}

fn i_of(v: Option<&Value>) -> Option<i64> {
    v.and_then(int_of).map(|i| i as i64)
}

fn strs(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(str_of).collect(),
        Some(Value::String(s)) if !s.is_empty() => s.chars().map(|c| c.to_string()).collect(),
        _ => Vec::new(),
    }
}

fn list(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

/// A command to run: an argv list (exec'd, no shell) or a shell command string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    Argv(Vec<String>),
    Shell(String),
}

impl Run {
    pub fn from_value(v: &Value) -> Run {
        match v {
            Value::Array(a) => Run::Argv(a.iter().map(str_of).collect()),
            other => Run::Shell(str_of(other)),
        }
    }

    /// The guest's command line (hydo_composer.shell_of).
    pub fn shell(&self) -> String {
        match self {
            Run::Argv(a) => crate::composer::shell_of(&Value::Array(
                a.iter().cloned().map(Value::String).collect(),
            )),
            Run::Shell(s) => s.clone(),
        }
    }
}

/// The `guest:` section, with hydo-build's defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guest {
    pub ram_mib: u32,
    pub cpu: String,
    pub clock_start: Option<Value>,
    pub clock_after_cut: Option<i64>,
}

impl Guest {
    pub fn from_value(m: &Value) -> Guest {
        let g = m.get("guest");
        let get = |k: &str| g.and_then(|g| g.get(k));
        Guest {
            ram_mib: get("ram_mib").and_then(Value::as_u64).unwrap_or(1024) as u32,
            cpu: get("cpu")
                .and_then(Value::as_str)
                .unwrap_or("v2")
                .to_string(),
            clock_start: get("clock_start").cloned(),
            clock_after_cut: i_of(get("clock_after_cut")),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    pub name: String,
    /// None for an image service whose command comes from the image.
    pub cmd: Option<String>,
    pub setup: Option<String>,
    pub ready: String,
    pub ready_timeout: Option<i64>,
    pub port: Option<i64>,
    pub addr: Option<String>,
    pub depends_on: Vec<String>,
    pub recovery: Option<String>,
    /// `errors:` of this service (error logs as findings), as written.
    pub errors: Option<Value>,
    /// `image:` and the other OCI keys, as written (hydo_oci.py reads them).
    pub image: Option<Value>,
    pub raw: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvariantType {
    Always,
    AlwaysOrUnreachable,
    Eventually,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Invariant {
    pub id: String,
    pub kind: InvariantType,
    pub cmd: String,
    pub msg: Option<String>,
    pub every: Option<i64>,
    pub final_: bool,
    pub within: Option<i64>,
    pub after: Option<String>,
    pub poll: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Knob {
    pub token: String,
    pub name: Option<String>,
    pub default: String,
    pub set: String,
    pub values: Vec<String>,
    pub safe: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Workload {
    pub steps: i64,
    pub run: String,
    pub retry: Option<Value>,
    pub process: Option<String>,
    pub timeout: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub name: String,
    pub method: Option<String>,
    pub path: Option<String>,
    pub status: Option<i64>,
    pub code: Option<i64>,
    pub reference: Option<String>,
    pub effect: Option<bool>,
    pub idempotent: Option<bool>,
    pub webhook: Option<Value>,
    pub raw: Value,
}

/// A fake outside service (`fakes:`) or a fault proxy (`proxies:`).
#[derive(Clone, Debug, PartialEq)]
pub struct World {
    pub name: String,
    pub port: i64,
    pub addr: Option<String>,
    pub protocol: String,
    pub idempotency: Option<String>,
    pub reference: Option<String>,
    pub routes: Vec<Route>,
    /// proxies only: `to:` or `service:`
    pub to: Option<String>,
    pub service: Option<String>,
}

/// A history check (`checks:`, hydo/HISTORIES.md).
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryCheck {
    pub id: String,
    pub kind: String,
    pub history: String,
    pub model: Option<String>,
    pub initial: Option<String>,
    pub consistency: Option<String>,
    pub msg: Option<String>,
    pub final_: Option<String>,
}

/// The test composer's plan after expansion (what /etc/hydo/compose.json holds).
#[derive(Clone, Debug, PartialEq)]
pub struct TestPlan {
    pub params: Vec<(String, i64)>,
    pub commands: Vec<Command>,
    pub checks: Vec<CheckCmd>,
    pub regressions: Vec<Regression>,
    pub composed: bool,
    pub steps: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub run: Run,
    pub timeout: i64,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckCmd {
    pub name: String,
    pub kind: String,
    pub run: Run,
    pub msg: String,
    pub every: Option<i64>,
    pub within: Option<i64>,
    pub after: Option<Value>,
    pub poll: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    Ok,
    Violation,
    Fp(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Regression {
    pub name: String,
    pub schedule: String,
    pub expect: Expect,
    pub commands: Option<Vec<String>>,
    pub compose: Option<String>,
    pub saved_schedule: Option<String>,
    pub contents: Vec<(String, String)>,
}

impl TestPlan {
    pub fn from_meta(meta: &Value) -> TestPlan {
        let params = meta
            .get("params")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), i_of(Some(v)).unwrap_or(0)))
                    .collect()
            })
            .unwrap_or_default();
        let commands = list(meta.get("commands"))
            .iter()
            .map(|c| Command {
                id: str_of(&c["id"]),
                kind: str_of(&c["kind"]),
                name: str_of(&c["name"]),
                run: Run::from_value(&c["run"]),
                timeout: i_of(c.get("timeout")).unwrap_or(0),
                content: s_of(c.get("content")).unwrap_or_default(),
            })
            .collect();
        let checks = list(meta.get("checks"))
            .iter()
            .map(|c| CheckCmd {
                name: str_of(&c["name"]),
                kind: str_of(&c["kind"]),
                run: Run::from_value(&c["run"]),
                msg: s_of(c.get("msg")).unwrap_or_default(),
                every: i_of(c.get("every")),
                within: i_of(c.get("within")),
                after: c.get("after").cloned(),
                poll: i_of(c.get("poll")),
            })
            .collect();
        let regressions = list(meta.get("regressions"))
            .iter()
            .map(|r| {
                let ex = s_of(r.get("expect")).unwrap_or_else(|| "ok".into());
                Regression {
                    name: str_of(&r["name"]),
                    schedule: s_of(r.get("schedule")).unwrap_or_default(),
                    expect: match ex.as_str() {
                        "ok" => Expect::Ok,
                        "violation" => Expect::Violation,
                        other => Expect::Fp(other.trim_start_matches("fp=").to_string()),
                    },
                    commands: r
                        .get("commands")
                        .filter(|c| !c.is_null())
                        .map(|c| strs(Some(c))),
                    compose: s_of(r.get("compose")),
                    saved_schedule: s_of(r.get("saved_schedule")),
                    contents: r
                        .get("contents")
                        .and_then(Value::as_object)
                        .map(|o| o.iter().map(|(k, v)| (k.clone(), str_of(v))).collect())
                        .unwrap_or_default(),
                }
            })
            .collect();
        TestPlan {
            params,
            commands,
            checks,
            regressions,
            composed: meta.get("composed").is_some_and(truthy),
            steps: i_of(meta.get("steps")),
        }
    }
}

/// A validated manifest.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub description: Option<String>,
    pub guest: Guest,
    pub network: Option<String>,
    pub services: Vec<Service>,
    pub provision: Option<String>,
    pub workload: Workload,
    pub invariants: Vec<Invariant>,
    pub knobs: Vec<Knob>,
    /// `faults:` as listed; None = the default kinds.
    pub faults: Option<Vec<String>>,
    pub freeze_match: Option<Value>,
    pub fakes: Vec<World>,
    pub proxies: Vec<World>,
    pub checks: Vec<HistoryCheck>,
    /// `errors:` after validation (`errors: true` is `{}`); None when off.
    pub errors: Option<Value>,
    pub buggify: bool,
    /// The composer's plan, when the manifest has one (`test:` or a hydo/ folder).
    pub test: Option<TestPlan>,
    /// The normalised data, exactly as hydo-build holds it after validation.
    pub data: Value,
}

fn world_of(e: &Value) -> World {
    World {
        name: str_of(&e["name"]),
        port: i_of(e.get("port")).unwrap_or(0),
        addr: s_of(e.get("addr")),
        protocol: s_of(e.get("protocol")).unwrap_or_else(|| "http".into()),
        idempotency: s_of(e.get("idempotency")),
        reference: s_of(e.get("ref")),
        routes: list(e.get("routes"))
            .iter()
            .map(|r| Route {
                name: str_of(&r["name"]),
                method: s_of(r.get("method")),
                path: s_of(r.get("path")),
                status: i_of(r.get("status")),
                code: i_of(r.get("code")),
                reference: s_of(r.get("ref")),
                effect: r.get("effect").and_then(Value::as_bool),
                idempotent: r.get("idempotent").and_then(Value::as_bool),
                webhook: r.get("webhook").cloned().filter(|w| !w.is_null()),
                raw: r.clone(),
            })
            .collect(),
        to: s_of(e.get("to")),
        service: s_of(e.get("service")),
    }
}

impl Manifest {
    /// Typed view of validated data (`data` must have passed `validate`).
    pub fn from_validated(data: Value) -> Manifest {
        let m = &data;
        let services = list(m.get("services"))
            .iter()
            .map(|s| Service {
                name: str_of(&s["name"]),
                cmd: s_of(s.get("cmd")),
                setup: s_of(s.get("setup")),
                ready: s_of(s.get("ready")).unwrap_or_default(),
                ready_timeout: i_of(s.get("ready_timeout")),
                port: i_of(s.get("port")),
                addr: s_of(s.get("addr")),
                depends_on: strs(s.get("depends_on")),
                recovery: s_of(s.get("recovery")),
                errors: s.get("errors").cloned().filter(|x| !x.is_null()),
                image: s.get("image").cloned(),
                raw: s.clone(),
            })
            .collect();
        let wl = m.get("workload").cloned().unwrap_or(Value::Null);
        let invariants = list(m.get("invariants"))
            .iter()
            .map(|v| {
                let t = s_of(v.get("type")).unwrap_or_else(|| "always".into());
                let every = i_of(v.get("every"));
                Invariant {
                    id: str_of(&v["id"]),
                    kind: match t.as_str() {
                        "eventually" => InvariantType::Eventually,
                        "always_or_unreachable" => InvariantType::AlwaysOrUnreachable,
                        _ => InvariantType::Always,
                    },
                    cmd: s_of(v.get("cmd")).unwrap_or_default(),
                    msg: s_of(v.get("msg")),
                    every,
                    final_: match v.get("final") {
                        Some(f) => truthy(f),
                        None => every.is_none(),
                    },
                    within: i_of(v.get("within")),
                    after: s_of(v.get("after")),
                    poll: i_of(v.get("poll")),
                }
            })
            .collect();
        let knobs = list(m.get("knobs"))
            .iter()
            .map(|k| Knob {
                token: str_of(&k["token"]),
                name: s_of(k.get("name")),
                default: str_of(&k["default"]),
                set: str_of(&k["set"]),
                values: strs(k.get("values")),
                safe: s_of(k.get("safe")),
            })
            .collect();
        let checks = list(m.get("checks"))
            .iter()
            .map(|c| HistoryCheck {
                id: str_of(&c["id"]),
                kind: str_of(&c["type"]),
                history: str_of(&c["history"]),
                model: s_of(c.get("model")),
                initial: s_of(c.get("initial")),
                consistency: s_of(c.get("consistency")),
                msg: s_of(c.get("msg")),
                final_: s_of(c.get("final")),
            })
            .collect();
        let errors = match m.get("errors") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => None,
            Some(e) => Some(e.clone()),
        };
        Manifest {
            name: str_of(&m["name"]),
            description: s_of(m.get("description")),
            guest: Guest::from_value(m),
            network: s_of(m.get("network")),
            services,
            provision: s_of(m.get("provision")),
            workload: Workload {
                steps: i_of(wl.get("steps")).unwrap_or(0),
                run: s_of(wl.get("run")).unwrap_or_default(),
                retry: wl.get("retry").cloned(),
                process: s_of(wl.get("process")),
                timeout: wl.get("timeout").cloned(),
            },
            invariants,
            knobs,
            faults: m.get("faults").filter(|f| truthy(f)).map(|f| strs(Some(f))),
            freeze_match: m.get("freeze_match").cloned().filter(|x| !x.is_null()),
            fakes: list(m.get("fakes")).iter().map(world_of).collect(),
            proxies: list(m.get("proxies")).iter().map(world_of).collect(),
            checks,
            errors,
            buggify: m.get("buggify") == Some(&Value::Bool(true)),
            test: m
                .get("_compose")
                .filter(|c| !c.is_null())
                .map(TestPlan::from_meta),
            data,
        }
    }

    /// The normalised data (as hydo-build holds it after validate; `_compose` is the plan meta).
    pub fn to_json(&self) -> &Value {
        &self.data
    }
}
