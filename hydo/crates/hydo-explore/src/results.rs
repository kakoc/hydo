//! The explorer's side of the results format (the hydo-results crate): a run's result as a
//! [`Run`] record with its findings, the event stream, and the property table and liveness
//! summary over many runs. `hydo run` and `hydo repro` (hydo-cli) use the same conversions, so a
//! run directory and an exploration report the same things the same way.

use crate::pool::Res;
use crate::py::py_str;
use hydo_results::{
    kind, status, Artifacts, Counters, ErrorLogs, Finding, Fingerprints, Liveness, Property,
    PropertyTable, Run, Verdict,
};
use serde_json::{Map, Value};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

/// The image of a run or an exploration as the results format reports it: the path as given
/// (`path`), the file opened (`file`) when it differs, its sha256 when known, and what its records
/// say (Milestone 155): the fingerprint epoch and the guest kernel from `/etc/hydo/image.json`,
/// the hydo that built it, the manifest's sha256 and the service images' digests from the build
/// record `<image>.json`. An image without the records (built before M155) gets none of those.
pub fn image_of(path: &str, file: &Path, sha256: Option<String>) -> hydo_results::Image {
    let f = file.display().to_string();
    let mut im = hydo_results::Image {
        resolved: (f != path).then_some(f),
        path: path.to_string(),
        sha256,
        ..hydo_results::Image::default()
    };
    if let Ok(Some(r)) = hydo_engine::image::image_record(file) {
        im.epoch = Some(r.epoch);
        im.kernel = Some(format!(
            "{} (Linux {}, sha256 {})",
            r.kernel.file, r.kernel.version, r.kernel.sha256
        ));
    }
    if let Some(b) = hydo_image::read_sidecar(file) {
        im.built_by = Some(hydo_results::BuiltBy {
            version: b.hydo.version,
            commit: b.hydo.commit,
        });
        im.manifest_sha256 = Some(b.manifest.sha256);
        im.services = b
            .services
            .into_iter()
            .map(|s| hydo_results::ServiceImage {
                service: s.service,
                image: s.image,
                digest: s.digest,
                config_digest: s.config_digest,
            })
            .collect();
    }
    im
}

/// The running hydo's fingerprint epoch, for a document (M155).
pub fn this_epoch() -> Option<u64> {
    Some(hydo_image::EPOCH)
}

/// Result keys only the host keeps (never written as they are): the history checks' findings
/// and HYDO-ASSERT records, and the error-log analysis.
pub const CHECKS: &str = "_checks";
pub const CHECK_RECORDS: &str = "_check_records";
pub const ERRLOG: &str = "_errlog";

/// A result without the host-only keys (what a tool that prints a result map shows).
pub fn public(r: &Res) -> Res {
    r.iter()
        .filter(|(k, _)| !k.starts_with('_'))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The event stream (`events.jsonl`): one line per record, flushed as it is written.
pub struct Events {
    f: Mutex<Option<std::fs::File>>,
}

impl Events {
    /// No stream (records are dropped).
    pub fn none() -> Events {
        Events {
            f: Mutex::new(None),
        }
    }

    pub fn create(path: &Path) -> Events {
        let f = std::fs::File::create(path)
            .map_err(|e| eprintln!("hydo-explore: {}: {e}", path.display()))
            .ok();
        Events { f: Mutex::new(f) }
    }

    pub fn write<T: serde::Serialize>(&self, rec: &T) {
        if let Some(f) = self.f.lock().expect("lock").as_mut() {
            let _ = f.write_all(hydo_results::to_line(rec).as_bytes());
            let _ = f.flush();
        }
    }
}

/// `ok`, `violation` or `error` of a verdict line.
pub fn kind_of(v: &str) -> String {
    hydo_engine::protocol::verdict_kind(v).to_string()
}

fn s(r: &Res, k: &str) -> Option<String> {
    r.get(k).filter(|v| !v.is_null()).map(py_str)
}

fn u(r: &Res, k: &str) -> Option<u64> {
    r.get(k).and_then(Value::as_u64)
}

/// A result's verdict.
pub fn verdict_of(r: &Res) -> Verdict {
    let text = s(r, "verdict").unwrap_or_default();
    Verdict {
        kind: kind_of(&text),
        text,
        guest: s(r, "guest_verdict"),
        before_error_logs: s(r, "pre_errlog_verdict"),
    }
}

/// The verdict the guest itself gave (before the host's history checks and error logs).
pub fn guest_verdict(r: &Res) -> String {
    s(r, "guest_verdict")
        .or_else(|| s(r, "pre_errlog_verdict"))
        .or_else(|| s(r, "verdict"))
        .unwrap_or_default()
}

fn counters_of(r: &Res) -> Option<Counters> {
    let c = Counters {
        tjk: r
            .get("tjk")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).collect()),
        tjn: u(r, "tjn"),
        pctf: u(r, "pctf"),
        pctw: u(r, "pctw"),
        xtn: u(r, "xtn"),
        qn: u(r, "qn"),
    };
    (c != Counters::default()).then_some(c)
}

