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
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
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
/// The messages checks' reports (hydo_history::Checks::messages, as JSON values).
pub const MESSAGES: &str = "_messages";
/// The function contracts' findings (kind `contract`, Milestone 241).
pub const CONTRACTS: &str = "_contracts";
/// The value properties' findings (kind `values`, Milestone 291).
pub const VALUES: &str = "_values";
/// How many calls the function contracts judged ({calls, without_return}).
pub const CONTRACT_CALLS: &str = "_contract_calls";
/// Which error paths ran (`RunPaths`, Milestone 306).
pub const PATHS: &str = "_paths";

/// A result without the host-only keys (what a tool that prints a result map shows).
pub fn public(r: &Res) -> Res {
    r.iter()
        .filter(|(k, _)| !k.starts_with('_'))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The event stream (`events.jsonl`): one line per record, flushed as it is written. A stream of
/// an exploration also counts its runs as they are made ([`Events::counted`]), and prints a progress line on standard error every
/// [`PROGRESS_EVERY_S`] seconds (Milestone 301: 0.4.0's trial waited 30 minutes for a walk's
/// exploration, most of it shrinking, without a line).
pub struct Events {
    f: Mutex<Option<std::fs::File>>,
    progress: std::sync::Arc<Progress>,
    ticker: Option<std::thread::JoinHandle<()>>,
}

/// How often an exploration says how far it is.
pub const PROGRESS_EVERY_S: u64 = 30;

/// The runs an exploration made so far, by their part in it.
#[derive(Debug, Default)]
pub struct Progress {
    /// The schedules of the exploration, and how many of them failed.
    pub first: std::sync::atomic::AtomicU64,
    pub failed: std::sync::atomic::AtomicU64,
    /// Failures run again to confirm them.
    pub reruns: std::sync::atomic::AtomicU64,
    /// Shrink candidates and the runs that confirm a minimal schedule.
    pub shrinks: std::sync::atomic::AtomicU64,
    /// Any other run (probes, traces, branches).
    pub other: std::sync::atomic::AtomicU64,
    stop: std::sync::atomic::AtomicBool,
}

impl Progress {
    /// Count one record of the stream (a run counts; other records do not).
    pub fn count(&self, rec: &Value) {
        use std::sync::atomic::Ordering::Relaxed;
        if rec.get("type").and_then(Value::as_str) != Some("run") {
            return;
        }
        let role = rec.get("role").and_then(Value::as_str).unwrap_or("first");
        let ok = rec
            .pointer("/verdict/kind")
            .and_then(Value::as_str)
            .is_none_or(|k| k == "ok");
        match role {
            hydo_results::role::FIRST | hydo_results::role::MANUAL => {
                self.first.fetch_add(1, Relaxed);
                if !ok {
                    self.failed.fetch_add(1, Relaxed);
                }
            }
            hydo_results::role::RERUN => {
                self.reruns.fetch_add(1, Relaxed);
            }
            hydo_results::role::SHRINK
            | hydo_results::role::MINIMAL
            | hydo_results::role::ERROR_SHRINK
            | hydo_results::role::ERROR_MINIMAL => {
                self.shrinks.fetch_add(1, Relaxed);
            }
            _ => {
                self.other.fetch_add(1, Relaxed);
            }
        }
    }

    /// The progress line after `secs` seconds.
    pub fn line(&self, secs: u64) -> String {
        use std::sync::atomic::Ordering::Relaxed;
        let n = |a: &std::sync::atomic::AtomicU64| a.load(Relaxed);
        let plural =
            |k: u64, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
        let mut parts = vec![format!(
            "{} run ({} failed)",
            plural(n(&self.first), "schedule", "schedules"),
            n(&self.failed)
        )];
        if n(&self.reruns) > 0 {
            parts.push(plural(
                n(&self.reruns),
                "replay of a failure",
                "replays of failures",
            ));
        }
        if n(&self.shrinks) > 0 {
            parts.push(plural(n(&self.shrinks), "shrink run", "shrink runs"));
        }
        if n(&self.other) > 0 {
            parts.push(plural(n(&self.other), "other run", "other runs"));
        }
        let took = match (secs / 3600, secs / 60 % 60, secs % 60) {
            (0, 0, s) => format!("{s} s"),
            (0, m, s) => format!("{m} min {s} s"),
            (h, m, _) => format!("{h} h {m} min"),
        };
        format!(
            "hydo explore: {took} in: {}",
            hydo_schedule::describe::and_list(&parts)
        )
    }
}

impl Events {
    /// No stream (records are dropped).
    pub fn none() -> Events {
        Events {
            f: Mutex::new(None),
            progress: Default::default(),
            ticker: None,
        }
    }

    pub fn create(path: &Path) -> Events {
        let f = std::fs::File::create(path)
            .map_err(|e| eprintln!("hydo-explore: {}: {e}", path.display()))
            .ok();
        let progress: std::sync::Arc<Progress> = Default::default();
        let p = progress.clone();
        let ticker = std::thread::Builder::new()
            .name("explore-progress".into())
            .spawn(move || {
                let t0 = std::time::Instant::now();
                let mut said = 0;
                while !p.stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::park_timeout(std::time::Duration::from_millis(250));
                    let secs = t0.elapsed().as_secs();
                    if secs / PROGRESS_EVERY_S > said {
                        said = secs / PROGRESS_EVERY_S;
                        eprintln!("{}", p.line(said * PROGRESS_EVERY_S));
                    }
                }
            })
            .ok();
        Events {
            f: Mutex::new(f),
            progress,
            ticker,
        }
    }

    pub fn write<T: serde::Serialize>(&self, rec: &T) {
        if let Some(f) = self.f.lock().expect("lock").as_mut() {
            let _ = f.write_all(hydo_results::to_line(rec).as_bytes());
            let _ = f.flush();
        }
    }
}

