#![doc = include_str!("../README.md")]
//!
//! ## Notes for hydo's developers
//!
//! Error logs as findings, the host side (the Rust port of `hydo/onboard/hydo_errors.py`;
//! MANIFEST.md "Error logs as findings", docs/HISTORY.md M94).
//!
//! A manifest with `errors:` makes the guest print one record per error KIND (a service plus a
//! template: the log line with its timestamps, numbers, ids and hex replaced) at the end of every
//! boot (`hydo-init-errlog.sh`, which stays shell inside images):
//!
//! ```text
//! HYDO-ERRLOG {"svc":"redis","src":"log","n":2,"at":"s,12","tpl":"#:M <date> <time> # ...","ex":"..."}
//! HYDO-ERRLOG-SUM {"boot":"first","steps":20,"lines":5,"kinds":3,"shown":3}
//! ```
//!
//! A kind's [`signature`] is the first 12 hex digits of `sha256("<service>\n<template>")`. The
//! project's reviewed list `hydo/known-errors.yaml` ([`load_known`]) says which kinds are expected
//! (by `sig:` or by a `match:` regular expression on the template), optionally only in runs with a
//! fault (`with:`) or only for lines written while it was on (`during:`). Every other kind is NEW.
//! The list is read only on the host: a run's findings are a pure function of its consoles, its
//! schedule and the list.
//!
//! The library API (what the explorer and the `hydo` command call):
//! - one run: [`analyse`] (consoles + schedule + list -> [`Analysis`]), [`apply`] (fold it into a
//!   run's result: `errors_new`, `errors_known`, and with `unknown: fail` the VIOLATION verdict),
//!   [`summary_lines`], [`write_run_json`], [`run_files`], and [`for_run_dir`] (all of that for
//!   `hydo run` / `hydo repro`);
//! - an exploration: [`collect`] (runs -> one [`Aggregate`] per signature, in first-seen order) and
//!   [`report_md`] (the report.md section);
//! - the list: [`load_known`], [`default_file`], [`explain`], [`accept`];
//! - the CLI: [`cli`] (`hydo errors list|accept|check`).
//!
//! Every JSON value written here has the key order and the text Python's `json.dump(.., indent=1)`
//! gives, so files stay byte-identical to the Python version's.

mod pyregex;

use hydo_manifest::composer::py_int;
use hydo_manifest::py::{
    eq, json_dumps, json_dumps_indent, num_of, repr, rstrip, split_ws, str_of, strip, truthy,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};

pub use pyregex::translate as translate_regex;

/// The step rank that stands for "until the end of the run" in [`fault_windows`].
pub const INF: i64 = 1 << 30;
const KNOWN_KEYS: &[&str] = &[
    "by",
    "date",
    "description",
    "during",
    "example",
    "match",
    "reason",
    "service",
    "sig",
    "tpl",
    "with",
];
const TOP_KEYS: &[&str] = &["description", "format", "known", "unknown"];
/// The default places of the known list, relative to the working directory.
pub const DEFAULT_FILES: [&str; 2] = ["hydo/known-errors.yaml", "known-errors.yaml"];

/// The usage text of `hydo errors`.
pub const USAGE: &str = "usage:
    hydo errors list   [DIR]                       the new and known error kinds of a run or an exploration
                                                   (default DIR: the last exploration)
    hydo errors accept SIG [SIG...] --reason TEXT [--by NAME] [--with FAULT]... [--during FAULT]...
                       [--from DIR] [--file FILE]  add kinds to the known list (default hydo/known-errors.yaml)
    hydo errors check  [--file FILE]               validate the known list
FAULT: K (any kill), K=mysql, P=redis, N=kafka, C (a power cut), T (a clock jump), disk, disk=ioerr,
R=everysec (a knob value). --with: only in runs that have the fault; --during: only for lines
written while it was on (and in the workload range right after it).";

/// A refused known list or command line (Python: `ErrorsError`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorsError(pub String);

impl fmt::Display for ErrorsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ErrorsError {}

fn eerr(m: impl Into<String>) -> ErrorsError {
    ErrorsError(m.into())
}

/// `signature(svc, tpl)`: the first 12 hex digits of `sha256("<svc>\n<tpl>")`.
pub fn signature(svc: &str, tpl: &str) -> String {
    let d = Sha256::digest(format!("{svc}\n{tpl}").as_bytes());
    d.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python's text-mode read of a file opened with `errors="replace"`: bad UTF-8 becomes U+FFFD,
/// `\r\n` and `\r` become `\n`.
pub fn read_text_lossy(p: &Path) -> std::io::Result<String> {
    let b = std::fs::read(p)?;
    Ok(String::from_utf8_lossy(&b)
        .replace("\r\n", "\n")
        .replace('\r', "\n"))
}

/// Python's `int(x)` of a value read from JSON (None when Python would raise).
fn int_value(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_u64().map(|u| u as i64))
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.is_finite())
                    .map(|f| f.trunc() as i64)
            }),
        Value::String(s) => py_int(s, 10).ok().map(|x| x as i64),
        _ => None,
    }
}

// ---------------------------------------------------------------- the console records

/// One error kind of one run.
#[derive(Clone, Debug, PartialEq)]
pub struct Kind {
    pub sig: String,
    pub svc: String,
    pub src: String,
    pub tpl: String,
    pub ex: String,
    /// Lines of this kind (over every boot).
    pub n: i64,
    /// Where its lines were written, in run order: `s` (setup), a workload step, `end`, `r`
    /// (the recovery boot).
    pub at: Vec<String>,
}

impl Kind {
    /// The kind as Python's dict: sig, svc, src, tpl, ex, n, at.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in [
            ("sig", &self.sig),
            ("svc", &self.svc),
            ("src", &self.src),
            ("tpl", &self.tpl),
            ("ex", &self.ex),
        ] {
            m.insert(k.into(), Value::String(v.clone()));
        }
        m.insert("n".into(), Value::from(self.n));
        m.insert(
            "at".into(),
            Value::Array(self.at.iter().cloned().map(Value::String).collect()),
        );
        m
    }
}

/// What the consoles of one run carry ([`parse_consoles`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    /// The HYDO-ERRLOG-SUM records, one per boot.
    pub boots: Vec<Value>,
    pub steps: i64,
    pub kinds: Vec<Kind>,
}

/// `re.match(r"<prefix>(\{.*\})\s*$", line)`: the JSON text of a record line.
fn record_body<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(prefix)?;
    let t = rstrip(rest);
    (t.len() >= 2 && t.starts_with('{') && t.ends_with('}')).then_some(t)
}

fn record_json(line: &str, prefix: &str) -> Option<Option<Map<String, Value>>> {
    record_body(line, prefix).map(|b| match serde_json::from_str::<Value>(b) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    })
}

fn get_str(m: &Map<String, Value>, k: &str) -> String {
    m.get(k).map(str_of).unwrap_or_default()
}

