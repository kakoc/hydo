//! The manifest compiled for the guest: `/etc/hydo/manifest.sh` (variables the generic init
//! reads), `/etc/hydo/prelude.sh` (the project's environment and shell helpers),
//! `/etc/hydo/world.json` (what hydo-world serves) and `/etc/hosts`. Port of hydo-build's
//! `compile_shell`, `compile_checks`, `check_spec`, `compile_errors`, `world_config`,
//! `world_targets`, `world_addrs`, `default_hosts` and hydo_composer's `shell_lines`, with the
//! same lines in the same order, so every image keeps its bytes.

use crate::pyv::{self, get, int, pct_d, s, section};
use crate::{pyjson, Ctx, Fail};
use hydo_manifest::py;
use serde_json::{Map, Value};

pub const NET_GW: &str = "10.89.0.1";

/// `shq(s)`: one shell word in single quotes.
pub fn shq(x: &str) -> String {
    format!("'{}'", x.replace('\'', "'\\''"))
}

fn shqv(v: &Value) -> String {
    shq(&s(v))
}

fn arr<'a>(m: &'a Value, k: &str) -> Vec<&'a Value> {
    match m.get(k) {
        Some(Value::Array(a)) => a.iter().collect(),
        _ => Vec::new(),
    }
}

fn has(m: &Value, k: &str) -> bool {
    m.as_object().is_some_and(|o| o.contains_key(k))
}

pub fn has_netns(m: &Value) -> bool {
    m.get("network").and_then(Value::as_str) == Some("bridge")
}

pub fn has_links(m: &Value) -> bool {
    arr(m, "services").iter().any(|s| has(s, "addr"))
}

pub fn has_freeze(m: &Value) -> bool {
    faults_listed(m).iter().any(|f| f == "freeze")
}

/// `m.get("faults") or []` as strings (validated: known kinds).
fn faults_listed(m: &Value) -> Vec<String> {
    match m.get("faults") {
        Some(Value::Array(a)) => a.iter().map(py::str_of).collect(),
        _ => Vec::new(),
    }
}

/// `set(m.get("faults") or DEFAULT_FAULT_KINDS)`.
pub fn fault_kinds(m: &Value) -> Vec<String> {
    let listed = match m.get("faults") {
        Some(v) if py::truthy(v) => faults_listed(m),
        _ => Vec::new(),
    };
    if listed.is_empty() {
        ["kill", "pause", "network", "oneway", "cut", "clock", "disk"]
            .iter()
            .map(|x| x.to_string())
            .collect()
    } else {
        listed
    }
}

pub fn has_world(m: &Value) -> bool {
    m.get("fakes").is_some_and(py::truthy) || m.get("proxies").is_some_and(py::truthy)
}

/// `m.get("errors") not in (None, False)` (0 == False in Python, so `errors: 0` is off too).
pub fn has_errlog(m: &Value) -> bool {
    match m.get("errors") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(_) => true,
    }
}

pub fn has_checks(m: &Value) -> bool {
    m.get("checks").is_some_and(py::truthy)
}

pub fn has_eventually(m: &Value) -> bool {
    arr(m, "invariants")
        .iter()
        .any(|v| v.get("type").and_then(Value::as_str) == Some("eventually"))
}

fn worlds(m: &Value) -> Vec<&Value> {
    let mut v = arr(m, "fakes");
    v.extend(arr(m, "proxies"));
    v
}

/// `default_hosts(m)`: /etc/hosts when the manifest gives none.
pub fn default_hosts(m: &Value) -> String {
    let mut h = String::from("127.0.0.1 localhost\n");
    if has_netns(m) {
        h.push_str("::1 localhost ip6-localhost ip6-loopback\n");
        for sv in arr(m, "services") {
            h.push_str(&format!("{} {}\n", s(&sv["addr"]), s(&sv["name"])));
        }
        for e in worlds(m) {
            let a = e.get("addr").map(s).unwrap_or_else(|| NET_GW.to_string());
            h.push_str(&format!("{} {}\n", a, s(&e["name"])));
        }
    }
    h
}