impl Events {
    /// Count one run for the progress line, when it is made: a shrink's runs reach the stream
    /// only after the shrink.
    pub fn counted(&self, run: &Run) {
        if self.ticker.is_some() {
            if let Ok(v) = serde_json::to_value(run) {
                self.progress.count(&v);
            }
        }
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        self.progress
            .stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.ticker.take() {
            t.thread().unpark();
            let _ = t.join();
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
    if !c.messages.is_empty() {
        res.insert(
            MESSAGES.into(),
            Value::Array(
                c.messages
                    .iter()
                    .filter_map(|j| serde_json::from_str::<Value>(j).ok())
                    .collect(),
            ),
        );
    }
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

/// The host controller's `HYDO-CHECK-OUT {"id":..,"lines":[..],"omitted":n}` notes in a run's
/// consoles (Milestone 225): the output of each failing check before its last line, bounded.
/// The first note of an id wins.
pub fn check_outputs(text: &str) -> BTreeMap<String, (Vec<Value>, u64)> {
    let mut out = BTreeMap::new();
    for l in text.split('\n') {
        let Some(i) = l.find("HYDO-CHECK-OUT {") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&l[i + "HYDO-CHECK-OUT ".len()..]) else {
            continue;
        };
        let (Some(id), Some(lines)) = (
            v.get("id").and_then(Value::as_str),
            v.get("lines").and_then(Value::as_array),
        ) else {
            continue;
        };
        let omitted = v.get("omitted").and_then(Value::as_u64).unwrap_or(0);
        out.entry(id.to_string())
            .or_insert_with(|| (lines.clone(), omitted));
    }
    out
}

/// The host controller's `HYDO-UPGRADE {...}` notes in a run's consoles (Milestone 238): each
/// switch of a service to another version, in order (`Run.upgrades`).
pub fn upgrades_of(text: &str) -> Vec<hydo_results::UpgradeSwitch> {
    text.split('\n')
        .filter_map(|l| {
            let i = l.find("HYDO-UPGRADE {")?;
            serde_json::from_str(&l[i + "HYDO-UPGRADE ".len()..]).ok()
        })
        .collect()
}

/// The failing checks' output (results addition of Milestone 225): an invariant or assertion
/// finding whose id has a `HYDO-CHECK-OUT` note gets `details.output` (the lines) and, when
/// lines were left out, `details.output_omitted`.
pub fn attach_check_outputs(findings: &mut [Finding], text: &str) {
    let outs = check_outputs(text);
    if outs.is_empty() {
        return;
    }
    for f in findings.iter_mut() {
        if f.kind != kind::INVARIANT && f.kind != kind::ASSERTION {
            continue;
        }
        if let Some((lines, omitted)) = outs.get(&f.id) {
            f.details
                .insert("output".into(), Value::Array(lines.clone()));
            if *omitted > 0 {
                f.details.insert("output_omitted".into(), json!(omitted));
            }
        }
    }
}

/// A run's result as a record. `consoles` = (path relative to the document's directory, text)
/// of the run's consoles, the run's own first.
pub fn run_of(tag: &str, text: &str, r: &Res, consoles: &[(String, String)]) -> Run {
    let verdict = verdict_of(r);
    let mut findings = Vec::new();
    findings.extend(invariant_finding(&guest_verdict(r)));
    let texts: Vec<String> = consoles.iter().map(|c| c.1.clone()).collect();
    findings.extend(assertion_findings(&merged_text(&texts, r)));
    attach_check_outputs(&mut findings, &texts.concat());
    if let Some(Value::Array(fs)) = r.get(CHECKS) {
        findings.extend(
            fs.iter()
                .filter_map(|f| serde_json::from_value::<Finding>(f.clone()).ok()),
        );
    }
    for key in [CONTRACTS, VALUES] {
        let Some(Value::Array(fs)) = r.get(key) else {
            continue;
        };
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
    let mut warnings = if verdict.kind == "ok" {
        vacuous(consoles, &findings)
    } else {
        Vec::new()
    };
    // Milestone 368: a console that the engine cut (hvx keeps 64 KiB of a VM's) comes first
    let mut cut = console_cut_warnings(consoles);
    cut.append(&mut warnings);
    let mut warnings = cut;
    let unseen = unseen_events(text, consoles);
    // Milestone 355: a reaction that the controller noted as never fired, unless the line of
    // unseen_events already says that no record names its event
    warnings.extend(
        unfired_reactions(consoles)
            .into_iter()
            .filter(|(tok, _)| {
                !unseen
                    .iter()
                    .any(|u| u.contains(&format!("reaction {tok} ")))
            })
            .map(|(_, w)| w),
    );
    warnings.extend(unseen);
    warnings.extend(failures_of_unmatched(consoles));
    let messages: Vec<hydo_results::MessagesCheck> = match r.get(MESSAGES) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| serde_json::from_value(x.clone()).ok())
            .collect(),
        _ => Vec::new(),
    };
    Run {
        tag: Some(tag.to_string()),
        messages,
        upgrades: upgrades_of(&texts.concat()),
        paths: crate::paths::of_res(r),
        functions: crate::funcs::of_res(r, text),
        schedule: text.to_string(),
        warnings,
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

/// Why a passing run may have checked nothing (no silent passes, 2026-11-07): a pass of a run
/// whose workload acknowledged nothing, or whose history check judged an empty history, is not
/// evidence that the system is correct; a broken workload (a client that cannot connect, a
/// service that never started) passes the same way. One sentence per reason.
///
/// - The workload acknowledged nothing: the first boot's last `hydo-explore-acked:` report has
///   no id, no composer command exited 0 (`compose.<id>.succeeded`), no Playwright test of
///   `browser:` passed (its `hydo-web: test "..." passed` line, Milestone 254), and no history
///   check saw an operation. A run without that report (it ended before the workload's end) is
///   not judged.
/// - A history check that passed with no operation (`ops` 0 in its details).
/// - A messages claim that passed with nothing to judge (`judged` 0), unless another claim of
///   the same check judged operations (Milestone 350).
pub fn vacuous(consoles: &[(String, String)], findings: &[Finding]) -> Vec<String> {
    let mut out = Vec::new();
    let ops = |f: &Finding| {
        f.details
            .get("details")
            .and_then(|d| d.get("ops"))
            .and_then(Value::as_u64)
    };
    let hist: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.kind == kind::HISTORY_CHECK)
        .collect();
    let first = consoles
        .iter()
        .find(|(name, _)| !name.contains(".recovery"))
        .map(|(_, t)| t.as_str())
        .unwrap_or("");
    let acked = first
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("hydo-explore-acked:"))
        .map(|l| {
            l.split_whitespace()
                .filter(|w| !w.starts_with("clock=") && !w.starts_with("w:"))
                .count()
        });
    let composed = consoles.iter().any(|(_, t)| {
        t.lines().any(|l| {
            l.starts_with("HYDO-ASSERT ")
                && l.contains("\"id\":\"compose.")
                && l.contains(".succeeded\"")
                && l.contains("\"ok\":true")
        })
    });
    // `browser:` (Milestone 254): a passing Playwright test is the workload's acknowledged work
    let tested = consoles
        .iter()
        .any(|(_, t)| t.lines().any(browser_test_passed));
    let hist_ops = hist.iter().any(|f| ops(f).is_some_and(|n| n > 0));
    if acked == Some(0) && !composed && !tested && !hist_ops {
        out.push(
            "the workload acknowledged nothing: no id in $HYDO_ACKED, no test command exited 0, \
             no history operation; this pass checked nothing the workload did (a client that \
             cannot reach a service passes the same way)"
                .to_string(),
        );
    }
    // a messages check whose claims judged operations (Milestone 350): another claim of the
    // check that had nothing to judge does not make the pass vacuous. Garage's `acked_outcome`
    // check judged about 80 operations a run, and its implicit `no-phantom` judged none, because
    // the client records no deliveries
    let judged = |f: &Finding| {
        f.details
            .get("details")
            .and_then(|d| d.get("judged"))
            .and_then(Value::as_u64)
    };
    let check_of = |f: &Finding| -> Option<String> {
        let claim = f
            .details
            .get("details")
            .and_then(|d| d.get("claim"))
            .and_then(Value::as_str)?;
        f.id.strip_suffix(claim)
            .and_then(|c| c.strip_suffix('.'))
            .map(String::from)
    };
    let judging: HashSet<String> = hist
        .iter()
        .filter(|f| judged(f).is_some_and(|n| n > 0))
        .filter_map(|f| check_of(f))
        .collect();
    for f in hist {
        if f.status == status::OK && ops(f) == Some(0) {
            out.push(format!(
                "the history check {} judged an empty history (0 operations): nothing was \
                 recorded with hydo_hist",
                f.id
            ));
        } else if f.status == status::OK {
            // a messages claim that had nothing to judge holds vacuously
            let claim = check_of(f);
            if claim.is_some()
                && judged(f) == Some(0)
                && !claim.as_ref().is_some_and(|c| judging.contains(c))
            {
                out.push(format!(
                    "the messages claim {} had nothing to judge: it holds vacuously",
                    f.id
                ));
            }
        }
    }
    out
}

/// What `check.mjs failures` adds to its held sentence when `browser.watch.failures_of` matched
/// none of the requests the pages sent (Milestone 264).
pub const FAILURES_OF_UNMATCHED: &str = "; but watch.failures_of (";

/// A page check that judged no failed request because `browser.watch.failures_of` matched none
/// of the requests the pages sent (Milestone 264: Memos's first pattern did so for 60 schedules,
/// and nothing said it). One warning per check, from its held record.
pub fn failures_of_unmatched(consoles: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (_, t) in consoles {
        for (id, said) in last_held(t) {
            if let Some(i) = said.find(FAILURES_OF_UNMATCHED) {
                let w = format!(
                    "the page check {id} judged no failed request: {}; fix the pattern in \
                     hydo.yaml (browser.watch.failures_of)",
                    &said[i + "; but ".len()..]
                );
                if !out.contains(&w) {
                    out.push(w);
                }
            }
        }
    }
    out
}

/// Whether a console line is the `browser:` reporter's line of a passing test:
/// `hydo-web: test "<title>" passed in <s> s` (hydo-manifest's `browser/reporter.cjs`).
pub fn browser_test_passed(l: &str) -> bool {
    l.strip_prefix("hydo-web: test \"")
        .and_then(|r| r.rsplit_once("\" "))
        .is_some_and(|(_, st)| st.starts_with("passed in "))
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
    // Milestone 394: the functions only on the first runs, which the live map reads; a shrink
    // run's thousands of functions would make the stream ten times as large
    if role != hydo_results::role::FIRST && role != hydo_results::role::MANUAL {
        run.functions = None;
    }
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
    /// eventually: the largest `within_s` of its records (-1: none)
    max_within: f64,
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
            max_within: -1.0,
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
        if p.ty == b"eventually" {
            if let Some(e) = find(rec, b"\"within_s\":") {
                let x = &rec[e + 11..];
                let w = atof(if x.first() == Some(&b'"') { &x[1..] } else { x });
                if w > p.max_within {
                    p.max_within = w;
                }
            }
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
            // Milestone 360: a record longer than the C header's line keeps its type, id and
            // outcome. The keys of hydo's own records are sorted, so `details` comes first, and a
            // violation's long details cut `id`, `ok` and `t` off: the table counted the claims
            // that held and dropped most that broke (the trial of Milestone 359 read
            // `jobs.acked-outcome 13 0 PASS` beside a run that broke it).
            let short;
            let raw = if raw.len() >= LINE_LEN - 1 {
                match long_record(raw) {
                    Some(s) => {
                        short = s;
                        short.as_slice()
                    }
                    None => raw,
                }
            } else {
                raw
            };
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

/// A HYDO-ASSERT or HYDO-ASSERT-DECL line longer than [`LINE_LEN`], written again with its
/// `details` last, so that the line's cut at [`LINE_LEN`] keeps every other key. None when the
/// record is not one JSON object (such a line is read as before).
fn long_record(raw: &[u8]) -> Option<Vec<u8>> {
    let (mark, i) = match find(raw, b"HYDO-ASSERT-DECL ") {
        Some(i) => ("HYDO-ASSERT-DECL ", i),
        None => ("HYDO-ASSERT ", find(raw, b"HYDO-ASSERT ")?),
    };
    let body = std::str::from_utf8(&raw[i + mark.len()..]).ok()?;
    let Ok(serde_json::Value::Object(o)) = serde_json::from_str::<serde_json::Value>(body.trim())
    else {
        return None;
    };
    let mut parts: Vec<String> = Vec::new();
    for (k, v) in o.iter().filter(|(k, _)| k.as_str() != "details") {
        parts.push(format!("{}:{v}", serde_json::Value::String(k.clone())));
    }
    if let Some(d) = o.get("details") {
        parts.push(format!("\"details\":{d}"));
    }
    let mut out = raw[..i].to_vec();
    out.extend_from_slice(mark.as_bytes());
    out.extend_from_slice(format!("{{{}}}", parts.join(",")).as_bytes());
    Some(out)
}

fn verdict(p: &P) -> &'static str {
    property_verdict(&String::from_utf8_lossy(&p.ty), p.passes, p.fails)
}

/// The property table's verdict of a property of one assertion type over its evaluations that
/// held and that did not (also how `hydo explore --merge` judges a property of several parts).
pub fn property_verdict(assertion: &str, passes: u64, fails: u64) -> &'static str {
    match assertion {
        "always" => {
            if fails > 0 {
                "FAIL"
            } else if passes > 0 {
                "PASS"
            } else {
                "FAIL (never evaluated)"
            }
        }
        "always_or_unreachable" => {
            if fails > 0 {
                "FAIL"
            } else {
                "PASS"
            }
        }
        "unreachable" => {
            if fails > 0 {
                "FAIL (reached)"
            } else {
                "PASS"
            }
        }
        "eventually" => {
            if fails > 0 {
                "FAIL (deadline missed)"
            } else if passes > 0 {
                "PASS"
            } else {
                "NOT EVALUATED"
            }
        }
        _ => {
            if passes > 0 {
                "PASS"
            } else {
                "NOT EXERCISED"
            }
        }
    }
}

/// A property table verdict as a [`status`].
pub fn property_status(verdict: &str) -> &'static str {
    if verdict.starts_with("FAIL") {
        status::VIOLATION
    } else if verdict == "NOT EXERCISED" {
        status::NOT_EXERCISED
    } else if verdict == "NOT EVALUATED" {
        status::NOT_EVALUATED
    } else {
        status::OK
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// What each property's last evaluation that held said, from one run's consoles: (id, its
/// `details.out`, else its details as JSON), in the order the ids first held. A check file's
/// `details.out` is its last output line, so a passing run's report can show what each check
/// judged (Milestone 239).
pub fn last_held(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for l in text.lines() {
        let Some(i) = l.find("HYDO-ASSERT {") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&l[i + 12..]) else {
            continue;
        };
        if v.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            continue;
        }
        let (Some(id), Some(d)) = (v.get("id").and_then(|x| x.as_str()), v.get("details")) else {
            continue;
        };
        let said = match d.get("out").and_then(|x| x.as_str()) {
            Some(s) => s.trim().to_string(),
            None if d.is_null() => continue,
            None => d.to_string(),
        };
        if said.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(k, _)| k == id) {
            Some(x) => x.1 = said,
            None => out.push((id.to_string(), said)),
        }
    }
    out
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
    let props: Vec<Property> = t
        .p
        .iter()
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
                status: property_status(v).into(),
                first_failure: (!p.first_fail.is_empty()).then(|| lossy(&p.first_fail)),
                slowest_ok_s: (p.ty == b"eventually" && p.max_ok_elapsed >= 0.0)
                    .then_some(p.max_ok_elapsed),
                within_s: (p.ty == b"eventually" && p.max_within >= 0.0).then_some(p.max_within),
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
                slow = match p.within_s {
                    Some(w) => format!("  [slowest in time: {s:.2} s of {w} s]"),
                    None => format!("  [slowest in time: {s:.2} s]"),
                };
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
            // a property no SDK declared: a check or invariant the project wrote (before
            // Milestone 239 `[not in catalog]`, which read like an error)
            if p.declared {
                ""
            } else {
                "  [the project's own check]"
            }
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
            for (ok, el, det) in &eps {
                let w = match det.get("within_s") {
                    Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
                    Some(v) => v.as_f64(),
                    None => None,
                };
                if let Some(w) = w {
                    if p.within_s.is_none_or(|x| w > x) {
                        p.within_s = Some(w);
                    }
                }
                // satisfied after the deadline: the host's "satisfied only <t>s after <anchor>"
                let late = !ok
                    && det
                        .get("out")
                        .and_then(Value::as_str)
                        .is_some_and(|o| o.contains("satisfied only "));
                if late && p.slowest_late_s.is_none_or(|s| *el > s) {
                    p.slowest_late_s = Some(*el);
                    p.slowest_late_run = Some(stem.clone());
                }
            }
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
        "## Liveness (eventually properties, hydo guide sdks)".to_string(),
        String::new(),
        "| property | held in time | missed deadline | slowest in time | deadline | slowest held late | first miss |".into(),
        "|---|---|---|---|---|---|---|".into(),
    ];
    for p in l {
        let slow = match (p.slowest_s, &p.slowest_run) {
            (Some(w), Some(s)) => format!("{w:.2} s ({s})"),
            _ => "-".into(),
        };
        let within = p
            .within_s
            .map(|w| format!("{w} s"))
            .unwrap_or_else(|| "-".into());
        let late = match (p.slowest_late_s, &p.slowest_late_run) {
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
            "| `{}` | {} | {} | {slow} | {within} | {late} | {miss} |",
            p.id, p.held_runs, p.missed_runs
        ));
    }
    out.push(String::new());
    out
}