/// `parse_consoles(srcs)`: the records of ONE run, `srcs` = [(name, console text)] (the workload
/// boot, then the recovery boot). None when no boot printed a HYDO-ERRLOG-SUM (the image has no
/// `errors:`). One kind per signature, in first-seen order, with its lines and labels merged.
pub fn parse_consoles(srcs: &[(String, String)]) -> Option<Parsed> {
    let mut boots: Vec<Value> = Vec::new();
    let mut kinds: Vec<Kind> = Vec::new();
    for (_, text) in srcs {
        // the engine appends a re-run of the same tag to the same console file: only the last
        // boot's block counts (the records after the summary before the last one)
        let mut lines: Vec<String> = hydo_manifest::composer::splitlines(text)
            .into_iter()
            .filter_map(|l| l.find("HYDO-ERRLOG").map(|i| l[i..].to_string()))
            .collect();
        let ends: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| record_body(l, "HYDO-ERRLOG-SUM ").is_some())
            .map(|(k, _)| k)
            .collect();
        if ends.len() > 1 {
            lines = lines[ends[ends.len() - 2] + 1..].to_vec();
        }
        for line in &lines {
            if let Some(m) = record_json(line, "HYDO-ERRLOG-SUM ") {
                if let Some(m) = m {
                    boots.push(Value::Object(m));
                }
                continue;
            }
            let Some(Some(r)) = record_json(line, "HYDO-ERRLOG ") else {
                continue;
            };
            let (svc, tpl) = (get_str(&r, "svc"), get_str(&r, "tpl"));
            let s = signature(&svc, &tpl);
            let k = match kinds.iter().position(|k| k.sig == s) {
                Some(i) => &mut kinds[i],
                None => {
                    kinds.push(Kind {
                        sig: s,
                        svc,
                        src: get_str(&r, "src"),
                        tpl,
                        ex: get_str(&r, "ex"),
                        n: 0,
                        at: Vec::new(),
                    });
                    kinds.last_mut().unwrap()
                }
            };
            k.n += r.get("n").and_then(int_value).unwrap_or(0);
            let at = r.get("at").map(str_of).unwrap_or_default();
            for lab in at.split(',') {
                if !lab.is_empty() && !k.at.iter().any(|x| x == lab) {
                    k.at.push(lab.to_string());
                }
            }
        }
    }
    if boots.is_empty() {
        return None;
    }
    let steps = boots
        .iter()
        .map(|b| b.get("steps").and_then(int_value).unwrap_or(0))
        .max()
        .unwrap_or(0);
    for k in kinds.iter_mut() {
        k.at.sort_by_key(|l| label_rank(l, steps));
    }
    Some(Parsed {
        boots,
        steps,
        kinds,
    })
}

/// `label_rank(lab, steps)`: s (before the workload) < 0..steps-1 (workload ranges) < end
/// (= steps) < r (recovery boot).
pub fn label_rank(lab: &str, steps: i64) -> i64 {
    match lab {
        "s" => -1,
        "end" => steps,
        "r" => steps + 1,
        _ => py_int(lab, 10).map(|x| x as i64).unwrap_or(steps),
    }
}

/// `label_text(labels)`: `s` reads "setup", `r` reads "recovery".
pub fn label_text<S: AsRef<str>>(labels: &[S]) -> String {
    labels
        .iter()
        .map(|l| match l.as_ref() {
            "s" => "setup",
            "r" => "recovery",
            x => x,
        })
        .collect::<Vec<_>>()
        .join(",")
}

// ---------------------------------------------------------------- faults of a schedule

/// `fault_windows(schedule, steps)`: (token, lo, hi) for the schedule's fault and knob tokens: the
/// step ranks during which a line counts as written "while the fault was on" (hi inclusive: the
/// workload range right after the fault is lifted is part of it).
pub fn fault_windows(schedule: &str, steps: i64) -> Vec<(String, i64, i64)> {
    let (host, guest) = schedule.split_once('|').unwrap_or((schedule, ""));
    let mut out = Vec::new();
    for t in split_ws(host) {
        if t.starts_with("disk=") && t != "disk=none" {
            out.push((t.to_string(), 0, steps));
        } else if t.starts_with("cf=") {
            out.push((t.to_string(), 0, INF));
        }
    }
    let mut toks = split_ws(guest);
    if toks.first() == Some(&"run") {
        toks.remove(0);
    }
    let cap = |v: i64| v.min(steps);
    let int = |s: &str| py_int(s, 10).map(|x| x as i64);
    for t in toks {
        let Some((k, v)) = t.split_once('=') else {
            continue;
        };
        // ^([^@]+)@(\d+)(.*)$
        let m = v.split_once('@').and_then(|(name, rest)| {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            (!name.is_empty() && !digits.is_empty())
                .then(|| (digits.clone(), rest[digits.len()..].to_string()))
        });
        let win: Result<(i64, i64), String> = (|| {
            if let Some((at, tail)) = &m {
                if (k == "K" || k == "P") && tail.starts_with('+') {
                    let at = int(at)?;
                    return Ok((at, cap(at + int(&tail[1..])?)));
                }
                if k == "P" || k == "X" || k == "F" {
                    let at = int(at)?;
                    return Ok((at, cap(at + 1)));
                }
                if k == "N" || k == "O" || k == "H" {
                    if let Some(r) = tail.strip_prefix('-') {
                        let d: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
                        if !d.is_empty() {
                            return Ok((int(at)?, cap(int(&d)?)));
                        }
                    }
                }
            }
            if k == "C" {
                return Ok((int(v)?, INF));
            }
            if k == "T" {
                return Ok((int(v.split(':').next().unwrap_or(""))?, INF));
            }
            Ok((-1, INF))
        })();
        let (lo, hi) = win.unwrap_or((-1, INF));
        out.push((t.to_string(), lo, hi));
    }
    out
}

/// `fault_matches(pat, tok)`: the FAULT patterns of the known list: "K" (any kill), "K=mysql" (a
/// kill of mysql), "C" (a power cut, C= or cf=), "disk" (any disk fault), "disk=ioerr",
/// "R=everysec" (a knob value).
pub fn fault_matches(pat: &str, tok: &str) -> bool {
    tok == pat
        || ['@', '=', ':']
            .iter()
            .any(|c| tok.starts_with(&format!("{pat}{c}")))
        || (pat == "C" && tok.starts_with("cf="))
}

// ---------------------------------------------------------------- the known list

/// One entry of the known list.
#[derive(Clone, Debug)]
pub struct KnownEntry {
    /// The entry as written (with `with:` / `during:` turned into lists).
    pub fields: Map<String, Value>,
    re: Option<fancy_regex::Regex>,
    with: Option<Vec<String>>,
    during: Option<Vec<String>>,
}

impl KnownEntry {
    /// The entry's `reason:`.
    pub fn reason(&self) -> Value {
        self.fields.get("reason").cloned().unwrap_or(Value::Null)
    }
}

/// The known list ([`load_known`]).
#[derive(Clone, Debug)]
pub struct KnownList {
    /// The file as given (None: no list, every kind is new).
    pub path: Option<String>,
    /// sha256 of the file's text.
    pub sha256: Option<String>,
    /// `report` (a new kind is a finding of its own) or `fail` (it also fails the run); the
    /// explorer's `--errors` overrides it.
    pub unknown: String,
    pub known: Vec<KnownEntry>,
}

