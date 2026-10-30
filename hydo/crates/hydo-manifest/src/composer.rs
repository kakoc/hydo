//! The test composer's plan (hydo/onboard/hydo_composer.py): the `hydo/` folder (`scan_folder`,
//! `read_regression`), the plan's validation and expansion into the manifest (`expand`, with the
//! regression staging decisions of `regression_status`), `test_from_meta`, `check_version` and
//! the seed / resolved / saved forms of the composition token W= that expansion needs.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::py::{
    self, crash, eq, has, in_strs, is_int, json_dumps, repr, repr_str, str_of, truthy,
};
use crate::pyre;
use crate::validate::{err, iter_of, or_iter, R};
use crate::ManifestError;

pub const FORMAT: i128 = 1;
pub const WORKLOAD_KINDS: &[&str] = &[
    "first",
    "parallel_driver",
    "serial_driver",
    "singleton_driver",
    "anytime",
    "eventually",
    "finally",
];
pub const PLAN_KINDS: &[&str] = &[
    "parallel_driver",
    "serial_driver",
    "singleton_driver",
    "anytime",
];
pub const CHECK_KINDS: &[&str] = &["final", "always", "eventually"];
pub const HISTORY_KINDS: &[&str] = &["linearizable", "list_append"];
pub const HISTORY_KEYS: &[&str] = &["history", "model", "initial", "consistency"];
pub const NAME_RE: &str = r"[A-Za-z0-9_.\-]+";
/// (name, default, lo, hi) in hydo_composer's DEFAULTS order.
pub const PARAMS: &[(&str, i128, i128, i128)] = &[
    ("parallel", 4, 1, 16),
    ("per_round", 2, 1, 8),
    ("round_ms", 200, 10, 60000),
    ("command_timeout", 60, 1, 3600),
    ("drain_timeout", 60, 1, 3600),
    ("check_every", 2, 1, 3600),
    ("check_within", 30, 1, 3600),
    ("max_commands", 0, 0, 100000),
    ("budget_ms", 0, 0, 3600000),
];
const KNOWN_CMD: &[&str] = &["name", "kind", "run", "timeout", "description", "content"];
const KNOWN_CHECK: &[&str] = &[
    "name",
    "kind",
    "run",
    "msg",
    "every",
    "within",
    "after",
    "poll",
    "description",
    "history",
    "model",
    "initial",
    "consistency",
];
const KNOWN_REG: &[&str] = &[
    "name",
    "schedule",
    "expect",
    "commands",
    "description",
    "compose",
    "contents",
    "saved_schedule",
];
pub const DEFAULT_STEPS: i128 = 20;
pub const SH: &str = "/bin/busybox sh /etc/hydo/compose.sh";
pub const GUEST_MAX: usize = 500;

fn known_test() -> Vec<&'static str> {
    let mut v: Vec<&str> = PARAMS.iter().map(|p| p.0).collect();
    v.extend(["commands", "checks", "regressions", "description"]);
    v
}

fn s(x: &str) -> Value {
    Value::String(x.to_string())
}

fn n(x: i128) -> Value {
    if let Ok(i) = i64::try_from(x) {
        Value::Number(i.into())
    } else {
        Value::String(x.to_string())
    }
}

pub fn sha_hex(b: &[u8]) -> String {
    let d = Sha256::digest(b);
    d.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn match_name(x: &str) -> bool {
    pyre::match_dollar(NAME_RE, x).is_some()
}

fn sorted_keys_minus(o: &Map<String, Value>, known: &[&str]) -> Vec<String> {
    let mut bad: Vec<String> = o
        .keys()
        .filter(|k| !known.contains(&k.as_str()))
        .cloned()
        .collect();
    bad.sort();
    bad
}

// ------------------------------------------------------------------ the hydo/ folder

/// Python's `str.splitlines()`.
pub fn splitlines(t: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = t.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push(std::mem::take(&mut cur));
            }
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `_header(path)`: `# hydo: key=value ...` and `# hydo-msg: text` in the first 4 KiB.
fn header(path: &Path) -> (Vec<(String, Value)>, Option<String>) {
    use std::io::Read;
    let mut kv: Vec<(String, Value)> = Vec::new();
    let mut msg = None;
    let mut head = Vec::new();
    match std::fs::File::open(path) {
        Ok(f) => {
            if f.take(4096).read_to_end(&mut head).is_err() {
                return (kv, msg);
            }
        }
        Err(_) => return (kv, msg),
    }
    if head.contains(&0) {
        return (kv, msg);
    }
    for line in splitlines(&String::from_utf8_lossy(&head)) {
        if let Some(g) = pyre::match_dollar(r"\s*(?:#|//)\s*hydo-msg:\s*(.*?)\s*", &line) {
            msg = g[0].clone();
            continue;
        }
        if let Some(g) = pyre::match_start(r"\s*(?:#|//)\s*hydo:\s*(.*)$", &line) {
            for tok in py::split_ws(g[0].as_deref().unwrap_or("")) {
                if let Some((k, v)) = tok.split_once('=') {
                    let val = if pyre::fullmatch(r"\d+", v) && v.chars().all(|c| c.is_ascii_digit())
                    {
                        v.parse::<i64>()
                            .map(|i| Value::Number(i.into()))
                            .unwrap_or_else(|_| s(v))
                    } else {
                        s(v)
                    };
                    match kv.iter_mut().find(|(a, _)| a == k) {
                        Some(e) => e.1 = val,
                        None => kv.push((k.to_string(), val)),
                    }
                }
            }
        }
    }
    (kv, msg)
}

fn split_kind<'a>(fname: &str, kinds: &[&'a str], wh: &str) -> R<(&'a str, String)> {
    let mut ks: Vec<&str> = kinds.to_vec();
    ks.sort_by_key(|k| std::cmp::Reverse(k.len()));
    for k in ks {
        if let Some(name) = fname.strip_prefix(&format!("{k}_")) {
            if !match_name(name) {
                return Err(err(format!(
                    "{wh}/{fname}: the name after {k}_ may use letters, digits, '.', '_', '-'"
                )));
            }
            return Ok((k, name.to_string()));
        }
    }
    Err(err(format!(
        "{}/{}: unknown prefix (a command is <kind>_<name> with kind one of {}; helper_* files and dot files are ignored)",
        wh,
        fname,
        kinds.join(", ")
    )))
}