/// Reactions (`A=<event>:...`) whose event no assertion record of the run names: the event is an
/// assertion id only the running program declares, so a misspelt one cannot be refused before
/// the run; it never fires, and the run is the run without that reaction. One sentence each.
pub fn unseen_events(schedule: &str, consoles: &[(String, String)]) -> Vec<String> {
    let Ok(s) = hydo_schedule::Schedule::parse(schedule) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for t in &s.guest {
        let hydo_schedule::GuestToken::React(r) = t else {
            continue;
        };
        let needle = format!("\"id\":\"{}\"", r.event);
        let seen = consoles.iter().any(|(_, c)| {
            c.lines().any(|l| {
                (l.starts_with("HYDO-ASSERT ") || l.starts_with("HYDO-ASSERT-DECL "))
                    && l.contains(&needle)
            })
        });
        if !seen {
            out.push(format!(
                "the reaction {t} never fired: no assertion record of the run names the event {} \
                 (a misspelt event id is not refused before the run)",
                r.event
            ));
        }
    }
    out
}

/// Milestone 368: one warning per console that ends with a `HYDO-CONSOLE-CUT` note (hvx keeps
/// 64 KiB of a VM's console and cut the rest). The words name the console and what is lost.
pub fn console_cut_warnings(consoles: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (name, c) in consoles {
        if c.lines().any(|l| l.starts_with("HYDO-CONSOLE-CUT ")) {
            let boot = if name.contains(".recovery") {
                "the recovery boot's console"
            } else {
                "the console"
            };
            out.push(format!(
                "{boot} ({name}) was cut at 64 KiB: the hvx engine keeps the first 65536 bytes of \
                 a VM's console, so the records written after them (HYDO-HIST and HYDO-ASSERT too) \
                 are missing, and a check of those records judged only the first part. Print less \
                 (short ids, one history record per batch), or run on the KVM engine \
                 (--backend kvm), which keeps 32 MiB"
            ));
        }
    }
    out
}