impl KnownList {
    /// No list: every kind is new, `unknown: report`.
    pub fn empty() -> KnownList {
        KnownList {
            path: None,
            sha256: None,
            unknown: "report".into(),
            known: Vec::new(),
        }
    }
}

/// `default_file()`: `hydo/known-errors.yaml` or `known-errors.yaml` under `start`, if present (the
/// path is returned as Python builds it: relative when `start` is ".").
pub fn default_file(start: &Path) -> Option<String> {
    for f in DEFAULT_FILES {
        let p = if start == Path::new(".") {
            f.to_string()
        } else {
            start.join(f).display().to_string()
        };
        if Path::new(&p).is_file() {
            return Some(p);
        }
    }
    None
}

fn sorted_unknown(m: &Map<String, Value>, known: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = m
        .keys()
        .filter(|k| !known.contains(&k.as_str()))
        .cloned()
        .collect();
    v.sort();
    v
}

/// `load_known(path)`: the known list, each entry validated and its `match:` compiled; None: an
/// empty list. A file that cannot be read is an ErrorsError too (Python stops with a traceback).
pub fn load_known(path: Option<&str>) -> Result<KnownList, ErrorsError> {
    let Some(path) = path else {
        return Ok(KnownList::empty());
    };
    let bytes = std::fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            eerr(format!(
                "{path}: no such file (write the known list with `hydo errors accept`, or name one with --file FILE)"
            ))
        } else {
            eerr(format!("{path}: {e}"))
        }
    })?;
    let text = String::from_utf8(bytes)
        .map_err(|_| eerr(format!("{path}: not UTF-8 text")))?
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let parsed = if path.ends_with(".json") {
        serde_json::from_str::<Value>(&text).map_err(|e| eerr(format!("{path}: {e}")))
    } else {
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        hydo_manifest::yaml::parse_yaml(&text, &name).map_err(|e| {
            let m = e.message;
            let m = m
                .strip_prefix(hydo_manifest::py::CRASH_PREFIX)
                .unwrap_or(&m)
                .to_string();
            eerr(format!("{path}: {m}"))
        })
    }?;
    let d = match parsed {
        Value::Null => Map::new(),
        Value::Object(m) => m,
        _ => return Err(eerr(format!("{path}: a mapping (format, unknown, known)"))),
    };
    let bad = sorted_unknown(&d, TOP_KEYS);
    if !bad.is_empty() {
        return Err(eerr(format!(
            "{path}: unknown field(s) {} (known: {})",
            bad.join(", "),
            TOP_KEYS.join(", ")
        )));
    }
    if !eq(d.get("format").unwrap_or(&Value::from(1)), &Value::from(1)) {
        return Err(eerr(format!("{path}: format must be 1")));
    }
    let pol = d
        .get("unknown")
        .cloned()
        .unwrap_or(Value::String("report".into()));
    let pol = match pol.as_str() {
        Some(p @ ("report" | "fail")) => p.to_string(),
        _ => return Err(eerr(format!(
            "{path}: unknown: report (a new kind is a finding of its own) or fail (it also fails the run)"
        ))),
    };
    let ents = match d.get("known") {
        Some(v) if truthy(v) => match v {
            Value::Array(a) => a.clone(),
            _ => return Err(eerr(format!("{path}: known: a list of entries"))),
        },
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for (i, e) in ents.iter().enumerate() {
        let wh = format!("{path}: known[{i}]");
        let Value::Object(e) = e else {
            return Err(eerr(format!("{wh}: a mapping")));
        };
        let bad = sorted_unknown(e, KNOWN_KEYS);
        if !bad.is_empty() {
            return Err(eerr(format!(
                "{wh}: unknown field(s) {} (known: {})",
                bad.join(", "),
                KNOWN_KEYS.join(", ")
            )));
        }
        if e.contains_key("sig") == e.contains_key("match") {
            return Err(eerr(format!(
                "{wh}: exactly one of sig: (a signature) and match: (a regular expression on the template)"
            )));
        }
        if let Some(s) = e.get("sig") {
            let st = str_of(s);
            if !(st.len() == 12 && st.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))) {
                return Err(eerr(format!(
                    "{wh}: sig: 12 hex digits, as `hydo errors list` prints them (got {})",
                    repr(s)
                )));
            }
        }
        let reason = match e.get("reason") {
            Some(v) if truthy(v) => str_of(v),
            _ => String::new(),
        };
        if strip(&reason).is_empty() {
            return Err(eerr(format!(
                "{wh}: reason: why this kind is expected (required)"
            )));
        }
        let mut fields = e.clone();
        let mut re = None;
        if let Some(m) = e.get("match") {
            match pyregex::compile(&str_of(m)) {
                Ok(r) => re = Some(r),
                Err((true, x)) => {
                    return Err(eerr(format!(
                        "{wh}: match: {} is not a regular expression ({x})",
                        repr(m)
                    )))
                }
                Err((false, x)) => {
                    return Err(eerr(format!(
                        "{wh}: match: {} is a Python regular expression this hydo cannot run ({x})",
                        repr(m)
                    )))
                }
            }
        }
        let mut lists: [Option<Vec<String>>; 2] = [None, None];
        for (j, k) in ["with", "during"].into_iter().enumerate() {
            let v = match e.get(k) {
                None | Some(Value::Null) => continue,
                Some(Value::String(s)) => {
                    let v = Value::Array(vec![Value::String(s.clone())]);
                    fields.insert(k.into(), v.clone());
                    v
                }
                Some(v) => v.clone(),
            };
            let ok = match &v {
                Value::Array(a) if !a.is_empty() => a
                    .iter()
                    .all(|x| matches!(x, Value::String(s) if !s.is_empty() && !s.contains(' '))),
                _ => false,
            };
            if !ok {
                return Err(eerr(format!(
                    "{wh}: {k}: a list of faults such as K=mysql, C, disk=ioerr, R=everysec"
                )));
            }
            lists[j] = Some(
                v.as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_str().unwrap_or_default().to_string())
                    .collect(),
            );
        }
        let [with, during] = lists;
        out.push(KnownEntry {
            fields,
            re,
            with,
            during,
        });
    }
    Ok(KnownList {
        path: Some(path.to_string()),
        sha256: Some(sha256_hex(text.as_bytes())),
        unknown: pol,
        known: out,
    })
}

/// `explain(kind, known, schedule, steps)`: the index of the first known entry that covers this
/// kind in this run, or None.
pub fn explain(kind: &Kind, known: &KnownList, schedule: &str, steps: i64) -> Option<usize> {
    let mut wins: Option<Vec<(String, i64, i64)>> = None;
    for (i, e) in known.known.iter().enumerate() {
        if let Some(s) = e.fields.get("service").filter(|s| truthy(s)) {
            if !eq(s, &Value::String(kind.svc.clone())) {
                continue;
            }
        }
        if let Some(s) = e.fields.get("sig") {
            if !eq(s, &Value::String(kind.sig.clone())) {
                continue;
            }
        }
        if let Some(re) = &e.re {
            if !pyregex::search(re, &kind.tpl) {
                continue;
            }
        }
        let w = wins.get_or_insert_with(|| fault_windows(schedule, steps));
        if let Some(with) = &e.with {
            if !with
                .iter()
                .any(|p| w.iter().any(|(t, _, _)| fault_matches(p, t)))
            {
                continue;
            }
        }
        if let Some(during) = &e.during {
            let ws: Vec<(i64, i64)> = w
                .iter()
                .filter(|(t, _, _)| during.iter().any(|p| fault_matches(p, t)))
                .map(|(_, lo, hi)| (*lo, *hi))
                .collect();
            let all = kind.at.iter().all(|l| {
                let r = label_rank(l, steps);
                ws.iter().any(|(lo, hi)| *lo <= r && r <= *hi)
            });
            if !all {
                continue;
            }
        }
        return Some(i);
    }
    None
}