/// `world_targets(m)`: every name an H= token may name.
fn world_targets(m: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for e in worlds(m) {
        let n = s(&e["name"]);
        out.push(n.clone());
        for r in arr(e, "routes") {
            out.push(format!("{}.{}", n, s(&r["name"])));
        }
    }
    out
}

/// `world_addrs(m)`: network: bridge, the addresses hydo-world listens on besides NET_GW.
fn world_addrs(m: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in worlds(m) {
        let a = e.get("addr").map(s).unwrap_or_else(|| NET_GW.to_string());
        if a != NET_GW && !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

const KNOWN_HOOK: &[&str] = &[
    "url", "method", "body", "headers", "delay_ms", "retries", "sign",
];

/// `world_config(m)`: /etc/hydo/world.json.
pub fn world_config(m: &Value) -> String {
    let svcs: Map<String, Value> = arr(m, "services")
        .iter()
        .map(|x| (s(&x["name"]), (*x).clone()))
        .collect();
    let mut cfg = Map::new();
    cfg.insert("log".into(), "/tmp/hydo/world.log".into());
    cfg.insert("sock".into(), "/tmp/hydo/world.sock".into());
    for kind in ["fakes", "proxies"] {
        let mut lst = Vec::new();
        for e in arr(m, kind) {
            let mut c = Map::new();
            for k in ["name", "port", "protocol", "idempotency", "ref"] {
                if let Some(v) = e.get(k) {
                    c.insert(k.into(), v.clone());
                }
            }
            let dflt = if has_netns(m) { NET_GW } else { "127.0.0.1" };
            c.insert(
                "addr".into(),
                Value::String(e.get("addr").map(s).unwrap_or_else(|| dflt.into())),
            );
            if kind == "proxies" {
                if let Some(svc) = e.get("service") {
                    let t = &svcs[&s(svc)];
                    let a = t.get("addr").map(s).unwrap_or_else(|| "127.0.0.1".into());
                    let port = py::int_of(&t["port"])
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| s(&t["port"]));
                    c.insert("to".into(), Value::String(format!("{a}:{port}")));
                } else {
                    c.insert("to".into(), Value::String(s(&e["to"])));
                }
            }
            let mut routes = Vec::new();
            for r in arr(e, "routes") {
                let mut d = Map::new();
                for k in [
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
                    "save",
                    "load",
                ] {
                    if let Some(v) = r.get(k) {
                        d.insert(k.into(), v.clone());
                    }
                }
                if let Some(wh) = r.get("webhook").filter(|w| !w.is_null()) {
                    let mut w = Map::new();
                    for k in KNOWN_HOOK {
                        if let Some(v) = wh.get(*k) {
                            w.insert((*k).into(), v.clone());
                        }
                    }
                    d.insert("webhook".into(), Value::Object(w));
                }
                routes.push(Value::Object(d));
            }
            c.insert("routes".into(), Value::Array(routes));
            lst.push(Value::Object(c));
        }
        cfg.insert(kind.into(), Value::Array(lst));
    }
    pyjson::dumps_indent1_sorted(&Value::Object(cfg)) + "\n"
}

/// `check_spec(c)`: the JSON the guest prints as its HYDO-CHECK line.
fn check_spec(c: &Value) -> String {
    let mut sp = Map::new();
    sp.insert("id".into(), c["id"].clone());
    sp.insert("type".into(), c["type"].clone());
    sp.insert("history".into(), c["history"].clone());
    sp.insert(
        "msg".into(),
        c.get("msg").cloned().unwrap_or_else(|| c["id"].clone()),
    );
    if c["type"].as_str() == Some("linearizable") {
        sp.insert(
            "model".into(),
            c.get("model").cloned().unwrap_or("register".into()),
        );
        sp.insert(
            "initial".into(),
            c.get("initial").cloned().unwrap_or("nil".into()),
        );
    } else {
        sp.insert(
            "consistency".into(),
            c.get("consistency")
                .cloned()
                .unwrap_or("strict-serializable".into()),
        );
    }
    pyjson::dumps_compact_sorted(&Value::Object(sp))
}

fn compile_checks(m: &Value, l: &mut Vec<String>) {
    let mut names = Vec::new();
    for (i, c) in arr(m, "checks").iter().enumerate() {
        let k = format!("c{i}");
        l.push(format!("HYDO_CHECK_{k}_id={}", shqv(&c["id"])));
        l.push(format!("HYDO_CHECK_{k}_json={}", shq(&check_spec(c))));
        let fin = c
            .get("final")
            .filter(|f| py::truthy(f))
            .map(s)
            .unwrap_or_default();
        l.push(format!("HYDO_CHECK_{k}_final={}", shq(&fin)));
        names.push(k);
    }
    l.push(format!("HYDO_CHECKS={}", shq(&names.join(" "))));
}

// ------------------------------------------------------------------ error logs (errors:)
const LEVEL_ERROR: &str = concat!(
    r#"(^|[^A-Za-z0-9_])(ERROR|ERRO|FATAL|FATA|CRITICAL|CRIT|PANIC|SEVERE|EMERG|ALERT)([^A-Za-z0-9_]|$)"#,
    r#"|(^|[^A-Za-z0-9_])(Error|Fatal|Critical|Panic|Severe)(:|\])"#,
    r#"|[Ll][Ee][Vv][Ee][Ll]"?[=:] *"?(error|err|fatal|crit|critical|panic|emerg|alert|ERROR|ERR|FATAL|CRITICAL|PANIC)([^A-Za-z]|$)"#,
    r#"|\[(error|err|fatal|crit|critical|alert|emerg|panic)\]"#,
    r#"|^panic: |^fatal error: "#
);
const LEVEL_WARNING: &str = concat!(
    r#"(^|[^A-Za-z0-9_])(WARN|WARNING)([^A-Za-z0-9_]|$)|(^|[^A-Za-z0-9_])Warning(:|\])"#,
    r#"|[Ll][Ee][Vv][Ee][Ll]"?[=:] *"?(warn|warning|WARN|WARNING)([^A-Za-z]|$)|\[(warn|warning)\]"#
);
const EXCEPTION_LINE: &str = r#"^[A-Za-z_$][A-Za-z0-9_$.]*(Exception|Error)(:|$)"#;
const REDIS_LEVEL: &str = r#"^[0-9]+:[XCSM] [^#]* # "#;

fn errlog_services(m: &Value) -> Vec<String> {
    let e = &m["errors"];
    let all: Vec<String> = arr(m, "services").iter().map(|x| s(&x["name"])).collect();
    let cov: Vec<String> = match e.get("services") {
        Some(Value::Array(a)) if !a.is_empty() => a.iter().map(s).collect(),
        _ => all.clone(),
    };
    arr(m, "services")
        .iter()
        .filter(|x| {
            let n = s(&x["name"]);
            let off = x
                .get("errors")
                .filter(|e| py::truthy(e))
                .and_then(|e| e.get("off"))
                .is_some_and(py::truthy);
            cov.contains(&n) && !off
        })
        .map(|x| s(&x["name"]))
        .collect()
}

fn errlog_level(m: &Value, sv: &Value) -> String {
    let empty = Value::Object(Map::new());
    let se = sv.get("errors").filter(|e| py::truthy(e)).unwrap_or(&empty);
    let fmt = se.get("format").map(s).unwrap_or_else(|| "auto".into());
    let mut alts: Vec<String> = Vec::new();
    if fmt == "auto" {
        alts.push(LEVEL_ERROR.into());
        let top = m["errors"].get("level").cloned().unwrap_or("error".into());
        let lv = se.get("level").cloned().unwrap_or(top);
        if lv.as_str() == Some("warning") {
            alts.push(LEVEL_WARNING.into());
        }
    } else if fmt == "redis" {
        alts.push(REDIS_LEVEL.into());
    }
    if let Some(mt) = se.get("match").filter(|x| py::truthy(x)) {
        alts.push(s(mt));
    }
    alts.iter()
        .map(|a| format!("({a})"))
        .collect::<Vec<_>>()
        .join("|")
}

fn compile_errors(m: &Value, order: &[String], l: &mut Vec<String>) -> Result<(), Fail> {
    let e = &m["errors"];
    let svcs: Map<String, Value> = arr(m, "services")
        .iter()
        .map(|x| (s(&x["name"]), (*x).clone()))
        .collect();
    let cov = errlog_services(m);
    let max = int(e.get("max").unwrap_or(&Value::from(40)), "errors.max")?;
    l.push(format!("HYDO_ERR_MAX={max}"));
    let covered: Vec<&str> = order
        .iter()
        .filter(|n| cov.contains(n))
        .map(|x| x.as_str())
        .collect();
    l.push(format!("HYDO_ERR_SERVICES={}", shq(&covered.join(" "))));
    let empty = Value::Object(Map::new());
    for n in order {
        if !cov.contains(n) {
            continue;
        }
        let sv = &svcs[n];
        let se = sv.get("errors").filter(|e| py::truthy(e)).unwrap_or(&empty);
        let mut norm = Vec::new();
        for r in pyv::items(se.get("normalize"), "errors.normalize")? {
            match &r {
                Value::String(x) => norm.push(format!("{x}\t*")),
                other => {
                    let rep = other.get("replace").map(s).unwrap_or_else(|| "*".into());
                    norm.push(format!("{}\t{}", s(&other["pattern"]), rep));
                }
            }
        }
        let fmt = se.get("format").map(s).unwrap_or_else(|| "auto".into());
        let ignore = se
            .get("ignore")
            .filter(|x| py::truthy(x))
            .map(s)
            .unwrap_or_default();
        let files = match se.get("files") {
            Some(f) if py::truthy(f) => pyv::join_strs(f, " ", "errors.files")?,
            _ => String::new(),
        };
        l.push(format!("HYDO_ERR_{n}_level={}", shq(&errlog_level(m, sv))));
        l.push(format!(
            "HYDO_ERR_{n}_exc={}",
            shq(if fmt == "auto" { EXCEPTION_LINE } else { "" })
        ));
        l.push(format!("HYDO_ERR_{n}_ignore={}", shq(&ignore)));
        l.push(format!("HYDO_ERR_{n}_files={}", shq(&files)));
        l.push(format!("HYDO_ERR_{n}_norm={}", shq(&norm.join("\n"))));
    }
    Ok(())
}

/// hydo_composer's `shell_lines(meta)`.
fn composer_lines(meta: &Value, l: &mut Vec<String>) -> Result<(), Fail> {
    let p = &meta["params"];
    l.push("HYDO_COMPOSE=1".into());
    for k in [
        "parallel",
        "per_round",
        "round_ms",
        "command_timeout",
        "drain_timeout",
        "max_commands",
        "budget_ms",
    ] {
        l.push(format!(
            "HYDO_COMPOSE_{}={}",
            k.to_uppercase(),
            pct_d(&p[k], &format!("test.{k}"))?
        ));
    }
    l.push("HYDO_KNOB_W_default='-'".into());
    l.push(format!(
        "HYDO_KNOB_W_set={}",
        shq("/bin/busybox sh /etc/hydo/compose.sh setw \"$HYDO_VALUE\"")
    ));
    l.push("HYDO_KNOB_W_name='test composer (W=)'".into());
    Ok(())
}

/// The tool paths `compile_shell` writes: tc (or ""), the freeze helper and iptables, `ip`.
pub struct ShellTools {
    pub tc: String,
    pub fc: Option<(String, String)>,
    pub ip: Option<String>,
}

/// `compile_shell(m, tc, fc, ip)` -> (manifest.sh, prelude.sh).
pub fn compile_shell(
    ctx: &Ctx,
    m: &Value,
    order: &[String],
    t: &ShellTools,
) -> Result<(String, String), Fail> {
    let svcs: Map<String, Value> = arr(m, "services")
        .iter()
        .map(|x| (s(&x["name"]), (*x).clone()))
        .collect();
    let wl = &m["workload"];
    let run = s(&wl["run"]);
    let pgrep = match wl.get("process").filter(|p| py::truthy(p)) {
        Some(p) => s(p),
        None => {
            let first = py::split_ws(&run)
                .first()
                .map(|x| x.to_string())
                .ok_or_else(|| {
                    Fail::Crash(
                        "workload.run is only blank space (no program name to watch)".into(),
                    )
                })?;
            py::basename(&first)
        }
    };
    let data = section(m, "data")?;
    let guest = section(m, "guest")?;
    let mut l = vec![
        "# generated by hvx/onboard/hydo-build from hydo.yaml -- do not edit".to_string(),
        format!("HYDO_NAME={}", shqv(&m["name"])),
        format!("HYDO_SERVICES={}", shq(&order.join(" "))),
        format!("HYDO_STEPS={}", pct_d(&wl["steps"], "workload.steps")?),
        format!("HYDO_WORKLOAD_RUN={}", shq(&run)),
        format!(
            "HYDO_WORKLOAD_RETRY={}",
            shq(&wl
                .get("retry")
                .filter(|x| py::truthy(x))
                .map(s)
                .unwrap_or_default())
        ),
        format!("HYDO_WORKLOAD_PGREP={}", shq(&pgrep)),
        format!(
            "HYDO_WORKLOAD_TIMEOUT={}",
            int(
                wl.get("timeout").unwrap_or(&Value::from(300)),
                "workload.timeout"
            )?
        ),
        format!(
            "HYDO_PROVISION={}",
            shq(&m
                .get("provision")
                .filter(|x| py::truthy(x))
                .map(s)
                .unwrap_or_default())
        ),
        format!(
            "HYDO_DATA_OPTS={}",
            shq(&get(data, "mount")?.map(s).unwrap_or_else(|| "rw".into()))
        ),
        format!(
            "HYDO_DATA_DIRS={}",
            shq(&match get(data, "dirs")? {
                Some(d) if py::truthy(d) => pyv::join_strs(d, " ", "data.dirs")?,
                _ => String::new(),
            })
        ),
        format!(
            "HYDO_CLOCK_START={}",
            int(
                get(guest, "clock_start")?.unwrap_or(&Value::from(1767225600)),
                "guest.clock_start"
            )?
        ),
        format!("HYDO_TC={}", shq(&t.tc)),
    ];
    if let Some(c) = get(guest, "clock_after_cut")? {
        l.push(format!(
            "HYDO_CLOCK_AFTER_CUT={}",
            int(c, "guest.clock_after_cut")?
        ));
    }
    if let Some((fc, ipt)) = &t.fc {
        l.push(format!("HYDO_FC={}", shq(fc)));
        l.push(format!("HYDO_IPT={}", shq(ipt)));
    }
    if has_world(m) {
        l.push("HYDO_WORLD=/bin/hydo-world".into());
        l.push(format!(
            "HYDO_WORLD_TARGETS={}",
            shq(&world_targets(m).join(" "))
        ));
    }
    if has_netns(m) {
        l.push("HYDO_NET=bridge".into());
        l.push(format!("HYDO_IP={}", shq(t.ip.as_deref().unwrap_or(""))));
        l.push(format!("HYDO_NET_GW={NET_GW}"));
        l.push(format!("HYDO_NET_EXTRA={}", shq(&world_addrs(m).join(" "))));
    }
    for n in order {
        let sv = &svcs[n];
        let cmd = sv
            .get("cmd")
            .ok_or_else(|| Fail::Crash(format!("services: {n} has no cmd: (KeyError 'cmd')")))?;
        l.push(format!("HYDO_SVC_{n}_cmd={}", shqv(cmd)));
        l.push(format!(
            "HYDO_SVC_{n}_setup={}",
            shq(&sv
                .get("setup")
                .filter(|x| py::truthy(x))
                .map(s)
                .unwrap_or_default())
        ));
        l.push(format!("HYDO_SVC_{n}_ready={}", shqv(&sv["ready"])));
        l.push(format!(
            "HYDO_SVC_{n}_timeout={}",
            int(
                sv.get("ready_timeout").unwrap_or(&Value::from(120)),
                &format!("services.{n}.ready_timeout")
            )?
        ));
        l.push(format!(
            "HYDO_SVC_{n}_port={}",
            shq(&sv.get("port").map(s).unwrap_or_default())
        ));
        let rec = sv.get("recovery").map(py::truthy).unwrap_or(true);
        l.push(format!("HYDO_SVC_{n}_recovery={}", if rec { 1 } else { 0 }));
        if let Some(a) = sv.get("addr") {
            l.push(format!("HYDO_SVC_{n}_addr={}", shqv(a)));
        }
    }
    let mut inv_names = Vec::new();
    for (i, v) in arr(m, "invariants").iter().enumerate() {
        let k = format!("i{i}");
        inv_names.push(k.clone());
        let every = v.get("every");
        let fin = match v.get("final") {
            Some(f) => py::truthy(f),
            None => every.is_none_or(|e| e.is_null()),
        };
        l.push(format!("HYDO_INV_{k}_id={}", shqv(&v["id"])));
        l.push(format!(
            "HYDO_INV_{k}_type={}",
            shq(&v.get("type").map(s).unwrap_or_else(|| "always".into()))
        ));
        l.push(format!(
            "HYDO_INV_{k}_msg={}",
            shqv(v.get("msg").unwrap_or(&v["id"]))
        ));
        l.push(format!("HYDO_INV_{k}_cmd={}", shqv(&v["cmd"])));
        l.push(format!(
            "HYDO_INV_{k}_every={}",
            shq(&every.filter(|e| py::truthy(e)).map(s).unwrap_or_default())
        ));
        l.push(format!("HYDO_INV_{k}_final={}", if fin { 1 } else { 0 }));
        if v.get("type").and_then(Value::as_str) == Some("eventually") {
            l.pop();
            l.push(format!("HYDO_INV_{k}_final=0"));
            l.push(format!(
                "HYDO_INV_{k}_within={}",
                pct_d(&v["within"], "invariants.within")?
            ));
            l.push(format!(
                "HYDO_INV_{k}_after={}",
                shq(&v.get("after").map(s).unwrap_or_else(|| "heal".into()))
            ));
            l.push(format!(
                "HYDO_INV_{k}_poll={}",
                pct_d(v.get("poll").unwrap_or(&Value::from(1)), "invariants.poll")?
            ));
        }
    }
    l.push(format!("HYDO_INVARIANTS={}", shq(&inv_names.join(" "))));
    if has_checks(m) {
        compile_checks(m, &mut l);
    }
    for k in arr(m, "knobs") {
        let t = s(&k["token"]);
        l.push(format!("HYDO_KNOB_{t}_default={}", shqv(&k["default"])));
        l.push(format!("HYDO_KNOB_{t}_set={}", shqv(&k["set"])));
        l.push(format!(
            "HYDO_KNOB_{t}_name={}",
            shq(&k.get("name").map(s).unwrap_or_else(|| t.clone()))
        ));
    }
    if let Some(c) = m.get("_compose").filter(|c| py::truthy(c)) {
        composer_lines(c, &mut l)?;
    }
    if has_errlog(m) {
        compile_errors(m, order, &mut l)?;
    }
    let manifest_sh = l.join("\n") + "\n";

    let mut p = vec![
        "# generated by hvx/onboard/hydo-build: the project's env + shell helpers (hydo.yaml)"
            .to_string(),
        "export HOME=/tmp TZ=UTC LANG=C".to_string(),
    ];
    match m.get("env") {
        None => {}
        Some(e) if !py::truthy(e) => {}
        Some(Value::Object(o)) => {
            for (k, v) in o {
                if !crate::fullmatch_ident(k) {
                    return Err(Fail::Manifest(format!(
                        "env: {} is not a valid variable name",
                        py::repr_str(k)
                    )));
                }
                p.push(format!("export {}={}", k, shqv(v)));
            }
        }
        Some(other) => {
            return Err(Fail::Crash(format!(
                "env: a {} where a mapping of names to values was expected",
                py::type_name(other)
            )))
        }
    }
    if let Some(sh) = m.get("shell").filter(|x| py::truthy(x)) {
        let sh = pyv::need_str(sh, "shell")?;
        p.push(sh.trim_end_matches('\n').to_string());
    }
    if has_checks(m) {
        let hist = ctx.onboard.join("..").join("history").join("hydo_hist.sh");
        let t = crate::read_text(&hist)?;
        p.push(t.trim_end_matches('\n').to_string());
    }
    let prelude = p.join("\n") + "\n";
    Ok((manifest_sh, prelude))
}