/// The grouping key of a verdict (report.rs `sig_key`).
pub fn signature(v: &str) -> String {
    crate::report::sig_key(v)
}

/// The guest's failed invariant (none when the guest's verdict is OK). Its id is the invariant
/// the verdict names (`VIOLATION [<id>] ...`), else the verdict's signature.
pub fn invariant_finding(guest: &str) -> Option<Finding> {
    let k = kind_of(guest);
    if k == "ok" || guest.is_empty() {
        return None;
    }
    let sig = signature(guest);
    let named = guest
        .split_once('[')
        .and_then(|(pre, rest)| {
            (pre.ends_with("VIOLATION ") || pre.ends_with("invariant "))
                .then(|| rest.split_once(']'))
                .flatten()
        })
        .map(|(id, _)| id.to_string())
        .or_else(|| {
            guest
                .split_once("ERROR invariant ")
                .and_then(|(_, r)| r.split_whitespace().next().map(String::from))
        });
    let mut d = Map::new();
    d.insert("signature".into(), Value::String(sig.clone()));
    Some(Finding {
        kind: kind::INVARIANT.into(),
        id: named.unwrap_or(sig),
        status: k,
        message: guest.to_string(),
        details: d,
    })
}

/// The history checks of a run as findings.
pub fn history_findings(c: &hydo_history::Checks) -> Vec<Finding> {
    c.checks
        .iter()
        .map(|x| {
            let mut d = Map::new();
            d.insert("check".into(), Value::String(x.check.clone()));
            d.insert("history".into(), Value::String(x.history.clone()));
            d.insert("description".into(), Value::String(x.msg.clone()));
            if let Ok(v) = serde_json::from_str::<Value>(&x.details) {
                d.insert("details".into(), v);
            }
            if !x.explanation.is_empty() {
                d.insert("explanation".into(), Value::String(x.explanation.clone()));
            }
            if !x.warnings.is_empty() {
                d.insert(
                    "warnings".into(),
                    Value::Array(x.warnings.iter().cloned().map(Value::String).collect()),
                );
            }
            Finding {
                kind: kind::HISTORY_CHECK.into(),
                id: x.id.clone(),
                status: x.status.name().into(),
                message: x.summary.clone(),
                details: d,
            }
        })
        .collect()
}

/// What the host keeps of a run's history checks in its result: the findings and the
/// HYDO-ASSERT records (the property table reads them with the consoles).
pub fn keep_checks(res: &mut Res, c: &hydo_history::Checks) {
    let f: Vec<Value> = history_findings(c)
        .into_iter()
        .map(|f| serde_json::to_value(f).expect("a finding"))
        .collect();
    res.insert(CHECKS.into(), Value::Array(f));
    res.insert(
        CHECK_RECORDS.into(),
        Value::Array(c.records.iter().cloned().map(Value::String).collect()),
    );
}