/// A kind the known list covers in this run.
#[derive(Clone, Debug, PartialEq)]
pub struct KnownKind {
    pub kind: Kind,
    /// The index of the entry that covers it.
    pub entry: usize,
    /// That entry's `reason:`.
    pub reason: Value,
}

impl KnownKind {
    /// Python's `dict(kind, entry=i, reason=...)`.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut m = self.kind.to_json();
        m.insert("entry".into(), Value::from(self.entry as u64));
        m.insert("reason".into(), self.reason.clone());
        m
    }
}

/// One run's error kinds against the known list ([`analyse`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub steps: i64,
    pub boots: Vec<Value>,
    pub new: Vec<Kind>,
    pub known: Vec<KnownKind>,
    /// A boot printed only the first kinds (`errors.max`).
    pub truncated: bool,
}

impl Analysis {
    /// The analysis as Python's dict (explore.py writes it as `consoles/<tag>.errors.json`).
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("steps".into(), Value::from(self.steps));
        m.insert("boots".into(), Value::Array(self.boots.clone()));
        m.insert(
            "new".into(),
            Value::Array(
                self.new
                    .iter()
                    .map(|k| Value::Object(k.to_json()))
                    .collect(),
            ),
        );
        m.insert(
            "known".into(),
            Value::Array(
                self.known
                    .iter()
                    .map(|k| Value::Object(k.to_json()))
                    .collect(),
            ),
        );
        m.insert("truncated".into(), Value::Bool(self.truncated));
        Value::Object(m)
    }
}

/// `analyse(srcs, schedule, known)`: one run; None when its consoles carry no records (no
/// `errors:` in the manifest).
pub fn analyse(srcs: &[(String, String)], schedule: &str, known: &KnownList) -> Option<Analysis> {
    let p = parse_consoles(srcs)?;
    let mut new = Vec::new();
    let mut old = Vec::new();
    for k in p.kinds {
        match explain(&k, known, schedule, p.steps) {
            None => new.push(k),
            Some(i) => old.push(KnownKind {
                reason: known.known[i].reason(),
                kind: k,
                entry: i,
            }),
        }
    }
    let truncated = p.boots.iter().any(|b| {
        let g = |k: &str| b.get(k).and_then(num_of).unwrap_or(0.0);
        g("kinds") > g("shown")
    });
    Some(Analysis {
        steps: p.steps,
        boots: p.boots,
        new,
        known: old,
        truncated,
    })
}

/// `verdict_of(kind)`: the VIOLATION line of a run failed by a new kind (`unknown: fail`).
pub fn verdict_of(kind: &Kind) -> String {
    format!(
        "VIOLATION error-log [{}] {} ({})",
        kind.svc,
        head(&kind.tpl, 160),
        kind.sig
    )
}

/// `apply(res, an, known)`: fold one run's analysis into its result (explore.py's result dict,
/// `hydo run`'s run.json): `errors_new` (signatures), `errors_known` (a count), and with
/// `unknown: fail` an OK verdict becomes [`verdict_of`] the first new kind (the old one is kept as
/// `pre_errlog_verdict`). Without an analysis the result is unchanged.
pub fn apply(res: &mut Map<String, Value>, an: Option<&Analysis>, known: &KnownList) {
    let Some(an) = an else {
        return;
    };
    res.insert(
        "errors_new".into(),
        Value::Array(
            an.new
                .iter()
                .map(|k| Value::String(k.sig.clone()))
                .collect(),
        ),
    );
    res.insert("errors_known".into(), Value::from(an.known.len() as u64));
    let ok = res
        .get("verdict")
        .and_then(Value::as_str)
        .is_some_and(|v| v.contains("HYDO-ORACLE: OK"));
    if known.unknown == "fail" && !an.new.is_empty() && ok {
        let v = res["verdict"].clone();
        res.insert("pre_errlog_verdict".into(), v);
        res.insert("verdict".into(), Value::String(verdict_of(&an.new[0])));
    }
}

/// `run_files(prefix)`: the consoles of one run, `<prefix>.log` and `<prefix>.recovery.log`.
pub fn run_files(prefix: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for suffix in ["", ".recovery"] {
        let p = PathBuf::from(format!("{}{}.log", prefix.display(), suffix));
        if p.exists() {
            if let Ok(t) = read_text_lossy(&p) {
                out.push((p.file_name().unwrap().to_string_lossy().into_owned(), t));
            }
        }
    }
    out
}

fn pad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - n))
    }
}

/// `summary_lines(an, limit)`: the lines `hydo run` prints (each after "hydo: ").
pub fn summary_lines(an: Option<&Analysis>, limit: usize) -> Vec<String> {
    let Some(an) = an else {
        return Vec::new();
    };
    let mut l = vec![format!(
        "error logs: {} new kind{}, {} known{}",
        an.new.len(),
        if an.new.len() == 1 { "" } else { "s" },
        an.known.len(),
        if an.truncated {
            " (the guest printed only the first kinds: errors.max)"
        } else {
            ""
        }
    )];
    for k in an.new.iter().take(limit) {
        l.push(format!(
            "  NEW   {}  {} n={} at {} {}",
            k.sig,
            pad(&k.svc, 8),
            pad(&k.n.to_string(), 3),
            pad(&label_text(&k.at), 14),
            head(&k.ex, 110)
        ));
    }
    if an.new.len() > limit {
        l.push(format!("  ... {} more new kinds", an.new.len() - limit));
    }
    for k in an.known.iter().take(limit) {
        let reason = if truthy(&k.reason) {
            str_of(&k.reason)
        } else {
            String::new()
        };
        l.push(format!(
            "  known {}  {} n={} at {} ({})",
            k.kind.sig,
            pad(&k.kind.svc, 8),
            pad(&k.kind.n.to_string(), 3),
            pad(&label_text(&k.kind.at), 14),
            head(&reason, 80)
        ));
    }
    l
}