fn sorted_listdir(d: &Path) -> R<Vec<String>> {
    let rd = std::fs::read_dir(d).map_err(|e| crash(format!("listdir {}: {e}", d.display())))?;
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

fn executable(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: access(2) reads a NUL-terminated path and writes nothing.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// `scan_folder(mdir)`: the hydo/ folder next to a manifest, or None when it has none of
/// workload/, checks/, regressions/.
pub fn scan_folder(mdir: &str) -> R<Option<Value>> {
    let mut dirs = Map::new();
    for d in ["workload", "checks", "regressions"] {
        let p = format!("{mdir}/{d}");
        if Path::new(&p).is_dir() {
            dirs.insert(d.to_string(), s(&p));
        }
    }
    if dirs.is_empty() {
        return Ok(None);
    }
    let mut out = Map::new();
    let mut lists: Vec<(String, Vec<Value>)> = vec![
        ("commands".into(), Vec::new()),
        ("checks".into(), Vec::new()),
        ("regressions".into(), Vec::new()),
    ];
    let check_kinds: Vec<&str> = CHECK_KINDS.iter().chain(HISTORY_KINDS).copied().collect();
    for (sub, kinds, key) in [
        ("workload", WORKLOAD_KINDS, "commands"),
        ("checks", &check_kinds[..], "checks"),
    ] {
        let Some(dp) = dirs.get(sub).and_then(|v| v.as_str()) else {
            continue;
        };
        for fname in sorted_listdir(Path::new(dp))? {
            let p = format!("{dp}/{fname}");
            if fname.starts_with('.') || fname.starts_with("helper_") {
                continue;
            }
            if Path::new(&p).is_dir() {
                return Err(err(format!(
                    "{sub}/{fname}: a directory; only helper_* directories may sit beside commands"
                )));
            }
            let (k, name) = split_kind(&fname, kinds, sub)?;
            if !executable(Path::new(&p)) {
                return Err(err(format!(
                    "{sub}/{fname}: not executable (chmod +x; a script needs a #! line)"
                )));
            }
            let (kv, msg) = header(Path::new(&p));
            let mut e = Map::new();
            e.insert("name".into(), s(&name));
            e.insert("kind".into(), s(k));
            e.insert(
                "run".into(),
                Value::Array(vec![s(&format!("/hydo/{sub}/{fname}"))]),
            );
            if key == "commands" {
                let bytes = std::fs::read(&p).map_err(|x| crash(format!("{p}: {x}")))?;
                e.insert("content".into(), s(&sha_hex(&bytes)[..16]));
            }
            let allowed: &[&str] = if key == "commands" {
                &["timeout"]
            } else if HISTORY_KINDS.contains(&k) {
                HISTORY_KEYS
            } else {
                &["every", "within", "after", "poll"]
            };
            for (a, v) in &kv {
                if !allowed.contains(&a.as_str()) {
                    return Err(err(format!(
                        "{}/{}: header key {} (allowed here: {})",
                        sub,
                        fname,
                        repr_str(a),
                        allowed.join(", ")
                    )));
                }
                e.insert(a.clone(), v.clone());
            }
            if let Some(m) = msg.filter(|m| !m.is_empty()) {
                if key == "checks" {
                    e.insert("msg".into(), s(&m));
                }
            }
            if let Some(l) = lists.iter_mut().find(|(k, _)| k == key) {
                l.1.push(Value::Object(e));
            }
        }
    }
    if let Some(rp) = dirs.get("regressions").and_then(|v| v.as_str()) {
        for fname in sorted_listdir(Path::new(rp))? {
            if !fname.ends_with(".schedule") {
                continue;
            }
            let r = read_regression(&format!("{rp}/{fname}"))?;
            lists[2].1.push(r);
        }
    }
    for (k, v) in lists {
        out.insert(k, Value::Array(v));
    }
    out.insert("_dirs".into(), Value::Object(dirs));
    Ok(Some(Value::Object(out)))
}

/// `read_regression(path)`.
pub fn read_regression(path: &str) -> R<Value> {
    let bytes = std::fs::read(path).map_err(|e| crash(format!("{path}: {e}")))?;
    let text =
        String::from_utf8(bytes).map_err(|_| crash(format!("{path}: UnicodeDecodeError")))?;
    // universal newlines: \r\n and \r end a line, as Python's text mode reads them
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut sched: Option<String> = None;
    let mut expect = "ok".to_string();
    let mut cmds: Option<Vec<String>> = None;
    let mut compose: Option<String> = None;
    let mut contents: Vec<(String, String)> = Vec::new();
    for line in text.split_inclusive('\n') {
        let st = py::strip(line);
        if st.is_empty() {
            continue;
        }
        if st.starts_with('#') {
            if let Some(g) = pyre::match_start(r"#\s*expect:\s*(\S+)", st) {
                expect = g[0].clone().unwrap_or_default();
            }
            if let Some(g) = pyre::match_dollar(r"#\s*commands:\s*(.*)", st) {
                cmds = Some(
                    py::split_ws(g[0].as_deref().unwrap_or(""))
                        .iter()
                        .map(|x| x.to_string())
                        .collect(),
                );
            }
            if let Some(g) = pyre::match_dollar(r"#\s*compose:\s*(\S+)\s*", st) {
                compose = g[0].clone();
            }
            if let Some(g) = pyre::match_dollar(r"#\s*command-sha:\s*(\S+)\s+(\S+)\s*", st) {
                let k = g[0].clone().unwrap_or_default();
                let v = g[1].clone().unwrap_or_default();
                match contents.iter_mut().find(|(a, _)| *a == k) {
                    Some(e) => e.1 = v,
                    None => contents.push((k, v)),
                }
            }
            continue;
        }
        if sched.is_some() {
            return Err(err(format!("{path}: more than one schedule line")));
        }
        sched = Some(st.to_string());
    }
    let base = py::basename(path);
    let name = base.strip_suffix(".schedule").unwrap_or(&base).to_string();
    let mut r = Map::new();
    r.insert("name".into(), s(&name));
    r.insert(
        "schedule".into(),
        sched.map(Value::String).unwrap_or(Value::Null),
    );
    r.insert("expect".into(), s(&expect));
    if let Some(c) = cmds {
        r.insert(
            "commands".into(),
            Value::Array(c.into_iter().map(Value::String).collect()),
        );
    }
    if let Some(c) = compose {
        r.insert("compose".into(), s(&c));
    }
    if !contents.is_empty() {
        let mut m = Map::new();
        for (k, v) in contents {
            m.insert(k, Value::String(v));
        }
        r.insert("contents".into(), Value::Object(m));
    }
    let r = Value::Object(r);
    check_regression(&r, path)?;
    Ok(r)
}

fn check_regression(r: &Value, wh: &str) -> R<()> {
    let sched = r.get("schedule").cloned().unwrap_or(Value::Null);
    let bad = if !truthy(&sched) {
        true
    } else {
        match &sched {
            Value::String(st) => match st.split_once('|') {
                None => true,
                Some((_, g)) => py::split_ws(g).first() != Some(&"run"),
            },
            Value::Array(a) => {
                if a.iter().any(|x| x == &s("|")) {
                    return Err(crash("a list schedule cannot be split"));
                }
                true
            }
            Value::Object(o) => {
                if o.contains_key("|") {
                    return Err(crash("a mapping schedule cannot be split"));
                }
                true
            }
            _ => return Err(crash("argument of type is not iterable")),
        }
    };
    if bad {
        return Err(err(format!(
            "{}: the schedule must look like '<host tokens> | run <guest tokens>' (got {})",
            wh,
            repr(&sched)
        )));
    }
    let ex = str_of(r.get("expect").unwrap_or(&s("ok")));
    if ex != "ok" && ex != "violation" && !pyre::fullmatch("fp=[0-9a-f]{16}", &ex) {
        return Err(err(format!(
            "{}: expect is ok, violation or fp=<16 hex digits> (got {})",
            wh,
            repr_str(&ex)
        )));
    }
    Ok(())
}

// ------------------------------------------------------------------ validation + expansion

fn argv(run: Option<&Value>, wh: &str) -> R<Value> {
    match run {
        Some(Value::String(st)) if !py::strip(st).is_empty() => Ok(Value::String(st.clone())),
        Some(Value::Array(a)) if !a.is_empty() && a.iter().all(|x| x.is_string() || is_int(x)) => {
            let v: Vec<String> = a.iter().map(str_of).collect();
            if v.iter().any(|x| x.contains('\n') || x.contains('\0')) {
                return Err(err(format!(
                    "{wh}.run: an argument contains a newline or NUL"
                )));
            }
            Ok(Value::Array(v.into_iter().map(Value::String).collect()))
        }
        _ => Err(err(format!(
            "{wh}.run: an argv list of strings (exec'd, no shell) or a shell command string"
        ))),
    }
}

pub fn shq(x: &str) -> String {
    format!("'{}'", x.replace('\'', "'\\''"))
}

/// `shell_of(run)`: argv quoted and exec'd, or the shell string as it is.
pub fn shell_of(run: &Value) -> String {
    match run {
        Value::Array(a) => format!(
            "exec {}",
            a.iter()
                .map(|x| shq(&str_of(x)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        other => str_of(other),
    }
}

fn history_check(c: &Map<String, Value>, wh: &str) -> R<Value> {
    let kind_v = c.get("kind").cloned().unwrap_or(Value::Null);
    let kind = if in_strs(&kind_v, &["list_append", "list-append"]) {
        "list-append"
    } else {
        "linearizable"
    };
    if !c.get("history").is_some_and(truthy) {
        return Err(err(format!(
            "{}: a {} check needs history: (the name the workload records under)",
            wh,
            str_of(&kind_v)
        )));
    }
    for k in ["every", "within", "after", "poll"] {
        if c.contains_key(k) {
            return Err(err(format!(
                "{wh}: {k}: is not for a history check (it is evaluated once, on the host)"
            )));
        }
    }
    let mut e = Map::new();
    e.insert("id".into(), s(&str_of(&c["name"])));
    e.insert("type".into(), s(kind));
    e.insert("history".into(), s(&str_of(&c["history"])));
    let msg = match c.get("msg") {
        Some(m) if truthy(m) => str_of(m),
        _ => str_of(&c["name"]),
    };
    e.insert("msg".into(), s(&msg));
    for k in ["model", "initial", "consistency"] {
        if let Some(v) = c.get(k) {
            e.insert(k.into(), s(&str_of(v)));
        }
    }
    if let Some(run) = c.get("run").filter(|r| !r.is_null()) {
        e.insert("final".into(), s(&shell_of(&argv(Some(run), wh)?)));
    }
    Ok(Value::Object(e))
}

fn pair_hash_eq(a: &(Value, Value), b: &(Value, Value)) -> bool {
    eq(&a.0, &b.0) && eq(&a.1, &b.1)
}

fn merge_folder(m: &mut Map<String, Value>, folder: &Value) -> R<()> {
    if m.get("test").is_none_or(|t| t.is_null()) {
        m.insert("test".into(), Value::Object(Map::new()));
    }
    let Some(Value::Object(t)) = m.get_mut("test") else {
        return Err(err("test: must be a mapping"));
    };
    for key in ["commands", "checks", "regressions"] {
        let existing = or_iter(t.get(key))?;
        let mut have: Vec<(Value, Value)> = Vec::new();
        for e in &existing {
            if let Value::Object(o) = e {
                let name = o.get("name").cloned().unwrap_or(Value::Null);
                if matches!(name, Value::Array(_) | Value::Object(_)) {
                    return Err(crash("unhashable type in a set of (kind, name)"));
                }
                have.push((o.get("kind").cloned().unwrap_or(Value::Null), name));
            }
        }
        let fl = folder[key].as_array().cloned().unwrap_or_default();
        for e in &fl {
            let k = e.get("kind").cloned().unwrap_or(Value::Null);
            let pair = (k.clone(), e["name"].clone());
            if have.iter().any(|h| pair_hash_eq(h, &pair)) {
                return Err(err(format!(
                    "test.{}: {} {} is defined both in hydo.yaml and in the hydo/ folder",
                    key,
                    str_of(e.get("kind").unwrap_or(&s(""))),
                    repr(&e["name"])
                )));
            }
        }
        if !fl.is_empty() {
            let mut all = existing;
            all.extend(fl);
            t.insert(key.to_string(), Value::Array(all));
        }
    }
    Ok(())
}

/// The plan meta `expand` returns (staged as /etc/hydo/compose.json), with the warnings it gave.
pub fn expand(m: &mut Value, folder: Option<&Value>, warn: &mut Vec<String>) -> R<Option<Value>> {
    let mo = m
        .as_object_mut()
        .ok_or_else(|| err("the manifest must be a mapping"))?;
    if let Some(f) = folder.filter(|f| truthy(f)) {
        merge_folder(mo, f)?;
    }
    if let Some(fv) = mo.get("format") {
        if !eq(fv, &n(FORMAT)) {
            return Err(err(format!(
                "format: this hydo reads plan format {} (got {}); update hydo or regenerate the plan",
                FORMAT,
                repr(fv)
            )));
        }
    }
    let t = match mo.get("test") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(t)) => t.clone(),
        Some(_) => return Err(err("test: must be a mapping")),
    };
    let kt = known_test();
    let bad = sorted_keys_minus(&t, &kt);
    if !bad.is_empty() {
        let mut k2 = kt.clone();
        k2.sort();
        return Err(err(format!(
            "test: unknown field(s) {} (known: {})",
            bad.join(", "),
            k2.join(", ")
        )));
    }
    let mut par = Map::new();
    for (k, dflt, lo, hi) in PARAMS {
        let v = match t.get(*k) {
            Some(v) => {
                if !(py::is_plain_int(v) && py::int_of(v).is_some_and(|i| *lo <= i && i <= *hi)) {
                    return Err(err(format!(
                        "test.{}: a whole number in [{}, {}] (got {})",
                        k,
                        lo,
                        hi,
                        repr(v)
                    )));
                }
                v.clone()
            }
            None => n(*dflt),
        };
        par.insert(k.to_string(), v);
    }
    let mut cmds: Vec<Map<String, Value>> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (i, c) in or_iter(t.get("commands"))?.iter().enumerate() {
        let wh = format!("test.commands[{i}]");
        let Value::Object(c) = c else {
            return Err(err(format!("{wh}: must be a mapping")));
        };
        let bad = sorted_keys_minus(c, KNOWN_CMD);
        if !bad.is_empty() {
            return Err(err(format!("{}: unknown field(s) {}", wh, bad.join(", "))));
        }
        let name = str_of(c.get("name").unwrap_or(&s("")));
        let kind = c.get("kind").cloned().unwrap_or(Value::Null);
        if !match_name(&name) {
            return Err(err(format!(
                "{}: name {}: letters, digits, '.', '_', '-'",
                wh,
                repr_str(&name)
            )));
        }
        if !in_strs(&kind, WORKLOAD_KINDS) {
            return Err(err(format!(
                "{}: kind must be one of {} (got {})",
                wh,
                WORKLOAD_KINDS.join(", "),
                repr(&kind)
            )));
        }
        let kind = str_of(&kind);
        let cid = format!("{kind}_{name}");
        if seen.contains(&cid) {
            return Err(err(format!("{wh}: duplicate command {cid}")));
        }
        seen.push(cid.clone());
        let to = c
            .get("timeout")
            .cloned()
            .unwrap_or_else(|| par["command_timeout"].clone());
        if !(py::is_plain_int(&to) && py::int_of(&to).is_some_and(|i| (1..=3600).contains(&i))) {
            return Err(err(format!("{wh}: timeout is whole seconds in [1, 3600]")));
        }
        let run = argv(c.get("run"), &wh)?;
        let content = match c.get("content") {
            None | Some(Value::Null) => {
                let j = json_dumps(&Value::Array(vec![run.clone(), to.clone()]));
                s(&sha_hex(j.as_bytes())[..16])
            }
            Some(Value::String(x)) if pyre::match_dollar("[0-9a-f]{8,64}", x).is_some() => s(x),
            Some(_) => {
                return Err(err(format!(
                    "{wh}: content is 8 to 64 lowercase hex digits (a hash of the command's code)"
                )))
            }
        };
        let mut e = Map::new();
        e.insert("id".into(), s(&cid));
        e.insert("kind".into(), s(&kind));
        e.insert("name".into(), s(&name));
        e.insert("run".into(), run);
        e.insert("timeout".into(), to);
        e.insert("content".into(), content);
        cmds.push(e);
    }
    cmds.sort_by(|a, b| str_of(&a["id"]).cmp(&str_of(&b["id"])));
    let mut checks: Vec<Map<String, Value>> = Vec::new();
    let mut cseen: Vec<String> = Vec::new();
    let mut hist: Vec<Value> = Vec::new();
    for (i, c) in or_iter(t.get("checks"))?.iter().enumerate() {
        let wh = format!("test.checks[{i}]");
        let Value::Object(c) = c else {
            return Err(err(format!("{wh}: must be a mapping")));
        };
        let bad = sorted_keys_minus(c, KNOWN_CHECK);
        if !bad.is_empty() {
            return Err(err(format!("{}: unknown field(s) {}", wh, bad.join(", "))));
        }
        let name = str_of(c.get("name").unwrap_or(&s("")));
        let kind = c.get("kind").cloned().unwrap_or(Value::Null);
        if !match_name(&name) {
            return Err(err(format!(
                "{}: name {}: letters, digits, '.', '_', '-'",
                wh,
                repr_str(&name)
            )));
        }
        if in_strs(&kind, &["linearizable", "list_append", "list-append"]) {
            if cseen.contains(&name) {
                return Err(err(format!(
                    "{}: duplicate check name {} (the name is the property id)",
                    wh,
                    repr_str(&name)
                )));
            }
            cseen.push(name.clone());
            hist.push(history_check(c, &wh)?);
            continue;
        }
        if HISTORY_KEYS.iter().any(|k| c.contains_key(*k)) {
            return Err(err(format!(
                "{}: {}: are for kind linearizable or list_append",
                wh,
                HISTORY_KEYS.join("/")
            )));
        }
        if !in_strs(&kind, CHECK_KINDS) {
            return Err(err(format!(
                "{}: kind must be one of {} (got {})",
                wh,
                CHECK_KINDS.join(", "),
                repr(&kind)
            )));
        }
        let kind = str_of(&kind);
        if cseen.contains(&name) {
            return Err(err(format!(
                "{}: duplicate check name {} (the name is the property id)",
                wh,
                repr_str(&name)
            )));
        }
        cseen.push(name.clone());
        let mut e = Map::new();
        e.insert("name".into(), s(&name));
        e.insert("kind".into(), s(&kind));
        e.insert("run".into(), argv(c.get("run"), &wh)?);
        let msg = match c.get("msg") {
            Some(m) if truthy(m) => str_of(m),
            _ => name.clone(),
        };
        e.insert("msg".into(), s(&msg));
        for k in ["every", "within", "poll"] {
            if let Some(v) = c.get(k) {
                if !(py::is_plain_int(v) && py::int_of(v).is_some_and(|i| (1..=3600).contains(&i)))
                {
                    return Err(err(format!("{wh}: {k} is whole seconds in [1, 3600]")));
                }
                e.insert(k.into(), v.clone());
            }
        }
        if let Some(a) = c.get("after") {
            e.insert("after".into(), a.clone());
        }
        if kind != "eventually"
            && ["within", "poll", "after"]
                .iter()
                .any(|k| e.contains_key(*k))
        {
            return Err(err(format!(
                "{wh}: within/poll/after are for kind eventually"
            )));
        }
        if kind != "always" && e.contains_key("every") {
            return Err(err(format!("{wh}: every is for kind always")));
        }
        checks.push(e);
    }
    checks.sort_by(|a, b| str_of(&a["name"]).cmp(&str_of(&b["name"])));
    if !hist.is_empty() {
        let cur = mo.get("checks").cloned();
        if let Some(c) = &cur {
            if !c.is_null() && !c.is_array() {
                return Err(err("checks: must be a list"));
            }
        }
        let mut all = or_iter(cur.as_ref())?;
        hist.sort_by(|a, b| str_of(&a["id"]).cmp(&str_of(&b["id"])));
        all.extend(hist);
        mo.insert("checks".into(), Value::Array(all));
    }
    let mut regs: Vec<Map<String, Value>> = Vec::new();
    for (i, r) in or_iter(t.get("regressions"))?.iter().enumerate() {
        let wh = format!("test.regressions[{i}]");
        let ok = matches!(r, Value::Object(o) if sorted_keys_minus(o, KNOWN_REG).is_empty());
        if !ok {
            return Err(err(format!("{wh}: a mapping with name, schedule, expect")));
        }
        check_regression(r, &wh)?;
        let ro = r.as_object().cloned().unwrap_or_default();
        let mut e = Map::new();
        let name = match ro.get("name") {
            Some(x) if truthy(x) => str_of(x),
            _ => format!("r{i}"),
        };
        e.insert("name".into(), s(&name));
        e.insert("schedule".into(), ro["schedule"].clone());
        e.insert(
            "expect".into(),
            s(&str_of(ro.get("expect").unwrap_or(&s("ok")))),
        );
        if let Some(c) = ro.get("commands").filter(|c| !c.is_null()) {
            e.insert(
                "commands".into(),
                Value::Array(iter_of(c)?.iter().map(|x| s(&str_of(x))).collect()),
            );
        }
        for k in ["compose", "saved_schedule"] {
            if let Some(v) = ro.get(k).filter(|v| !v.is_null()) {
                e.insert(k.into(), s(&str_of(v)));
            }
        }
        if let Some(c) = ro.get("contents").filter(|c| !c.is_null()) {
            let Value::Object(co) = c else {
                return Err(err(format!(
                    "{wh}: contents is a mapping command id -> content hash"
                )));
            };
            let mut items: Vec<(String, String)> =
                co.iter().map(|(k, v)| (k.clone(), str_of(v))).collect();
            items.sort();
            let mut cm = Map::new();
            for (k, v) in items {
                cm.insert(k, Value::String(v));
            }
            e.insert("contents".into(), Value::Object(cm));
        }
        regs.push(e);
    }
    let to_arr =
        |v: &Vec<Map<String, Value>>| Value::Array(v.iter().cloned().map(Value::Object).collect());
    if cmds.is_empty() && checks.is_empty() {
        if !regs.is_empty() {
            return Ok(Some(py::obj(vec![
                ("params", Value::Object(par)),
                ("commands", Value::Array(Vec::new())),
                ("checks", Value::Array(Vec::new())),
                ("regressions", to_arr(&regs)),
                ("composed", Value::Bool(false)),
            ])));
        }
        return Ok(None);
    }
    let planned: Vec<&Map<String, Value>> = cmds
        .iter()
        .filter(|c| in_strs(&c["kind"], PLAN_KINDS))
        .collect();
    let has_planned = !planned.is_empty();
    if !cmds.is_empty() && !has_planned {
        return Err(err("test.commands: at least one parallel_driver, serial_driver, singleton_driver or anytime command is needed (the composer has nothing to run otherwise)"));
    }
    for k in or_iter(mo.get("knobs"))? {
        if let Value::Object(ko) = &k {
            if ko.get("token") == Some(&s("W")) {
                return Err(err(
                    "knobs: token W is the test composer's schedule token in a plan with test commands",
                ));
            }
        }
    }
    if has_planned {
        if mo.get("workload").is_none_or(|w| w.is_null()) {
            mo.insert("workload".into(), Value::Object(Map::new()));
        }
        let Some(Value::Object(wl)) = mo.get_mut("workload") else {
            return Err(err("workload: must be a mapping"));
        };
        if wl.contains_key("run") || wl.contains_key("process") {
            return Err(err("workload.run/process: the test composer runs the workload (test.commands); remove them, or drop the test commands"));
        }
        if !wl.contains_key("steps") {
            wl.insert("steps".into(), n(DEFAULT_STEPS));
        }
        wl.insert("run".into(), s(&format!("{SH} step $HYDO_FROM $HYDO_TO")));
        wl.insert("process".into(), s("compose.sh run1"));
    }
    if cmds.iter().any(|c| c["kind"] == "first") {
        let prov = mo.get("provision").cloned().unwrap_or(Value::Null);
        let v = if truthy(&prov) {
            format!("( {}\n) && {} first", str_of(&prov), SH)
        } else {
            format!("{SH} first")
        };
        mo.insert("provision".into(), s(&v));
    }
    let mut invs: Vec<Value> = Vec::new();
    if has_planned {
        invs.push(py::obj(vec![
            ("id", s("compose.commands")),
            ("type", s("always")),
            ("cmd", s(&format!("{SH} verdict"))),
            (
                "msg",
                s("no composed command reported a violation (exit status 100)"),
            ),
        ]));
    }
    for (i, c) in cmds.iter().enumerate() {
        let k = str_of(&c["kind"]);
        if k == "eventually" || k == "finally" {
            let id = str_of(&c["id"]);
            let msg = if k == "eventually" {
                format!("after the faults stop, {id} succeeds")
            } else {
                format!(
                    "at the end, {id} succeeds (skipped unless every started command completed)"
                )
            };
            invs.push(py::obj(vec![
                ("id", s(&id)),
                ("type", s("always")),
                ("cmd", s(&format!("{SH} end {i}"))),
                ("msg", s(&msg)),
            ]));
        }
    }
    for (i, c) in checks.iter().enumerate() {
        if c["kind"] == "eventually" {
            invs.push(py::obj(vec![
                ("id", c["name"].clone()),
                ("type", s("eventually")),
                ("cmd", s(&shell_of(&c["run"]))),
                ("msg", c["msg"].clone()),
                (
                    "within",
                    c.get("within")
                        .cloned()
                        .unwrap_or_else(|| par["check_within"].clone()),
                ),
                (
                    "after",
                    c.get("after").cloned().unwrap_or_else(|| s("heal")),
                ),
                ("poll", c.get("poll").cloned().unwrap_or_else(|| n(1))),
            ]));
        } else {
            let mut v = vec![
                ("id", c["name"].clone()),
                ("type", s("always")),
                ("cmd", s(&format!("{SH} check {i}"))),
                ("msg", c["msg"].clone()),
            ];
            if c["kind"] == "always" {
                v.push((
                    "every",
                    c.get("every")
                        .cloned()
                        .unwrap_or_else(|| par["check_every"].clone()),
                ));
                v.push(("final", Value::Bool(true)));
            }
            invs.push(py::obj(v));
        }
    }
    let ids: Vec<String> = invs.iter().map(|v| str_of(&v["id"])).collect();
    for v in or_iter(mo.get("invariants"))? {
        if let Value::Object(vo) = &v {
            match vo.get("id") {
                Some(Value::Array(_)) | Some(Value::Object(_)) => {
                    return Err(crash("unhashable id"))
                }
                Some(Value::String(x)) if ids.contains(x) => {
                    return Err(err(format!(
                        "invariants: id {} is also a test command or check",
                        repr_str(x)
                    )))
                }
                _ => {}
            }
        }
    }
    let mut all_invs = invs;
    all_invs.extend(or_iter(mo.get("invariants"))?);
    mo.insert("invariants".into(), Value::Array(all_invs));
    let steps_v = match mo.get("workload").filter(|w| truthy(w)) {
        None => Value::Null,
        Some(Value::Object(w)) => w.get("steps").cloned().unwrap_or(Value::Null),
        Some(_) => return Err(crash("workload has no attribute 'get'")),
    };
    let steps = if truthy(&steps_v) {
        py_int_value(&steps_v)?
    } else {
        0
    };
    let mut meta = py::obj(vec![
        ("format", n(FORMAT)),
        ("params", Value::Object(par.clone())),
        ("commands", to_arr(&cmds)),
        ("checks", to_arr(&checks)),
        ("regressions", Value::Array(Vec::new())),
        ("composed", Value::Bool(has_planned)),
        ("steps", n(steps)),
        ("stable_compose", n(1)),
    ]);
    for r in regs.iter_mut() {
        let st = regression_status(&Value::Object(r.clone()), &meta, None)?;
        if Value::String(st.schedule.clone()) != r["schedule"] {
            if !r.contains_key("saved_schedule") {
                r.insert("saved_schedule".into(), r["schedule"].clone());
            }
            r.insert("schedule".into(), s(&st.schedule));
            if let Some(c) = &st.compose {
                r.insert("compose".into(), s(c));
            }
        }
        let name = str_of(&r["name"]);
        for w in st.error.iter().chain(st.warnings.iter()) {
            warn.push(format!("regression {name}: {w}"));
        }
    }
    meta["regressions"] = to_arr(&regs);
    Ok(Some(meta))
}

fn py_int_value(v: &Value) -> R<i128> {
    match v {
        Value::Bool(_) | Value::Number(_) if py::int_of(v).is_some() => {
            Ok(py::int_of(v).unwrap_or(0))
        }
        Value::Number(nn) => Ok(nn.as_f64().unwrap_or(0.0).trunc() as i128),
        Value::String(st) => py_int(st, 10).map_err(crash),
        _ => Err(crash(format!(
            "int() argument must be a string or a number, not {}",
            repr_str(py::type_name(v))
        ))),
    }
}

/// `test_from_meta(meta)`: the test: section that expands to meta again.
pub fn test_from_meta(meta: &Value) -> Value {
    let mut t = meta
        .get("params")
        .and_then(|p| p.as_object())
        .cloned()
        .unwrap_or_default();
    let cmds: Vec<Value> = meta
        .get("commands")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| {
            let mut e = Map::new();
            for k in ["name", "kind", "run", "timeout"] {
                e.insert(k.into(), c.get(k).cloned().unwrap_or(Value::Null));
            }
            if let Some(ct) = c.get("content").filter(|x| truthy(x)) {
                e.insert("content".into(), ct.clone());
            }
            Value::Object(e)
        })
        .collect();
    let checks: Vec<Value> = meta
        .get("checks")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| {
            let mut e = Map::new();
            for k in [
                "name", "kind", "run", "msg", "every", "within", "after", "poll",
            ] {
                if let Some(v) = c.get(k) {
                    e.insert(k.into(), v.clone());
                }
            }
            Value::Object(e)
        })
        .collect();
    t.insert("commands".into(), Value::Array(cmds));
    t.insert("checks".into(), Value::Array(checks));
    t.insert(
        "regressions".into(),
        meta.get("regressions")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
    );
    Value::Object(t)
}

// ------------------------------------------------------------------ hydo_version

/// `engine_version(here)` with `here` = the product's onboard/ directory.
pub fn engine_version(onboard_dir: &Path) -> String {
    for p in [
        onboard_dir.join("..").join("VERSION"),
        onboard_dir.join("..").join("..").join("VERSION"),
    ] {
        if p.is_file() {
            if let Ok(t) = std::fs::read_to_string(&p) {
                return py::strip(&t).to_string();
            }
        }
    }
    "0.0.0".into()
}

fn vt(x: &str) -> [i128; 3] {
    let ns: Vec<i128> = pyre::digit_runs(x)
        .iter()
        .take(3)
        .map(|d| d.parse().unwrap_or(0))
        .collect();
    [
        ns.first().copied().unwrap_or(0),
        ns.get(1).copied().unwrap_or(0),
        ns.get(2).copied().unwrap_or(0),
    ]
}

/// `check_version(want, have, ...)`.
pub fn check_version(want: &Value, have: &str, warn: &mut Vec<String>) -> R<()> {
    let w = py::strip(&str_of(want)).to_string();
    if !pyre::fullmatch(r"(>=)?\d+(\.\d+){0,2}", &w) {
        return Err(err(format!(
            "hydo_version: \"0.1\", \"0.1.0\" or \">=0.1\" (got {})",
            repr_str(&w)
        )));
    }
    let h = vt(have);
    let (ok, why) = if let Some(rest) = w.strip_prefix(">=") {
        (h >= vt(rest), format!("at least {rest}"))
    } else {
        let wv = vt(&w);
        let ok = h[..2] == wv[..2];
        if ok && pyre::digit_runs(&w).len() == 3 && h != wv {
            warn.push(format!(
                "hydo_version: the plan names {w}, this hydo is {have} (same major.minor: building)"
            ));
        }
        (ok, format!("{}.{}.x", wv[0], wv[1]))
    };
    if !ok {
        let msg = format!("hydo_version: the plan needs hydo {why}, this hydo is {have}");
        if std::env::var("HYDO_VERSION_CHECK").as_deref() == Ok("warn") {
            warn.push(format!("{msg} (HYDO_VERSION_CHECK=warn: building anyway)"));
        } else {
            return Err(err(format!(
                "{msg} (install that hydo, or set HYDO_VERSION_CHECK=warn)"
            )));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ W= (stable compositions, M98)

/// Python's `int(text, base)` for base 10 and 16 (whitespace, a sign, `0x`, single underscores).
pub fn py_int(text: &str, base: u32) -> Result<i128, String> {
    let bad = || {
        format!(
            "invalid literal for int() with base {}: {}",
            base,
            repr_str(text)
        )
    };
    let t = py::strip(text);
    let (neg, mut body) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let mut prefixed = false;
    if base == 16 {
        if let Some(r) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            body = r;
            prefixed = true;
        }
    }
    if prefixed {
        body = body.strip_prefix('_').unwrap_or(body);
    }
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return Err(bad());
    }
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    if !digits.chars().all(|c| c.is_digit(base)) {
        return Err(bad());
    }
    let v = i128::from_str_radix(&digits, base).map_err(|_| bad())?;
    Ok(if neg { -v } else { v })
}

pub fn py_mod(a: i128, m: i128) -> R<i128> {
    if m == 0 {
        return Err(crash("ZeroDivisionError: integer modulo by zero"));
    }
    let mut r = a % m;
    if r != 0 && ((r < 0) != (m < 0)) {
        r += m;
    }
    Ok(r)
}

pub const M31: i128 = 2147483647;

struct Minstd {
    s: i128,
}

impl Minstd {
    fn new(seed: i128) -> R<Minstd> {
        let mut m = Minstd {
            s: py_mod(seed, M31 - 1)? + 1,
        };
        for _ in 0..8 {
            m.next();
        }
        Ok(m)
    }
    fn next(&mut self) -> i128 {
        self.s = (self.s * 48271) % M31;
        self.s
    }
    fn below(&mut self, n: i128) -> R<i128> {
        let x = self.next();
        py_mod(x, n)
    }
}

pub type Entry = (i128, i128, usize, i128);

pub fn plan(seed: i128, q: i128, steps: i128, kinds: &[String]) -> R<Vec<Entry>> {
    let idx = |k: &str| -> Vec<usize> {
        kinds
            .iter()
            .enumerate()
            .filter(|(_, c)| *c == k)
            .map(|(i, _)| i)
            .collect()
    };
    let (p, se, sg, a) = (
        idx("parallel_driver"),
        idx("serial_driver"),
        idx("singleton_driver"),
        idx("anytime"),
    );
    let mut rng = Minstd::new(seed)?;
    let mut out: Vec<Entry> = Vec::new();
    let single = !sg.is_empty() && ((p.is_empty() && se.is_empty()) || rng.below(4)? == 0);
    let g = if single {
        sg[rng.below(sg.len() as i128)? as usize]
    } else {
        usize::MAX
    };
    let pick = |v: &Vec<usize>, i: i128| -> usize { v[i.rem_euclid(v.len() as i128) as usize] };
    let mut r = 0i128;
    while r < steps {
        if single {
            if r == 0 {
                let sd = rng.next();
                out.push((out.len() as i128, r, g, sd));
            }
        } else if !se.is_empty() && (p.is_empty() || rng.below(4)? == 0) {
            let c = pick(&se, rng.below(se.len() as i128)?);
            let sd = rng.next();
            out.push((out.len() as i128, r, c, sd));
        } else if !p.is_empty() {
            let k = 1 + rng.below(q)?;
            for _ in 0..k.max(0) {
                let c = pick(&p, rng.below(p.len() as i128)?);
                let sd = rng.next();
                out.push((out.len() as i128, r, c, sd));
            }
        }
        if !a.is_empty() && ((p.is_empty() && se.is_empty() && sg.is_empty()) || rng.below(3)? == 0)
        {
            let c = pick(&a, rng.below(a.len() as i128)?);
            let sd = rng.next();
            out.push((out.len() as i128, r, c, sd));
        }
        r += 1;
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OItem {
    I(i128),
    S(String),
}

/// A seed-form W= value (`parse_w`): x and o keep their first-seen order; Python keeps sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WSeed {
    pub seed: i128,
    pub p: i128,
    pub q: i128,
    pub r: i128,
    pub n: i128,
    pub t: i128,
    pub x: Vec<i128>,
    pub o: Vec<OItem>,
}

/// A ValueError: caught by regression_status, its text becomes the regression's error.
pub type VE = String;

pub fn ilist(st: &str) -> Result<Vec<i128>, VE> {
    let mut out: Vec<i128> = Vec::new();
    if st.is_empty() {
        return Ok(out);
    }
    for it in st.split('.') {
        if it.contains('-') {
            let parts: Vec<&str> = it.split('-').collect();
            if parts.len() != 2 {
                return Err(format!(
                    "too many values to unpack (expected 2, got {})",
                    parts.len()
                ));
            }
            let a = py_int(parts[0], 10)?;
            let b = py_int(parts[1], 10)?;
            let mut i = a;
            while i <= b {
                if !out.contains(&i) {
                    out.push(i);
                }
                i += 1;
            }
        } else if !it.is_empty() {
            let v = py_int(it, 10)?;
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    Ok(out)
}

pub fn parse_w(w: &str, params: &Value) -> Result<WSeed, VE> {
    let pi = |k: &str| py::int_of(&params[k]).unwrap_or(0);
    let mut d = WSeed {
        seed: 0,
        p: pi("parallel"),
        q: pi("per_round"),
        r: pi("round_ms"),
        n: pi("max_commands"),
        t: params.get("budget_ms").and_then(py::int_of).unwrap_or(0),
        x: Vec::new(),
        o: Vec::new(),
    };
    if w.is_empty() || w == "-" {
        return Ok(d);
    }
    let f: Vec<&str> = w.split(',').collect();
    d.seed = py_int(f[0], 16)? & 0xFFFF_FFFF;
    for x in &f[1..] {
        if x.is_empty() {
            continue;
        }
        let k = x.chars().next().unwrap_or(' ');
        let v = &x[k.len_utf8()..];
        match k {
            'p' => d.p = py_int(v, 10)?,
            'q' => d.q = py_int(v, 10)?,
            'r' => d.r = py_int(v, 10)?,
            'n' => d.n = py_int(v, 10)?,
            't' => d.t = py_int(v, 10)?,
            'o' if v.chars().any(|c| c.is_alphabetic()) && pyre::search("[A-Za-z]", v) => {
                d.o = v.split('+').map(|x| OItem::S(x.to_string())).collect()
            }
            'x' => d.x = ilist(v)?,
            'o' => d.o = ilist(v)?.into_iter().map(OItem::I).collect(),
            _ => return Err(format!("W={}: unknown field {}", w, repr_str(x))),
        }
    }
    Ok(d)
}

pub fn kept(d: &WSeed, steps: i128, ids: &[String], kinds: &[String]) -> R<Vec<Entry>> {
    let es = plan(d.seed, d.q, steps, kinds)?;
    let out: Vec<Entry> = es
        .into_iter()
        .filter(|e| {
            !d.x.contains(&e.0)
                && !d.o.iter().any(|o| match o {
                    OItem::I(i) => *i == e.2 as i128,
                    OItem::S(sx) => *sx == ids[e.2],
                })
        })
        .collect();
    Ok(if d.n != 0 {
        let k = if d.n < 0 {
            (out.len() as i128 + d.n).max(0) as usize
        } else {
            (d.n as usize).min(out.len())
        };
        out[..k].to_vec()
    } else {
        out
    })
}

/// A resolved composition (`parse_resolved`): entries are (entry, round, command id, seed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub p: i128,
    pub r: i128,
    pub t: i128,
    pub entries: Vec<(i128, i128, String, i128)>,
}

pub fn kind_of(cid: &str) -> Option<&'static str> {
    let mut ks: Vec<&'static str> = WORKLOAD_KINDS.to_vec();
    ks.sort_by_key(|k| std::cmp::Reverse(k.len()));
    ks.into_iter().find(|k| cid.starts_with(&format!("{k}_")))
}

fn head40(v: &str) -> String {
    v.chars().take(40).collect()
}

pub fn parse_resolved(v: &str) -> Result<Resolved, VE> {
    if !v.starts_with(':') {
        return Err(format!(
            "a resolved composition starts with ':' (got {})",
            repr_str(&v.chars().take(20).collect::<String>())
        ));
    }
    let mut d = Resolved {
        p: 0,
        r: 0,
        t: 0,
        entries: Vec::new(),
    };
    let (mut p_set, mut r_set) = (false, false);
    let mut ids: Option<Vec<String>> = None;
    for f in v[1..].split(',') {
        if pyre::fullmatch(r"[prt]\d+", f) {
            let val = py_int(&f[1..], 10)?;
            match &f[..1] {
                "p" => {
                    d.p = val;
                    p_set = true
                }
                "r" => {
                    d.r = val;
                    r_set = true
                }
                _ => d.t = val,
            }
        } else if f.starts_with('c') && ids.is_none() {
            let l: Vec<String> = f[1..].split('+').map(|x| x.to_string()).collect();
            if !l.iter().all(|x| match_name(x) && kind_of(x).is_some()) {
                return Err(format!(
                    "W={}: c lists command ids <kind>_<name>",
                    head40(v)
                ));
            }
            ids = Some(l);
        } else if pyre::fullmatch(r"\d+\.\d+\.\d+\.\d+", f) {
            let parts: Vec<i128> = f
                .split('.')
                .map(|x| py_int(x, 10))
                .collect::<Result<_, _>>()?;
            let k = parts[2];
            match &ids {
                Some(l) if k >= 0 && (k as usize) < l.len() => {
                    d.entries
                        .push((parts[0], parts[1], l[k as usize].clone(), parts[3]))
                }
                _ => {
                    return Err(format!(
                        "W={}: entry {} names command {} of a table of {}",
                        head40(v),
                        f,
                        k,
                        ids.as_ref().map(|l| l.len()).unwrap_or(0)
                    ))
                }
            }
        } else {
            return Err(format!("W={}: unknown field {}", head40(v), repr_str(f)));
        }
    }
    if !p_set || d.p == 0 || !r_set || d.r == 0 || d.entries.is_empty() {
        return Err(format!(
            "W={}: a resolved composition has p, r and at least one entry",
            head40(v)
        ));
    }
    let es = &d.entries;
    let mut nums: Vec<i128> = es.iter().map(|e| e.0).collect();
    nums.sort();
    nums.dedup();
    if nums.len() != es.len() || es.windows(2).any(|w| w[0].1 > w[1].1) {
        return Err(format!(
            "W={}: entry numbers are unique and rounds never go back",
            head40(v)
        ));
    }
    Ok(d)
}

pub fn render_resolved(d: &Resolved) -> String {
    let mut ids: Vec<String> = Vec::new();
    for e in &d.entries {
        if !ids.contains(&e.2) {
            ids.push(e.2.clone());
        }
    }
    let mut out = format!(":p{},r{}", d.p, d.r);
    if d.t != 0 {
        out.push_str(&format!(",t{}", d.t));
    }
    out.push_str(&format!(",c{}", ids.join("+")));
    for (e, r, cid, sd) in &d.entries {
        let k = ids.iter().position(|x| x == cid).unwrap_or(0);
        out.push_str(&format!(",{e}.{r}.{k}.{sd}"));
    }
    out
}

pub fn w_of(schedule: &str) -> Option<String> {
    let g = schedule.split_once('|').map(|x| x.1).unwrap_or(schedule);
    py::split_ws(g)
        .into_iter()
        .find_map(|t| t.strip_prefix("W=").map(|x| x.to_string()))
}

pub fn with_w(schedule: &str, w: &str) -> String {
    let (host, guest) = schedule.split_once('|').unwrap_or((schedule, ""));
    let mut toks: Vec<String> = py::split_ws(guest).iter().map(|x| x.to_string()).collect();
    let mut done = false;
    for t in toks.iter_mut() {
        if t.starts_with("W=") {
            *t = format!("W={w}");
            done = true;
        }
    }
    if !done {
        toks.push(format!("W={w}"));
    }
    format!("{}| {}", host, toks.join(" "))
}

pub fn fits(schedule: &str) -> bool {
    let g = schedule.split_once('|').map(|x| x.1).unwrap_or(schedule);
    py::strip(g).chars().count() <= GUEST_MAX
}

/// `key_of(resolved)`: the key of a saved composition (W=@<key>).
pub fn key_of(resolved: &str) -> String {
    sha_hex(resolved.as_bytes())[..12].to_string()
}

pub fn resolve_seed(
    w: &str,
    ids: &[String],
    kinds: &[String],
    steps: i128,
    params: &Value,
) -> R<Result<Resolved, VE>> {
    let d = match parse_w(w, params) {
        Ok(d) => d,
        Err(e) => return Ok(Err(e)),
    };
    let es = kept(&d, steps, ids, kinds)?;
    Ok(Ok(Resolved {
        p: d.p,
        r: d.r,
        t: d.t,
        entries: es
            .into_iter()
            .map(|(e, r, c, sd)| (e, r, ids[c].clone(), sd))
            .collect(),
    }))
}

pub fn resolved_problems(d: &Resolved, meta: &Value, steps: Option<i128>) -> Vec<String> {
    let msteps = meta.get("steps").and_then(py::int_of).unwrap_or(0);
    let steps = match steps {
        Some(x) if x != 0 => x,
        _ => {
            if msteps != 0 {
                msteps
            } else {
                DEFAULT_STEPS
            }
        }
    };
    let cmds = meta
        .get("commands")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let kinds: Vec<(String, String)> = cmds
        .iter()
        .map(|c| (str_of(&c["id"]), str_of(&c["kind"])))
        .collect();
    let mut errs = Vec::new();
    let mut missing: Vec<String> = d
        .entries
        .iter()
        .map(|e| e.2.clone())
        .filter(|c| !kinds.iter().any(|(k, _)| k == c))
        .collect();
    missing.sort();
    missing.dedup();
    if !missing.is_empty() {
        let mut now: Vec<String> = kinds.iter().map(|(k, _)| k.clone()).collect();
        now.sort();
        let now_s = if now.is_empty() {
            "-".to_string()
        } else {
            now.join(" ")
        };
        errs.push(format!(
            "the saved composition uses command{} {}, which the plan no longer has (commands now: {}); restore {}, or delete the regression and save a new one",
            if missing.len() > 1 { "s" } else { "" },
            missing.join(", "),
            now_s,
            if missing.len() > 1 { "them" } else { "it" }
        ));
    }
    let mut bad: Vec<String> = d
        .entries
        .iter()
        .filter_map(|e| {
            kinds
                .iter()
                .find(|(k, _)| *k == e.2)
                .filter(|(_, kd)| !PLAN_KINDS.contains(&kd.as_str()))
                .map(|(k, _)| k.clone())
        })
        .collect();
    bad.sort();
    bad.dedup();
    if !bad.is_empty() {
        errs.push(format!(
            "command{} {} cannot be composed (not a driver or anytime command)",
            if bad.len() > 1 { "s" } else { "" },
            bad.join(", ")
        ));
    }
    let late = d.entries.iter().map(|e| e.1).max().unwrap_or(0);
    if late >= steps {
        errs.push(format!(
            "the saved composition runs an entry in round {}, the plan has {} rounds (workload.steps); set steps to at least {}",
            late,
            steps,
            late + 1
        ));
    }
    errs
}

/// What replaying a regression does on a plan (`regression_status`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub schedule: String,
    pub compose: Option<String>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

/// `regression_status(r, meta, steps)`.
pub fn regression_status(r: &Value, meta: &Value, steps: Option<i128>) -> R<Status> {
    let sched = str_of(&r["schedule"]);
    let mut out = Status {
        schedule: sched.clone(),
        compose: None,
        error: None,
        warnings: Vec::new(),
    };
    let Some(w) = w_of(&sched) else {
        return Ok(out);
    };
    if w == "-" {
        return Ok(out);
    }
    let cmds = meta
        .get("commands")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let now: Vec<String> = cmds.iter().map(|c| str_of(&c["id"])).collect();
    let now_kinds: Vec<String> = cmds.iter().map(|c| str_of(&c["kind"])).collect();
    let msteps = meta.get("steps").and_then(py::int_of).unwrap_or(0);
    let eff_steps = match steps {
        Some(x) if x != 0 => x,
        _ => {
            if msteps != 0 {
                msteps
            } else {
                DEFAULT_STEPS
            }
        }
    };
    let compose = r.get("compose").filter(|c| truthy(c)).map(str_of);
    let saved_cmds: Option<Vec<String>> = r.get("commands").filter(|c| !c.is_null()).map(|c| {
        c.as_array()
            .map(|a| a.iter().map(str_of).collect())
            .unwrap_or_default()
    });
    let params = meta.get("params").cloned().unwrap_or(Value::Null);
    let d: Resolved = if let Some(key) = w.strip_prefix('@') {
        let Some(c) = compose else {
            out.error = Some(format!(
                "W={w} but the regression has no `# compose:` line (the resolved composition)"
            ));
            return Ok(out);
        };
        if key_of(&c) != key {
            out.error = Some(format!(
                "the `# compose:` line and W={} disagree (one of them was edited): the line's key is @{}",
                w,
                key_of(&c)
            ));
            return Ok(out);
        }
        out.compose = Some(c.clone());
        match parse_resolved(&c) {
            Ok(d) => d,
            Err(e) => {
                out.error = Some(e);
                return Ok(out);
            }
        }
    } else if w.starts_with(':') {
        match parse_resolved(&w) {
            Ok(d) => d,
            Err(e) => {
                out.error = Some(e);
                return Ok(out);
            }
        }
    } else if saved_cmds.as_ref().is_some_and(|c| *c != now) {
        let old = saved_cmds.unwrap_or_default();
        let kinds: Vec<Option<&str>> = old.iter().map(|c| kind_of(c)).collect();
        if kinds.iter().any(|k| k.is_none()) {
            out.error = Some(format!(
                "the saved command list has an id that is not <kind>_<name>: {}",
                old.join(" ")
            ));
            return Ok(out);
        }
        let kinds: Vec<String> = kinds
            .into_iter()
            .map(|k| k.unwrap_or("").to_string())
            .collect();
        let d = match resolve_seed(&w, &old, &kinds, eff_steps, &params)? {
            Ok(d) => d,
            Err(e) => {
                out.error = Some(e);
                return Ok(out);
            }
        };
        let res = render_resolved(&d);
        let mut sc = with_w(&sched, &res);
        if !fits(&sc) {
            sc = with_w(&sched, &format!("@{}", key_of(&res)));
            out.compose = Some(res);
        }
        out.schedule = sc;
        out.warnings.push(format!(
            "saved in the index form with another command list (then: {}); replayed as the composition it ran then, by command id",
            old.join(" ")
        ));
        d
    } else {
        if saved_cmds.is_none() || cmds.is_empty() {
            if meta.get("composed").is_some_and(truthy) {
                out.warnings.push(format!(
                    "W={w} is in the seed form without the command list it was saved with, so it composes from the current list; save it again for a stable composition"
                ));
            }
            return Ok(out);
        }
        match resolve_seed(&w, &now, &now_kinds, eff_steps, &params)? {
            Ok(d) => d,
            Err(e) => {
                out.error = Some(e);
                return Ok(out);
            }
        }
    };
    let errs = resolved_problems(&d, meta, steps);
    if !errs.is_empty() {
        out.error = Some(errs.join("; "));
    }
    if let Some(Value::Object(cs)) = r.get("contents") {
        let mut items: Vec<(String, String)> =
            cs.iter().map(|(k, v)| (k.clone(), str_of(v))).collect();
        items.sort();
        for (cid, sha) in items {
            let have = cmds
                .iter()
                .find(|c| str_of(&c["id"]) == cid)
                .and_then(|c| c.get("content"))
                .filter(|x| truthy(x));
            if let Some(h) = have {
                if str_of(h) != sha {
                    out.warnings.push(format!(
                        "command {} changed since the regression was saved (content {}, now {})",
                        cid,
                        sha,
                        str_of(h)
                    ));
                }
            }
        }
    }
    Ok(out)
}

/// Is `m` (a manifest mapping) carrying a test plan key?
pub fn has_test(m: &Value) -> Result<bool, ManifestError> {
    has(m, "test")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints() {
        assert_eq!(py_int(" 0x1F ", 16), Ok(31));
        assert_eq!(py_int("1_0", 16), Ok(16));
        assert!(py_int("zz", 16).unwrap_err().contains("base 16: 'zz'"));
        assert_eq!(py_int("+7", 10), Ok(7));
    }

    #[test]
    fn resolved_round_trip() {
        let v = ":p2,r200,cparallel_driver_transfer,7.4.0.721476632,8.4.0.40276333";
        assert_eq!(render_resolved(&parse_resolved(v).unwrap()), v);
        assert_eq!(key_of(v).len(), 12);
    }

    #[test]
    fn splitlines_like_python() {
        assert_eq!(splitlines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
        assert_eq!(splitlines("a\n\nb"), vec!["a", "", "b"]);
    }
}