/// The error kinds of a run's analysis (hydo_errors::Analysis::to_json) as findings, and the
/// analysis's context.
pub fn errlog_findings(an: &Value) -> (Vec<Finding>, ErrorLogs) {
    let mut out = Vec::new();
    for (st, list) in [(status::NEW, "new"), (status::KNOWN, "known")] {
        for k in an.get(list).and_then(Value::as_array).into_iter().flatten() {
            let g = |f: &str| k.get(f).cloned().unwrap_or(Value::Null);
            let mut d = Map::new();
            for (from, to) in [
                ("svc", "service"),
                ("src", "source"),
                ("tpl", "template"),
                ("ex", "example"),
                ("n", "lines"),
                ("at", "at"),
                ("entry", "known_entry"),
                ("reason", "reason"),
            ] {
                if let Some(v) = k.get(from) {
                    d.insert(to.into(), v.clone());
                }
            }
            out.push(Finding {
                kind: kind::ERROR_LOG.into(),
                id: py_str(&g("sig")),
                status: st.into(),
                message: format!("[{}] {}", py_str(&g("svc")), py_str(&g("tpl"))),
                details: d,
            });
        }
    }
    let el = ErrorLogs {
        known_file: None,
        known_sha256: None,
        unknown: None,
        steps: an.get("steps").and_then(Value::as_i64),
        boots: an
            .get("boots")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        truncated: an.get("truncated") == Some(&Value::Bool(true)),
    };
    (out, el)
}

/// The failed assertion properties in one run's merged consoles, sorted by id (the explorer's
/// `assert_failures`, with each property's message and its first failing record).
pub fn assertion_findings(text: &str) -> Vec<Finding> {
    let mut msgs: Map<String, Value> = Map::new();
    let mut bad: Vec<(String, Finding)> = Vec::new();
    for rec in crate::report::records(text) {
        let t = rec.get("t").and_then(Value::as_str).unwrap_or("");
        let id = rec.get("id").map(py_str).unwrap_or_else(|| "?".into());
        if let Some(m) = rec.get("msg").filter(|m| !m.is_null()) {
            msgs.entry(id.clone()).or_insert_with(|| m.clone());
        }
        let ok_false = rec.get("ok") == Some(&Value::Bool(false));
        if (matches!(t, "always" | "always_or_unreachable" | "eventually") && ok_false)
            || t == "unreachable"
        {
            if bad.iter().any(|(k, _)| *k == id) {
                continue;
            }
            let mut d = Map::new();
            d.insert("assertion".into(), Value::String(t.into()));
            if let Some(x) = rec.get("details") {
                d.insert("details".into(), x.clone());
            }
            bad.push((
                id.clone(),
                Finding {
                    kind: kind::ASSERTION.into(),
                    id,
                    status: status::VIOLATION.into(),
                    message: String::new(),
                    details: d,
                },
            ));
        }
    }
    bad.sort_by(|a, b| a.0.cmp(&b.0));
    bad.into_iter()
        .map(|(id, mut f)| {
            f.message = msgs.get(&id).map(py_str).unwrap_or_default();
            f
        })
        .collect()
}

/// One run's consoles and the host's check records as one text (what the property table reads).
pub fn merged_text(consoles: &[String], r: &Res) -> String {
    let mut text: String = consoles.concat();
    if let Some(Value::Array(recs)) = r.get(CHECK_RECORDS) {
        for x in recs {
            text.push_str(&py_str(x));
            text.push('\n');
        }
    }
    text
}