/// `write_run_json(path, an, known)`: `one.errors.json` of `hydo run` (what `hydo errors
/// list|accept --from <run dir>` read).
pub fn write_run_json(path: &Path, an: &Analysis, known: &KnownList) -> std::io::Result<()> {
    let mut kinds: Vec<Value> = Vec::new();
    for k in &an.new {
        let mut m = k.to_json();
        m.insert("new".into(), Value::Bool(true));
        m.insert("runs".into(), Value::from(1));
        kinds.push(Value::Object(m));
    }
    for k in &an.known {
        let mut m = k.to_json();
        m.insert("new".into(), Value::Bool(false));
        m.insert("runs".into(), Value::from(1));
        kinds.push(Value::Object(m));
    }
    let mut j = Map::new();
    j.insert("known_file".into(), opt_str(&known.path));
    j.insert("known_sha256".into(), opt_str(&known.sha256));
    j.insert("unknown".into(), Value::String(known.unknown.clone()));
    j.insert("steps".into(), Value::from(an.steps));
    j.insert("boots".into(), Value::Array(an.boots.clone()));
    j.insert("kinds".into(), Value::Array(kinds));
    std::fs::write(path, json_dumps_indent(&Value::Object(j), 1, false))
}

fn opt_str(v: &Option<String>) -> Value {
    v.clone().map(Value::String).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------- an exploration's kinds

/// One error kind over an exploration ([`collect`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Aggregate {
    pub sig: String,
    pub svc: String,
    pub src: String,
    pub tpl: String,
    pub ex: String,
    /// Runs that had it (new or known).
    pub runs: u64,
    /// Runs in which it was new.
    pub new_runs: u64,
    /// The first 12 seeds that had it.
    pub seeds: Vec<Value>,
    pub first_seed: Value,
    pub first_schedule: String,
    pub first_at: Vec<String>,
    pub lines: i64,
    /// The first run in which it was new: (seed, schedule, labels, example line).
    pub first_new: Option<(Value, String, Vec<String>, String)>,
    /// The reason of the entry that first covered it (Python: setdefault).
    pub reason: Option<Value>,
    /// Whether `reason` was set before `first_new` (Python's key order).
    reason_first: bool,
}

impl Aggregate {
    /// The aggregate as Python's dict (explore.py writes `dict(x, new=..., shrunk=...)` of it).
    pub fn to_json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in [
            ("sig", &self.sig),
            ("svc", &self.svc),
            ("src", &self.src),
            ("tpl", &self.tpl),
            ("ex", &self.ex),
        ] {
            m.insert(k.into(), Value::String(v.clone()));
        }
        m.insert("runs".into(), Value::from(self.runs));
        m.insert("new_runs".into(), Value::from(self.new_runs));
        m.insert("seeds".into(), Value::Array(self.seeds.clone()));
        m.insert("first_seed".into(), self.first_seed.clone());
        m.insert(
            "first_schedule".into(),
            Value::String(self.first_schedule.clone()),
        );
        m.insert("first_at".into(), strs(&self.first_at));
        m.insert("lines".into(), Value::from(self.lines));
        let reason = |m: &mut Map<String, Value>| {
            if let Some(r) = &self.reason {
                m.insert("reason".into(), r.clone());
            }
        };
        if self.reason_first {
            reason(&mut m);
        }
        if let Some((seed, sched, at, ex)) = &self.first_new {
            m.insert("first_new_seed".into(), seed.clone());
            m.insert("first_new_schedule".into(), Value::String(sched.clone()));
            m.insert("first_new_at".into(), strs(at));
            m.insert("first_new_ex".into(), Value::String(ex.clone()));
        }
        if !self.reason_first {
            reason(&mut m);
        }
        m
    }
}

fn strs(v: &[String]) -> Value {
    Value::Array(v.iter().cloned().map(Value::String).collect())
}

/// `collect(runs)`: runs = [(seed, schedule, analysis)] in seed order -> one aggregate per
/// signature, in first-seen order, new and known apart.
pub fn collect(runs: &[(Value, String, Option<Analysis>)]) -> Vec<Aggregate> {
    let mut agg: Vec<Aggregate> = Vec::new();
    for (seed, sched, an) in runs {
        let Some(an) = an else {
            continue;
        };
        let kinds = an
            .new
            .iter()
            .map(|k| (true, k, None))
            .chain(an.known.iter().map(|k| (false, &k.kind, Some(&k.reason))));
        for (is_new, k, reason) in kinds {
            let i = match agg.iter().position(|a| a.sig == k.sig) {
                Some(i) => i,
                None => {
                    agg.push(Aggregate {
                        sig: k.sig.clone(),
                        svc: k.svc.clone(),
                        src: k.src.clone(),
                        tpl: k.tpl.clone(),
                        ex: k.ex.clone(),
                        runs: 0,
                        new_runs: 0,
                        seeds: Vec::new(),
                        first_seed: seed.clone(),
                        first_schedule: sched.clone(),
                        first_at: k.at.clone(),
                        lines: 0,
                        first_new: None,
                        reason: None,
                        reason_first: false,
                    });
                    agg.len() - 1
                }
            };
            let a = &mut agg[i];
            a.runs += 1;
            a.lines += k.n;
            if is_new {
                a.new_runs += 1;
                if a.first_new.is_none() {
                    a.first_new = Some((seed.clone(), sched.clone(), k.at.clone(), k.ex.clone()));
                }
            } else if a.reason.is_none() {
                a.reason = Some(reason.cloned().unwrap_or(Value::Null));
                a.reason_first = a.first_new.is_none();
            }
            if a.seeds.len() < 12 {
                a.seeds.push(seed.clone());
            }
        }
    }
    agg
}

fn seed_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (num_of(a), num_of(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => str_of(a).cmp(&str_of(b)),
    }
}