/// Whether a run's warning is one of [`console_cut_warnings`].
pub fn is_console_cut_warning(w: &str) -> bool {
    w.contains("was cut at 64 KiB: the hvx engine keeps")
}

/// Milestone 355: the reactions that the controller noted as never fired (its
/// `HYDO-REACT-UNFIRED {...}` notes), each as its token and one warning in words. The first
/// note of a token wins.
pub fn unfired_reactions(consoles: &[(String, String)]) -> Vec<(String, String)> {
    const PRE: &str = "HYDO-REACT-UNFIRED ";
    let mut out: Vec<(String, String)> = Vec::new();
    for (_, c) in consoles {
        for l in c.lines() {
            let Some(i) = l.find("HYDO-REACT-UNFIRED {") else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&l[i + PRE.len()..]) else {
                continue;
            };
            let tok = v.get("token").and_then(Value::as_str).unwrap_or("A=?");
            if out.iter().any(|(t, _)| t == tok) {
                continue;
            }
            out.push((tok.to_string(), unfired_words(&v)));
        }
    }
    out
}

/// Whether a run's warning is one of a reaction that never fired ([`unfired_reactions`] or
/// [`unseen_events`]): the explorer counts such runs apart from the passes that checked nothing.
pub fn is_unfired_warning(w: &str) -> bool {
    w.starts_with("the reaction ") && w.contains(" never fired")
}