/// A run's result as a record. `consoles` = (path relative to the document's directory, text)
/// of the run's consoles, the run's own first.
pub fn run_of(tag: &str, text: &str, r: &Res, consoles: &[(String, String)]) -> Run {
    let verdict = verdict_of(r);
    let mut findings = Vec::new();
    findings.extend(invariant_finding(&guest_verdict(r)));
    let texts: Vec<String> = consoles.iter().map(|c| c.1.clone()).collect();
    findings.extend(assertion_findings(&merged_text(&texts, r)));
    if let Some(Value::Array(fs)) = r.get(CHECKS) {
        findings.extend(
            fs.iter()
                .filter_map(|f| serde_json::from_value::<Finding>(f.clone()).ok()),
        );
    }
    let mut error_logs = None;
    if let Some(an) = r.get(ERRLOG) {
        let (f, el) = errlog_findings(an);
        findings.extend(f);
        error_logs = Some(el);
    }
    Run {
        tag: Some(tag.to_string()),
        schedule: text.to_string(),
        verdict,
        fingerprints: Fingerprints {
            fp: s(r, "fp").unwrap_or_default(),
            wfp: s(r, "wfp"),
            ctl: s(r, "ctl"),
        },
        findings,
        error_logs,
        secs: r.get("secs").and_then(Value::as_f64),
        counters: counters_of(r),
        worker: u(r, "worker"),
        cached: r.get("cached").and_then(Value::as_bool),
        artifacts: (!consoles.is_empty()).then(|| Artifacts {
            consoles: consoles.iter().map(|c| c.0.clone()).collect(),
            engine_log: None,
        }),
        ..Run::default()
    }
}

/// An explorer run's consoles (`consoles/<tag>.log`, `consoles/<tag>.recovery.log`).
pub fn explorer_consoles(outdir: &Path, tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for suffix in ["", ".recovery"] {
        let rel = format!("consoles/{tag}{suffix}.log");
        if let Some(t) = crate::pool::read_text(&outdir.join(&rel)) {
            out.push((rel, t));
        }
    }
    out
}

/// An explorer run as a record.
pub fn explorer_run(outdir: &Path, tag: &str, text: &str, r: &Res, role: &str) -> Run {
    let mut run = run_of(tag, text, r, &explorer_consoles(outdir, tag));
    run.role = Some(role.to_string());
    // an agent image's run: its control transcripts (M126)
    run.control =
        hydo_control::agent::controls_in(&outdir.join("consoles"), &format!("{tag}."), "consoles/");
    run
}

// ---------------------------------------------------------------- the property table

/// The C header's limits (hydo/assert/hydo_assert_table.h), kept so the table is the one
/// hydo_assert_report printed.
const MAX_PROPS: usize = 256;
const ID_LEN: usize = 96;
const MSG_LEN: usize = 160;
const TYPE_LEN: usize = 24;
const LINE_LEN: usize = 1024;

#[derive(Default)]
struct P {
    ty: Vec<u8>,
    id: Vec<u8>,
    msg: Vec<u8>,
    declared: bool,
    passes: u64,
    fails: u64,
    runs_pass: u64,
    runs_fail: u64,
    first_fail: Vec<u8>,
    max_ok_elapsed: f64,
    run_pass: bool,
    run_fail: bool,
}