/// `report_md(agg, known, shrunk, by_hand)`: the "Error logs" section of an exploration's
/// report.md. `shrunk`: {signature: the kind's shrink result (tries, minimal, fp_a, fp_b,
/// present_a, present_b)}; `by_hand(schedule)`: the command line that runs one schedule.
pub fn report_md(
    agg: &[Aggregate],
    known: &KnownList,
    shrunk: &Map<String, Value>,
    by_hand: &dyn Fn(&str) -> String,
) -> Vec<String> {
    let mut new: Vec<&Aggregate> = agg.iter().filter(|a| a.new_runs > 0).collect();
    let mut old: Vec<&Aggregate> = agg.iter().filter(|a| a.new_runs == 0).collect();
    let mut l = vec![
        "## Error logs (errors: in the manifest)".to_string(),
        String::new(),
        format!(
            "Known list: {}{}; unknown kinds: {}. {} new kind{}, {} only known.",
            known.path.as_deref().unwrap_or("none"),
            known
                .sha256
                .as_ref()
                .map(|s| format!(" (sha256 {})", head(s, 16)))
                .unwrap_or_default(),
            known.unknown,
            new.len(),
            if new.len() == 1 { "" } else { "s" },
            old.len()
        ),
        String::new(),
    ];
    let fnew = |a: &Aggregate| a.first_new.clone().unwrap_or_default();
    new.sort_by(|a, b| seed_cmp(&fnew(a).0, &fnew(b).0).then_with(|| a.sig.cmp(&b.sig)));
    if !new.is_empty() {
        l.extend(
            [
                "### New kinds",
                "",
                "| signature | service | runs | first seed | at (first run) | example |",
                "|---|---|---|---|---|---|",
            ]
            .map(String::from),
        );
        for a in &new {
            let (seed, _, at, ex) = fnew(a);
            l.push(format!(
                "| `{}` | {} | {} | {} | {} | `{}` |",
                a.sig,
                a.svc,
                a.new_runs,
                str_of(&seed),
                label_text(&at),
                head(&ex, 100).replace('|', "/")
            ));
        }
        l.push(String::new());
        for a in &new {
            let (seed, sched, at, ex) = fnew(a);
            l.push(format!(
                "#### `{}` ({}): `{}`",
                a.sig,
                a.svc,
                head(&a.tpl, 160)
            ));
            l.push(String::new());
            l.push(format!("- example: `{}`", head(&ex, 200)));
            l.push(format!(
                "- first schedule (seed {}): `{}`",
                str_of(&seed),
                sched
            ));
            if at.iter().any(|x| x == "s") {
                l.push(
                    "- written before the workload (boot, provisioning), so it is in every schedule: not shrunk; triage it first"
                        .into(),
                );
            }
            if let Some(s) = shrunk.get(&a.sig).filter(|s| truthy(s)) {
                let g = |k: &str| s.get(k).cloned().unwrap_or(Value::Null);
                let minimal = str_of(&g("minimal"));
                l.push(format!(
                    "- minimal schedule ({} shrink runs): `{}`",
                    str_of(&g("tries")),
                    minimal
                ));
                l.push(format!(
                    "- run twice: fingerprints `{}` / `{}` ({}), the kind {}",
                    str_of(&g("fp_a")),
                    str_of(&g("fp_b")),
                    if eq(&g("fp_a"), &g("fp_b")) {
                        "identical"
                    } else {
                        "DIFFERENT"
                    },
                    if truthy(&g("present_a")) && truthy(&g("present_b")) {
                        "in both runs"
                    } else {
                        "NOT in both runs"
                    }
                ));
                l.push(format!("- by hand: {}", by_hand(&minimal)));
            }
            l.push(format!(
                "- expected? `hydo errors accept {} --reason \"...\"` (or `--during <fault>` to accept it only while a fault is on)",
                a.sig
            ));
            l.push(String::new());
        }
    }
    if !old.is_empty() {
        old.sort_by(|a, b| a.sig.cmp(&b.sig));
        l.extend(
            [
                "### Known kinds seen",
                "",
                "| signature | service | runs | reason |",
                "|---|---|---|---|",
            ]
            .map(String::from),
        );
        for a in &old {
            let reason = a
                .reason
                .as_ref()
                .filter(|r| truthy(r))
                .map(str_of)
                .unwrap_or_default();
            l.push(format!(
                "| `{}` | {} | {} | {} |",
                a.sig,
                a.svc,
                a.runs,
                head(&reason, 100).replace('|', "/")
            ));
        }
        l.push(String::new());
    }
    l
}

// ---------------------------------------------------------------- hydo errors (both CLIs)

fn take(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, ErrorsError> {
    if let Some(i) = args.iter().position(|a| a == flag) {
        if i + 1 >= args.len() {
            return Err(eerr(format!("{flag} needs a value")));
        }
        let v = args.remove(i + 1);
        args.remove(i);
        return Ok(Some(v));
    }
    Ok(None)
}

fn take_many(args: &mut Vec<String>, flag: &str) -> Result<Vec<String>, ErrorsError> {
    let mut out = Vec::new();
    while let Some(v) = take(args, flag)? {
        out.push(v);
    }
    Ok(out)
}

/// `kinds_of_dir(d)`: the kinds recorded in an explore dir (exploration.json's error kinds) or a
/// run dir (result.json's error-log findings), read with the results reader, as
/// `{"kinds": [{sig, svc, tpl, ex, at | first_at, runs, new}]}`, and the file read.
pub fn kinds_of_dir(d: &str) -> Result<(Value, String), ErrorsError> {
    let none = || {
        eerr(format!(
            "{d} has no error kinds in {} (hydo explore) or {} (hydo run); was the image built with errors: in its manifest?",
            hydo_results::EXPLORATION_DOC,
            hydo_results::RUN_DOC
        ))
    };
    let r = hydo_results::read_dir(Path::new(d)).map_err(|e| eerr(e.0))?;
    let s = |x: &str| Value::String(x.to_string());
    let (kinds, path) = match r {
        hydo_results::Results::Exploration { doc, path, .. } => {
            let ek = doc.and_then(|d| d.error_kinds).ok_or_else(none)?;
            let kinds: Vec<Value> = ek
                .kinds
                .iter()
                .map(|k| {
                    let mut m = Map::new();
                    m.insert("sig".into(), s(&k.signature));
                    m.insert("svc".into(), s(&k.service));
                    m.insert("tpl".into(), s(&k.template));
                    m.insert("ex".into(), s(&k.example));
                    m.insert("first_at".into(), strs(&k.first_at));
                    m.insert("runs".into(), Value::from(k.runs));
                    m.insert("new".into(), Value::Bool(k.new));
                    Value::Object(m)
                })
                .collect();
            (kinds, path)
        }
        hydo_results::Results::Run { run, path } => {
            if run.error_logs.is_none() {
                return Err(none());
            }
            let kinds: Vec<Value> = run
                .findings_of(hydo_results::kind::ERROR_LOG)
                .map(|f| {
                    let g = |k: &str| f.details.get(k).cloned().unwrap_or(Value::Null);
                    let mut m = Map::new();
                    m.insert("sig".into(), s(&f.id));
                    m.insert("svc".into(), g("service"));
                    m.insert("tpl".into(), g("template"));
                    m.insert("ex".into(), g("example"));
                    m.insert("at".into(), g("at"));
                    m.insert("runs".into(), Value::from(1));
                    m.insert(
                        "new".into(),
                        Value::Bool(f.status == hydo_results::status::NEW),
                    );
                    Value::Object(m)
                })
                .collect();
            (kinds, path)
        }
    };
    let mut j = Map::new();
    j.insert("kinds".into(), Value::Array(kinds));
    Ok((Value::Object(j), path.display().to_string()))
}

impl Aggregate {
    /// The kind in the results format.
    pub fn to_model(&self, shrink: Option<hydo_results::ErrorShrink>) -> hydo_results::ErrorKind {
        hydo_results::ErrorKind {
            signature: self.sig.clone(),
            service: self.svc.clone(),
            source: self.src.clone(),
            template: self.tpl.clone(),
            example: self.ex.clone(),
            new: self.new_runs > 0,
            runs: self.runs,
            new_runs: self.new_runs,
            seeds: self.seeds.clone(),
            first_seed: self.first_seed.clone(),
            first_schedule: self.first_schedule.clone(),
            first_at: self.first_at.clone(),
            lines: self.lines,
            first_new: self.first_new.as_ref().map(|(seed, sched, at, ex)| {
                hydo_results::FirstNew {
                    seed: seed.clone(),
                    schedule: sched.clone(),
                    at: at.clone(),
                    example: ex.clone(),
                }
            }),
            reason: self.reason.clone(),
            shrink,
        }
    }

    /// The kind as [`collect`] made it, from the results format.
    pub fn from_model(k: &hydo_results::ErrorKind) -> Aggregate {
        Aggregate {
            sig: k.signature.clone(),
            svc: k.service.clone(),
            src: k.source.clone(),
            tpl: k.template.clone(),
            ex: k.example.clone(),
            runs: k.runs,
            new_runs: k.new_runs,
            seeds: k.seeds.clone(),
            first_seed: k.first_seed.clone(),
            first_schedule: k.first_schedule.clone(),
            first_at: k.first_at.clone(),
            lines: k.lines,
            first_new: k.first_new.as_ref().map(|f| {
                (
                    f.seed.clone(),
                    f.schedule.clone(),
                    f.at.clone(),
                    f.example.clone(),
                )
            }),
            reason: k.reason.clone(),
            reason_first: false,
        }
    }
}

/// The "Error logs" section of report.md from an exploration's error kinds (the results format):
/// [`report_md`] with each shrunk kind's command line.
pub fn report_md_of(ek: &hydo_results::ErrorKinds) -> Vec<String> {
    let agg: Vec<Aggregate> = ek.kinds.iter().map(Aggregate::from_model).collect();
    let known = KnownList {
        path: ek.known_file.clone(),
        sha256: ek.known_sha256.clone(),
        unknown: ek.unknown.clone(),
        known: Vec::new(),
    };
    let mut shrunk = Map::new();
    for k in &ek.kinds {
        if let Some(s) = &k.shrink {
            let c = |i: usize| s.confirmations.get(i).cloned().unwrap_or_default();
            let fp = |i: usize| c(i).fp.map(Value::String).unwrap_or(Value::Null);
            let mut m = Map::new();
            m.insert("minimal".into(), Value::String(s.minimal.clone()));
            m.insert("tries".into(), Value::from(s.tries));
            m.insert("fp_a".into(), fp(0));
            m.insert("fp_b".into(), fp(1));
            m.insert(
                "verdict".into(),
                c(0).verdict.map(Value::String).unwrap_or(Value::Null),
            );
            m.insert("present_a".into(), Value::Bool(c(0).present == Some(true)));
            m.insert("present_b".into(), Value::Bool(c(1).present == Some(true)));
            shrunk.insert(k.signature.clone(), Value::Object(m));
        }
    }
    let by_hand = |m: &str| {
        ek.kinds
            .iter()
            .filter_map(|k| k.shrink.as_ref())
            .find(|s| s.minimal == m)
            .and_then(|s| s.repro_command.clone())
            .unwrap_or_default()
    };
    report_md(&agg, &known, &shrunk, &by_hand)
}

fn kinds_list(j: &Value) -> Vec<Value> {
    match j.get("kinds") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

/// Today's local date, `%Y-%m-%d`.
pub fn today() -> String {
    // SAFETY: time() and localtime_r() only write into the tm we own.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!(
            "{:04}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday
        )
    }
}