/// One [`unfired_reactions`] note in words.
fn unfired_words(v: &Value) -> String {
    let tok = v.get("token").and_then(Value::as_str).unwrap_or("A=?");
    let ev = v.get("event").and_then(Value::as_str).unwrap_or("?");
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let (nth, at, seen) = (n("nth").max(1), n("at"), n("seen"));
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    let armed_at = if at == 0 {
        "from the start of the workload".to_string()
    } else {
        format!("before step {at}")
    };
    let why = if !b("armed") {
        if b("cut") {
            format!("a power cut ended the steps before step {at}, where it is armed")
        } else {
            format!("the run ended before step {at}, where it is armed")
        }
    } else if seen >= nth {
        "its event came only as the steps ended, when the reactions stop".to_string()
    } else {
        let came = match seen {
            0 => format!("no record of its event {ev} came after it was armed {armed_at}"),
            1 => format!("its event {ev} came once after it was armed {armed_at}"),
            k => format!("its event {ev} came {k} times after it was armed {armed_at}"),
        };
        let want = if nth > 1 {
            format!(", and it acts at record {nth} of the event")
        } else {
            String::new()
        };
        let end = if b("cut") {
            ", before a power cut ended the steps"
        } else {
            ""
        };
        format!("{came}{want}{end}")
    };
    format!("the reaction {tok} never fired: {why}; the run did not test it")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passing_check_s_last_line_is_kept() {
        let text = "boot\n\
            HYDO-ASSERT {\"t\":\"always\",\"id\":\"db.acked\",\"ok\":true,\"details\":{\"out\":\"acked=3 all present\"}}\n\
            x HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"fault.paused\",\"ok\":true}\n\
            HYDO-ASSERT {\"t\":\"always\",\"id\":\"db.sum\",\"ok\":false,\"details\":{\"out\":\"sum 4 of 5\"}}\n\
            agent: \"HYDO-ASSERT {\\\"t\\\":\\\"always\\\",\\\"id\\\":\\\"db.acked\\\",\\\"ok\\\":true}\"\n\
            HYDO-ASSERT {\"t\":\"always\",\"id\":\"db.acked\",\"ok\":true,\"details\":{\"out\":\"acked=20 all present\"}}\n\
            HYDO-ASSERT {\"t\":\"sometimes\",\"id\":\"sdk.x\",\"ok\":true,\"details\":{\"n\":2}}\n";
        assert_eq!(
            last_held(text),
            [
                ("db.acked".to_string(), "acked=20 all present".to_string()),
                ("sdk.x".to_string(), "{\"n\":2}".to_string())
            ]
        );
    }

    /// Milestone 360: a host check's record whose sorted keys put long `details` first counts
    /// as the violation it is (the trial of Milestone 359 read `13 0 PASS` for a broken claim).
    #[test]
    fn a_long_violation_record_counts_as_a_failure() {
        let rec = |ok: bool, out: &str| {
            format!(
                "HYDO-ASSERT {{\"details\":{{\"check\":\"messages\",\"out\":\"{out}\"}},\"id\":\"jobs.acked-outcome\",\"msg\":\"messages history jobs\",\"ok\":{ok},\"t\":\"always\"}}\n"
            )
        };
        let long = "m0 was acknowledged and has no final state; ".repeat(40);
        let runs = vec![
            rec(true, "held"),
            rec(false, &long),
            rec(false, "short"),
            "boot only\n".to_string(),
        ];
        assert!(rec(false, &long).len() > LINE_LEN);
        let t = property_table(&runs).unwrap();
        let p = t
            .properties
            .iter()
            .find(|p| p.id == "jobs.acked-outcome")
            .unwrap();
        assert_eq!((p.passes, p.fails, p.runs_failed), (1, 2, 2), "{p:?}");
        assert_eq!(p.verdict, "FAIL");
        assert!(p
            .first_failure
            .as_deref()
            .unwrap()
            .starts_with("{\"check\""));
    }

    fn hist(ops: u64) -> Finding {
        let mut d = Map::new();
        d.insert("details".into(), serde_json::json!({ "ops": ops }));
        Finding {
            kind: kind::HISTORY_CHECK.into(),
            id: "kv".into(),
            status: status::OK.into(),
            message: String::new(),
            details: d,
        }
    }

    fn con(t: &str) -> Vec<(String, String)> {
        vec![("one.log".into(), t.into())]
    }

    /// Milestone 264: a page check whose `watch.failures_of` matched no request warns, once.
    #[test]
    fn a_failures_of_that_matched_nothing_warns() {
        let held = |out: &str| {
            format!(
                "HYDO-ASSERT {{\"t\":\"always\",\"id\":\"browser.failures_shown\",\"ok\":true,\"msg\":\"m\",\"details\":{{\"out\":\"{out}\"}}}}\n"
            )
        };
        let ok = "every failed request was followed by an error on the page, or sent again, within 5.0 s";
        assert!(failures_of_unmatched(&con(&held(ok))).is_empty());
        let none = format!(
            "{ok}; but watch.failures_of (/[A-Za-z]+/(Create|Update)$) matched none of the 41 \
             requests the pages sent (such as POST /memos.api.MemoService/CreateMemo), so no \
             failed request was judged"
        );
        let w = failures_of_unmatched(&con(&format!("{}{}", held(&none), held(&none))));
        assert_eq!(
            w,
            [
                "the page check browser.failures_shown judged no failed request: \
              watch.failures_of (/[A-Za-z]+/(Create|Update)$) matched none of the 41 requests the \
              pages sent (such as POST /memos.api.MemoService/CreateMemo), so no failed request \
              was judged; fix the pattern in hydo.yaml (browser.watch.failures_of)"
            ]
        );
        // a run's warnings carry it, whatever its verdict
        let r = run_of(
            "t",
            "disk=none | run",
            serde_json::json!({"verdict": "HYDO-ORACLE: OK invariants=1 held"})
                .as_object()
                .unwrap(),
            &con(&held(&none)),
        );
        assert!(r.warnings.contains(&w[0]), "{:?}", r.warnings);
    }

    #[test]
    fn a_pass_with_nothing_acknowledged_or_an_empty_history_is_flagged() {
        // nothing acknowledged, no command succeeded, no history
        let w = vacuous(&con("hydo-explore-acked: \nHYDO-ORACLE: OK\n"), &[]);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].starts_with("the workload acknowledged nothing"),
            "{w:?}"
        );
        // the world's records and the clock are not ids
        let w = vacuous(&con("hydo-explore-acked: clock=17 w:abc\n"), &[]);
        assert_eq!(w.len(), 1);
        // acknowledged ids, a test command that exited 0, or history operations: not flagged
        assert!(vacuous(&con("hydo-explore-acked: 0 1 2\n"), &[]).is_empty());
        let ok = "hydo-explore-acked: \nHYDO-ASSERT {\"t\":\"sometimes\",\
                  \"id\":\"compose.parallel_driver_set.succeeded\",\"ok\":true}\n";
        assert!(vacuous(&con(ok), &[]).is_empty());
        assert!(vacuous(&con("hydo-explore-acked: \n"), &[hist(4)]).is_empty());
        // a Playwright test that passed (browser:, Milestone 254) is acknowledged work; a
        // failed or skipped one is not
        let web = |st: &str| {
            format!("hydo-web: test \"shop.spec.ts › lists the products\" {st} in 0.4 s\nhydo-explore-acked: \n")
        };
        assert!(vacuous(&con(&web("passed")), &[]).is_empty());
        assert_eq!(vacuous(&con(&web("failed")), &[]).len(), 1);
        assert_eq!(vacuous(&con(&web("skipped")), &[]).len(), 1);
        // a history check over an empty history
        let w = vacuous(&con("hydo-explore-acked: 1\n"), &[hist(0)]);
        assert_eq!(
            w,
            vec![
                "the history check kv judged an empty history (0 operations): nothing was \
                  recorded with hydo_hist"
                    .to_string()
            ]
        );
        // a run that ended before the workload's end is not judged
        assert!(vacuous(&con("HYDO-ORACLE: ERROR provision failed\n"), &[]).is_empty());
    }

    /// Milestone 350: a messages check is one finding per claim. A claim with nothing to judge
    /// makes the pass vacuous only when no claim of the same check judged an operation: Garage's
    /// `acked-outcome` judged about 80 operations a run while `no-phantom` judged none, and every
    /// passing run counted as vacuous.
    #[test]
    fn a_messages_claim_with_nothing_to_judge_beside_one_that_judged_is_no_vacuous_pass() {
        let claim = |check: &str, name: &str, judged: u64| {
            let mut d = Map::new();
            d.insert(
                "details".into(),
                serde_json::json!({"ops": 82, "claim": name, "judged": judged, "undecided": 0}),
            );
            Finding {
                kind: kind::HISTORY_CHECK.into(),
                id: format!("{check}.{name}"),
                status: status::OK.into(),
                message: String::new(),
                details: d,
            }
        };
        let acked = con("hydo-explore-acked: 1 2\n");
        let garage = [
            claim("objects", "no-phantom", 0),
            claim("objects", "acked-outcome", 80),
        ];
        assert!(vacuous(&acked, &garage).is_empty());
        // every claim of the check judged nothing: vacuous, one sentence per claim
        let none = [
            claim("objects", "no-phantom", 0),
            claim("objects", "acked-outcome", 0),
        ];
        assert_eq!(vacuous(&acked, &none).len(), 2);
        // a claim of another check that judged operations does not cover this one
        let other = [
            claim("objects", "no-phantom", 0),
            claim("queue", "no-loss", 12),
        ];
        assert_eq!(
            vacuous(&acked, &other),
            ["the messages claim objects.no-phantom had nothing to judge: it holds vacuously"]
        );
    }

    #[test]
    fn a_reaction_whose_event_no_record_names_is_flagged() {
        let s = "disk=none | run A=client.place_faild:kill-redis";
        let rec = "HYDO-ASSERT {\"id\":\"client.place_failed\",\"ok\":true}\n";
        let w = unseen_events(s, &con(rec));
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("client.place_faild"), "{w:?}");
        let s = "disk=none | run A=client.place_failed:kill-redis";
        let decl = "HYDO-ASSERT-DECL {\"t\":\"sometimes\",\"id\":\"client.place_failed\"}\n";
        assert!(unseen_events(s, &con(decl)).is_empty());
        assert!(unseen_events("disk=none | run K=redis@3+2", &con("")).is_empty());
    }

    /// Milestone 355: the controller's note of a reaction that never fired is a warning in
    /// words, once per token.
    #[test]
    fn a_reaction_that_never_fired_is_named() {
        let note = |armed: bool, seen: u64, nth: u64, cut: bool| {
            let v = serde_json::json!({"token": "A=kv.failover_done@4+5500:cut",
                "event": "kv.failover_done", "nth": nth, "at": 4, "armed": armed,
                "seen": seen, "cut": cut});
            format!("hydo-ob: x\nHYDO-REACT-UNFIRED {v}\n")
        };
        let w = unfired_reactions(&con(&note(true, 0, 1, false)));
        assert_eq!(w.len(), 1);
        assert_eq!(
            w[0].1,
            "the reaction A=kv.failover_done@4+5500:cut never fired: no record of its event \
             kv.failover_done came after it was armed before step 4; the run did not test it"
        );
        assert!(is_unfired_warning(&w[0].1));
        let w = unfired_reactions(&con(&note(true, 2, 3, true)));
        assert!(
            w[0].1.contains(
                "came 2 times after it was armed before step 4, and it acts at record 3 of the \
                 event, before a power cut ended the steps"
            ),
            "{w:?}"
        );
        let w = unfired_reactions(&con(&note(false, 0, 1, false)));
        assert!(
            w[0].1
                .contains("the run ended before step 4, where it is armed"),
            "{w:?}"
        );
        // one note per token, also when another console repeats it
        let mut two = con(&note(true, 0, 1, false));
        two.push(("one.recovery.log".into(), note(true, 0, 1, false)));
        assert_eq!(unfired_reactions(&two).len(), 1);
        // in a run's record: one warning
        let text = "disk=none | run A=kv.failover_done@4+5500:cut";
        let r: crate::pool::Res =
            serde_json::from_str(r#"{"verdict":"OK","fp":"0"}"#).expect("json");
        let mut c = con(&note(true, 0, 1, false));
        c[0].1.push_str(
            "HYDO-ASSERT-DECL {\"t\":\"sometimes\",\"id\":\"kv.failover_done\"}\n\
             hydo-explore-acked: 1\n",
        );
        let run = run_of("seed-1", text, &r, &c);
        let unfired: Vec<&String> = run
            .warnings
            .iter()
            .filter(|w| is_unfired_warning(w))
            .collect();
        assert_eq!(unfired.len(), 1, "{:?}", run.warnings);
        // a misspelt event keeps the line of unseen_events alone
        let text = "disk=none | run A=kv.failover_dne@4+5500:cut";
        let v = serde_json::json!({"token": "A=kv.failover_dne@4+5500:cut",
            "event": "kv.failover_dne", "nth": 1, "at": 4, "armed": true, "seen": 0, "cut": false});
        let c = con(&format!("HYDO-REACT-UNFIRED {v}\n"));
        let run = run_of("seed-1", text, &r, &c);
        let unfired: Vec<&String> = run
            .warnings
            .iter()
            .filter(|w| is_unfired_warning(w))
            .collect();
        assert_eq!(unfired.len(), 1, "{:?}", run.warnings);
        assert!(unfired[0].contains("misspelt"), "{unfired:?}");
    }
}