/// hydo_assert_table.h's `struct ha_table`, in Rust.
#[derive(Default)]
struct Table {
    p: Vec<P>,
    runs: u64,
    setup: u64,
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn jstr(rec: &[u8], key: &str, outn: usize) -> Option<Vec<u8>> {
    let pat = format!("\"{key}\":\"");
    let mut p = find(rec, pat.as_bytes())? + pat.len();
    let mut out = Vec::new();
    while p < rec.len() && rec[p] != b'"' && rec[p] != b'\n' && out.len() + 1 < outn {
        if rec[p] == b'\\' && p + 1 < rec.len() {
            p += 1;
        }
        out.push(rec[p]);
        p += 1;
    }
    Some(out)
}

fn jraw(rec: &[u8], key: &str, outn: usize) -> Option<Vec<u8>> {
    let pat = format!("\"{key}\":");
    let p = find(rec, pat.as_bytes())? + pat.len();
    let mut e = p;
    while e < rec.len() && rec[e] != b'\n' && rec[e] != b'\r' {
        e += 1;
    }
    let len = (e - p).min(outn - 1);
    let mut out = rec[p..p + len].to_vec();
    let mut depth = 0i64;
    for k in 0..out.len() {
        if out[k] == b'{' {
            depth += 1;
        } else if out[k] == b'}' {
            depth -= 1;
            if depth < 0 {
                out.truncate(k);
                break;
            }
        }
    }
    Some(out)
}

/// C's `atof`: the longest decimal prefix (after blanks), 0 when there is none.
fn atof(b: &[u8]) -> f64 {
    let t: Vec<u8> = b
        .iter()
        .skip_while(|c| c.is_ascii_whitespace())
        .copied()
        .take_while(|c| c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.' | b'e' | b'E'))
        .collect();
    for n in (1..=t.len()).rev() {
        if let Ok(v) = std::str::from_utf8(&t[..n]).unwrap_or("").parse::<f64>() {
            return v;
        }
    }
    0.0
}

fn cut(b: &[u8], n: usize) -> Vec<u8> {
    b[..b.len().min(n - 1)].to_vec()
}

impl Table {
    fn get(&mut self, id: &[u8], ty: &[u8]) -> Option<usize> {
        if let Some(i) = self.p.iter().position(|p| p.id == id) {
            return Some(i);
        }
        if self.p.len() >= MAX_PROPS {
            return None;
        }
        self.p.push(P {
            id: cut(id, ID_LEN),
            ty: cut(ty, TYPE_LEN),
            max_ok_elapsed: -1.0,
            ..P::default()
        });
        Some(self.p.len() - 1)
    }

    fn record(&mut self, decl: bool, rec: &[u8]) {
        let (Some(ty), Some(id)) = (jstr(rec, "t", TYPE_LEN), jstr(rec, "id", ID_LEN)) else {
            return;
        };
        let msg = jstr(rec, "msg", MSG_LEN).unwrap_or_default();
        let Some(i) = self.get(&id, &ty) else { return };
        let p = &mut self.p[i];
        if p.msg.is_empty() && !msg.is_empty() {
            p.msg = cut(&msg, MSG_LEN);
        }
        if decl {
            p.declared = true;
            return;
        }
        if find(rec, b"\"ok\":true").is_some() {
            p.passes += 1;
            p.run_pass = true;
            if p.ty == b"eventually" {
                let mut v = -1.0;
                if let Some(e) = find(rec, b"\"elapsed_s\":") {
                    let x = &rec[e + 12..];
                    v = atof(if x.first() == Some(&b'"') { &x[1..] } else { x });
                } else if let Some(e) = find(rec, b"\"elapsed_ms\":") {
                    v = atof(&rec[e + 13..]) / 1000.0;
                }
                if v > p.max_ok_elapsed {
                    p.max_ok_elapsed = v;
                }
            }
        } else {
            p.fails += 1;
            p.run_fail = true;
            if p.first_fail.is_empty() {
                p.first_fail = jraw(rec, "details", MSG_LEN).unwrap_or_else(|| b"-".to_vec());
            }
        }
    }

    fn feed(&mut self, text: &str) {
        for p in self.p.iter_mut() {
            p.run_pass = false;
            p.run_fail = false;
        }
        let mut setup = false;
        let b = text.as_bytes();
        let mut lines: Vec<&[u8]> = b.split(|c| *c == b'\n').collect();
        if b.ends_with(b"\n") {
            lines.pop();
        }
        for raw in lines {
            let mut line = &raw[..raw.len().min(LINE_LEN - 1)];
            if let Some(z) = line.iter().position(|c| *c == 0) {
                line = &line[..z];
            }
            if find(line, b"HYDO-SETUP-COMPLETE").is_some() {
                setup = true;
            }
            if let Some(i) = find(line, b"HYDO-ASSERT-DECL ") {
                self.record(true, &line[i + 17..]);
            } else if let Some(i) = find(line, b"HYDO-ASSERT ") {
                self.record(false, &line[i + 12..]);
            }
        }
        for p in self.p.iter_mut() {
            p.runs_pass += p.run_pass as u64;
            p.runs_fail += p.run_fail as u64;
        }
        self.runs += 1;
        self.setup += setup as u64;
    }
}

fn verdict(p: &P) -> &'static str {
    match p.ty.as_slice() {
        b"always" => {
            if p.fails > 0 {
                "FAIL"
            } else if p.passes > 0 {
                "PASS"
            } else {
                "FAIL (never evaluated)"
            }
        }
        b"always_or_unreachable" => {
            if p.fails > 0 {
                "FAIL"
            } else {
                "PASS"
            }
        }
        b"unreachable" => {
            if p.fails > 0 {
                "FAIL (reached)"
            } else {
                "PASS"
            }
        }
        b"eventually" => {
            if p.fails > 0 {
                "FAIL (deadline missed)"
            } else if p.passes > 0 {
                "PASS"
            } else {
                "NOT EVALUATED"
            }
        }
        _ => {
            if p.passes > 0 {
                "PASS"
            } else {
                "NOT EXERCISED"
            }
        }
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The property table of runs (each text one run's merged consoles); None when no run carries a
/// HYDO-ASSERT record.
pub fn property_table(runs: &[String]) -> Option<PropertyTable> {
    let mut t = Table::default();
    let mut any = false;
    for x in runs.iter().filter(|x| x.contains("HYDO-ASSERT")) {
        any = true;
        t.feed(x);
    }
    if !any {
        return None;
    }
    let props: Vec<Property> =
        t.p.iter()
            .map(|p| {
                let v = verdict(p);
                Property {
                    id: lossy(&p.id),
                    assertion: lossy(&p.ty),
                    message: lossy(&p.msg),
                    declared: p.declared,
                    passes: p.passes,
                    fails: p.fails,
                    runs_passed: p.runs_pass,
                    runs_failed: p.runs_fail,
                    verdict: v.into(),
                    status: if v.starts_with("FAIL") {
                        status::VIOLATION
                    } else if v == "NOT EXERCISED" {
                        status::NOT_EXERCISED
                    } else if v == "NOT EVALUATED" {
                        status::NOT_EVALUATED
                    } else {
                        status::OK
                    }
                    .into(),
                    first_failure: (!p.first_fail.is_empty()).then(|| lossy(&p.first_fail)),
                    slowest_ok_s: (p.ty == b"eventually" && p.max_ok_elapsed >= 0.0)
                        .then_some(p.max_ok_elapsed),
                }
            })
            .collect();
    Some(PropertyTable {
        runs: t.runs,
        setup_complete_runs: t.setup,
        violations: props
            .iter()
            .filter(|p| p.verdict.starts_with("FAIL"))
            .count() as u64,
        properties: props,
    })
}

fn pad(s: &str, w: usize) -> String {
    if s.len() >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - s.len()))
    }
}

fn rpad(s: &str, w: usize) -> String {
    if s.len() >= w {
        s.to_string()
    } else {
        format!("{}{s}", " ".repeat(w - s.len()))
    }
}

/// The table as text, as hydo_assert_report printed it (`ha_print`).
pub fn render_properties(t: &PropertyTable) -> String {
    let mut o = format!(
        "{} {} {} {}  {} {}\n",
        pad("type", 22),
        pad("id", 32),
        rpad("ok", 7),
        rpad("fail", 7),
        pad("verdict", 24),
        "first failure / description"
    );
    let (mut nl, mut nlf) = (0, 0);
    for p in &t.properties {
        let mut slow = String::new();
        if p.assertion == "eventually" {
            nl += 1;
            nlf += (p.fails > 0) as u64;
            if let (None, Some(s)) = (&p.first_failure, p.slowest_ok_s) {
                slow = format!("  [slowest in time: {s:.2} s]");
            }
        }
        o.push_str(&format!(
            "{} {} {} {}  {} {}{}{}\n",
            pad(&p.assertion, 22),
            pad(&p.id, 32),
            rpad(&p.passes.to_string(), 7),
            rpad(&p.fails.to_string(), 7),
            pad(&p.verdict, 24),
            p.first_failure.as_deref().unwrap_or(&p.message),
            slow,
            if p.declared { "" } else { "  [not in catalog]" }
        ));
    }
    if nl > 0 {
        o.push_str(&format!(
            "liveness: {nl} eventually properties, {nlf} missed a deadline\n"
        ));
    }
    o.push_str(&format!(
        "runs={} (setup-complete seen in {}); properties={}; violations={}\n",
        t.runs,
        t.setup_complete_runs,
        t.properties.len(),
        t.violations
    ));
    o
}

// ---------------------------------------------------------------- liveness

/// The eventually properties over runs: (run name, merged text) in the order of the runs' file
/// names (the explorer's `liveness_summary`).
pub fn liveness(runs: &[(String, String)]) -> Vec<Liveness> {
    let mut props: std::collections::BTreeMap<String, Liveness> = Default::default();
    for (stem, text) in runs {
        type Episodes = Vec<(bool, f64, Value)>;
        let mut seen: Vec<(String, Episodes)> = Vec::new();
        for rec in crate::report::records(text) {
            if rec.get("t").and_then(Value::as_str) != Some("eventually") {
                continue;
            }
            let det = rec
                .get("details")
                .filter(|v| hydo_manifest::py::truthy(v))
                .cloned()
                .unwrap_or(Value::Object(Map::new()));
            let el = match det.get("elapsed_s") {
                Some(v) if !v.is_null() && v.as_str() != Some("?") => match v {
                    Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.0),
                    other => other.as_f64().unwrap_or(0.0),
                },
                _ => det.get("elapsed_ms").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0,
            };
            let id = rec.get("id").map(py_str).unwrap_or_else(|| "?".into());
            let ok = rec.get("ok") == Some(&Value::Bool(true));
            match seen.iter_mut().find(|(k, _)| *k == id) {
                Some((_, v)) => v.push((ok, el, det)),
                None => seen.push((id, vec![(ok, el, det)])),
            }
        }
        for (pid, eps) in seen {
            let p = props.entry(pid.clone()).or_insert(Liveness {
                id: pid,
                ..Liveness::default()
            });
            if eps.iter().all(|e| e.0) {
                p.held_runs += 1;
                let worst = eps.iter().map(|e| e.1).fold(f64::NEG_INFINITY, f64::max);
                if p.slowest_s.is_none_or(|s| worst > s) {
                    p.slowest_s = Some(worst);
                    p.slowest_run = Some(stem.clone());
                }
            } else {
                p.missed_runs += 1;
                if p.first_miss_run.is_none() {
                    p.first_miss_run = Some(stem.clone());
                    p.first_miss = Some(
                        eps.iter()
                            .find(|e| !e.0)
                            .map(|e| e.2.clone())
                            .unwrap_or(Value::Null),
                    );
                }
            }
        }
    }
    props.into_values().collect()
}

/// The liveness section of report.md.
pub fn render_liveness(l: &[Liveness]) -> Vec<String> {
    if l.is_empty() {
        return Vec::new();
    }
    let mut out = vec![
        "## Liveness (eventually properties, hvx/ASSERTIONS.md)".to_string(),
        String::new(),
        "| property | held in time | missed deadline | slowest in time | first miss |".into(),
        "|---|---|---|---|---|".into(),
    ];
    for p in l {
        let slow = match (p.slowest_s, &p.slowest_run) {
            (Some(w), Some(s)) => format!("{w:.2} s ({s})"),
            _ => "-".into(),
        };
        let miss = match &p.first_miss_run {
            Some(s) => format!(
                "{}: `{}`",
                s,
                p.first_miss
                    .as_ref()
                    .and_then(|d| d.get("out"))
                    .map(py_str)
                    .unwrap_or_default()
            ),
            None => "-".into(),
        };
        out.push(format!(
            "| `{}` | {} | {} | {slow} | {miss} |",
            p.id, p.held_runs, p.missed_runs
        ));
    }
    out.push(String::new());
    out
}