fn q(s: &str) -> String {
    json_dumps(&Value::String(s.to_string()))
}

const NEW_FILE_HEAD: &str = "# Known error kinds: lines a service writes to its log that are expected (hydo/onboard/MANIFEST.md,
# \"Error logs as findings\"). Each entry needs a reason; `hydo errors accept` appends them.
#   sig:    the kind's signature (or match: a regular expression on its template)
#   with:   only in runs whose schedule has one of these faults (K=mysql, C, disk, R=everysec)
#   during: only for lines written while one of these faults was on
format: 1
unknown: report       # report: a new kind is a finding; fail: it also fails the run
known:
";

/// `accept(path, kinds, reason, by, with, during, date)`: append one entry per kind (a kind's
/// recorded dict: sig, svc, tpl, ex) to the known list, creating it if needed; a kind already
/// listed by signature is skipped; a list that would not load is never written. Returns the
/// entries' text.
pub fn accept(
    path: &str,
    kinds: &[Value],
    reason: Option<&str>,
    by: &str,
    with: &[String],
    during: &[String],
    date: Option<&str>,
) -> Result<Vec<String>, ErrorsError> {
    let reason = reason.unwrap_or_default();
    if strip(reason).is_empty() {
        return Err(eerr("--reason: why these kinds are expected (required)"));
    }
    let new_file = !Path::new(path).exists();
    let mut text;
    let mut have: Vec<Value> = Vec::new();
    if new_file {
        text = NEW_FILE_HEAD.to_string();
    } else {
        text = read_text_lossy(Path::new(path)).map_err(|e| eerr(format!("{path}: {e}")))?;
        have = load_known(Some(path))?
            .known
            .iter()
            .map(|e| e.fields.get("sig").cloned().unwrap_or(Value::Null))
            .collect();
        if !text.split('\n').any(|l| l.starts_with("known:")) {
            text = format!("{}\nknown:\n", text.trim_end_matches('\n'));
        }
    }
    let date = date.map(str::to_string).unwrap_or_else(today);
    let mut add = Vec::new();
    for k in kinds {
        let sig = k.get("sig").cloned().unwrap_or(Value::Null);
        if have.iter().any(|h| eq(h, &sig)) {
            continue;
        }
        let g = |f: &str| k.get(f).map(str_of).unwrap_or_else(|| "None".into());
        let mut e = vec![
            format!("  - sig: {}", g("sig")),
            format!("    service: {}", g("svc")),
            format!("    tpl: {}", q(&g("tpl"))),
        ];
        if let Some(ex) = k.get("ex").filter(|x| truthy(x)) {
            e.push(format!("    example: {}", q(&head(&str_of(ex), 200))));
        }
        if !with.is_empty() {
            e.push(format!("    with: [{}]", with.join(", ")));
        }
        if !during.is_empty() {
            e.push(format!("    during: [{}]", during.join(", ")));
        }
        e.push(format!("    reason: {}", q(reason)));
        e.push(format!("    by: {}", q(by)));
        e.push(format!("    date: {}", q(&date)));
        add.push(e.join("\n"));
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    for a in &add {
        text.push_str(a);
        text.push('\n');
    }
    let tmp = format!("{path}.tmp");
    if let Some(d) = Path::new(path)
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
    {
        std::fs::create_dir_all(d).map_err(|e| eerr(format!("{}: {e}", d.display())))?;
    }
    std::fs::write(&tmp, &text).map_err(|e| eerr(format!("{tmp}: {e}")))?;
    load_known(Some(&tmp))?; // never write a list that does not load
    std::fs::rename(&tmp, path).map_err(|e| eerr(format!("{path}: {e}")))?;
    Ok(add)
}

fn cli_inner(
    mut args: Vec<String>,
    default_dir: &str,
    out: &mut dyn std::io::Write,
) -> Result<i32, ErrorsError> {
    let verb = args.remove(0);
    let f = match take(&mut args, "--file")?.filter(|f| !f.is_empty()) {
        Some(f) => f,
        None => default_file(Path::new(".")).unwrap_or_else(|| {
            DEFAULT_FILES[if Path::new("hydo").is_dir() { 0 } else { 1 }].to_string()
        }),
    };
    match verb.as_str() {
        "check" => {
            let k = load_known(Some(&f))?;
            let _ = writeln!(
                out,
                "hydo: {}: {} known kinds, unknown kinds: {}",
                f,
                k.known.len(),
                k.unknown
            );
            Ok(0)
        }
        "list" => {
            let d = args
                .first()
                .cloned()
                .unwrap_or_else(|| default_dir.to_string());
            let (j, p) = kinds_of_dir(&d)?;
            let kinds = kinds_list(&j);
            let _ = writeln!(
                out,
                "hydo: {} ({} kinds; the known list is applied as recorded in that run)",
                p,
                kinds.len()
            );
            for k in &kinds {
                let at = ["at", "first_at"]
                    .iter()
                    .filter_map(|x| k.get(*x).filter(|v| truthy(v)))
                    .next()
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(str_of).collect::<Vec<_>>())
                    .unwrap_or_default();
                let runs = k.get("runs").map(str_of).unwrap_or_else(|| "1".into());
                let ex = k
                    .get("ex")
                    .filter(|x| truthy(x))
                    .map(str_of)
                    .unwrap_or_default();
                let _ = writeln!(
                    out,
                    "{} {}  {} runs={} at {} {}",
                    pad(
                        if k.get("new").is_some_and(truthy) {
                            "NEW"
                        } else {
                            "known"
                        },
                        5
                    ),
                    str_of(&k["sig"]),
                    pad(&str_of(&k["svc"]), 8),
                    pad(&runs, 3),
                    pad(&label_text(&at), 14),
                    head(&ex, 110)
                );
            }
            Ok(0)
        }
        "accept" => {
            let reason = take(&mut args, "--reason")?;
            let by = match take(&mut args, "--by")?.filter(|b| !b.is_empty()) {
                Some(b) => b,
                None => std::env::var("USER")
                    .ok()
                    .filter(|u| !u.is_empty())
                    .unwrap_or_else(|| "unknown".into()),
            };
            let with = take_many(&mut args, "--with")?;
            let during = take_many(&mut args, "--during")?;
            let d = take(&mut args, "--from")?
                .filter(|x| !x.is_empty())
                .unwrap_or_else(|| default_dir.to_string());
            let sigs: Vec<String> = args.into_iter().filter(|a| !a.starts_with('-')).collect();
            if sigs.is_empty() {
                return Err(eerr("usage: hydo errors accept SIG [SIG...] --reason TEXT [--by NAME] [--with FAULT]... [--during FAULT]... [--from DIR] [--file FILE]"));
            }
            let (j, p) = kinds_of_dir(&d)?;
            let kinds = kinds_list(&j);
            // {sig: kind}: a later kind with the same signature replaces an earlier one in place
            let mut idx: Vec<(String, Value)> = Vec::new();
            for k in kinds {
                let s = str_of(&k["sig"]);
                match idx.iter_mut().find(|(x, _)| *x == s) {
                    Some(e) => e.1 = k,
                    None => idx.push((s, k)),
                }
            }
            let mut pick = Vec::new();
            for s in &sigs {
                let hit: Vec<&Value> = idx
                    .iter()
                    .filter(|(sig, _)| sig.starts_with(s.as_str()))
                    .map(|(_, k)| k)
                    .collect();
                if hit.len() != 1 {
                    return Err(eerr(format!(
                        "{}: {} in {}",
                        s,
                        if hit.is_empty() {
                            "no such kind"
                        } else {
                            "ambiguous prefix"
                        },
                        p
                    )));
                }
                pick.push(hit[0].clone());
            }
            let add = accept(&f, &pick, reason.as_deref(), &by, &with, &during, None)?;
            let _ = writeln!(
                out,
                "hydo: {} kind{} added to {}{}",
                add.len(),
                if add.len() == 1 { "" } else { "s" },
                f,
                if add.len() == pick.len() {
                    ""
                } else {
                    " (the others were already there)"
                }
            );
            Ok(0)
        }
        _ => Err(eerr(format!(
            "unknown verb {} (list, accept, check)",
            hydo_manifest::py::repr_str(&verb)
        ))),
    }
}

/// `cli(argv, default_dir)`: `hydo errors list|accept|check` (both platforms). Messages go to
/// stdout, refusals to stderr as `hydo: <message>`. Returns the exit code.
pub fn cli(argv: &[String], default_dir: &str) -> i32 {
    let args = argv.to_vec();
    if args.is_empty() || ["-h", "--help", "help"].contains(&args[0].as_str()) {
        println!("{USAGE}");
        return if args.is_empty() { 2 } else { 0 };
    }
    let stdout = std::io::stdout();
    let mut o = stdout.lock();
    match cli_inner(args, default_dir, &mut o) {
        Ok(rc) => rc,
        Err(e) => {
            drop(o);
            eprintln!("hydo: {e}");
            1
        }
    }
}

/// `for_run_dir(outdir, schedule, res, known_path, out)`: `hydo run` / `hydo repro`: the error
/// kinds of the run whose consoles are in `outdir` (`<tag>.log`, `<tag>.recovery.log`) against
/// the known list (`known_path`, else `hydo/known-errors.yaml` or `./known-errors.yaml`); prints
/// the summary through `out`, folds the result into `res` and returns the analysis with the list
/// (the run's result.json reports them; [`write_run_json`] is the M94 file of the same). Nothing
/// happens for an image without `errors:`. A list that does not load is reported and every kind
/// counts as new.
pub fn for_run_dir(
    outdir: &Path,
    schedule: &str,
    mut res: Map<String, Value>,
    known_path: Option<&str>,
    out: &mut dyn FnMut(&str),
) -> (Map<String, Value>, Option<(Analysis, KnownList)>) {
    let mut cons: Vec<PathBuf> = std::fs::read_dir(outdir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    let n = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    n.ends_with(".log")
                        && !n.starts_with('.')
                        && !n.ends_with(".checks.log")
                        && !n.ends_with("engine.log")
                        && !n.starts_with("worker")
                })
                .collect()
        })
        .unwrap_or_default();
    cons.sort_by(|a, b| {
        let r = |p: &PathBuf| p.to_string_lossy().ends_with(".recovery.log");
        r(a).cmp(&r(b))
            .then_with(|| a.to_string_lossy().cmp(&b.to_string_lossy()))
    });
    let srcs: Vec<(String, String)> = cons
        .iter()
        .filter_map(|p| {
            read_text_lossy(p)
                .ok()
                .map(|t| (p.file_name().unwrap().to_string_lossy().into_owned(), t))
        })
        .collect();
    let path = known_path
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .or_else(|| default_file(Path::new(".")));
    let known = match load_known(path.as_deref()) {
        Ok(k) => k,
        Err(e) => {
            out(&format!(
                "hydo: known errors: {e} (every kind counts as new)"
            ));
            KnownList::empty()
        }
    };
    let Some(an) = analyse(&srcs, schedule, &known) else {
        return (res, None);
    };
    for line in summary_lines(Some(&an), 12) {
        out(&format!("hydo: {line}"));
    }
    if !an.new.is_empty() {
        out(&format!(
            "hydo: expected? hydo errors accept <signature> --reason \"...\" --from {}",
            outdir.display()
        ));
    }
    apply(&mut res, Some(&an), &known);
    if res.get("pre_errlog_verdict").is_some_and(truthy) {
        out(&format!(
            "hydo: verdict with the known-errors list (unknown: fail): {}",
            res.get("verdict").map(str_of).unwrap_or_default()
        ));
    }
    (res, Some((an, known)))
}

#[cfg(test)]
mod tests;
