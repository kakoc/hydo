//! What the explorer reads back from a run (its HYDO-ASSERT records, its failed properties) and
//! the verdict helpers the reports use. The property table and the liveness summary are in
//! [`crate::results`].

use crate::pool::Res;
use crate::py::{float_repr, py_str};
use regex::Regex;
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::OnceLock;

fn assert_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"HYDO-ASSERT (\{.*\})").expect("pattern"))
}

/// The HYDO-ASSERT records of a text, one per line that has one (lines that do not parse are skipped).
pub fn records(text: &str) -> Vec<Map<String, Value>> {
    text.split('\n')
        .filter_map(|l| assert_re().captures(l))
        .filter_map(|c| serde_json::from_str::<Value>(&c[1]).ok())
        .filter_map(|v| v.as_object().cloned())
        .collect()
}

/// One explorer run's consoles and its host-checked records as one text.
pub fn merged(outdir: &Path, tag: &str, r: &Res) -> String {
    let texts: Vec<String> = crate::results::explorer_consoles(outdir, tag)
        .into_iter()
        .map(|c| c.1)
        .collect();
    crate::results::merged_text(&texts, r)
}

/// explore.py `assert_failures`: failed property evaluations in one run (its consoles and the
/// host's history-check records), sorted ids.
pub fn assert_failures(outdir: &Path, tag: &str, r: &Res) -> Vec<Value> {
    crate::results::assertion_findings(&merged(outdir, tag, r))
        .into_iter()
        .map(|f| Value::String(f.id))
        .collect()
}

/// Milestone 368: the first line of what the violated check said, with its numbers blanked as in
/// a [`signature`]: the first non-empty line of the check's `HYDO-CHECK-OUT` note in the run's
/// consoles, else the first line of a host check's explanation (`r`'s history checks), else the
/// verdict's own text. None for a verdict that is no violation. The shrinker compares it before
/// it keeps a shorter reaction delay: a delay that moves into another cause's window fails the
/// same check with other words (the NSQ trial, open item 184).
pub fn first_violation_text(
    verdict: &str,
    consoles: &[(String, String)],
    r: &Res,
) -> Option<String> {
    if verdict_kind(verdict) != "violation" {
        return None;
    }
    let id = verdict.split_once("VIOLATION [")?.1.split_once(']')?.0;
    let text: String = consoles.iter().map(|c| c.1.as_str()).collect();
    let first = |s: &str| {
        s.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| signature(l))
    };
    if let Some((lines, _)) = crate::results::check_outputs(&text).get(id) {
        let joined: Vec<&str> = lines.iter().filter_map(Value::as_str).collect();
        if let Some(l) = first(&joined.join("\n")) {
            return Some(l);
        }
    }
    let explained = r
        .get(crate::results::CHECKS)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|f| f.get("id").and_then(Value::as_str) == Some(id))
        .and_then(|f| f.pointer("/details/explanation"))
        .and_then(Value::as_str)
        .and_then(first);
    explained.or_else(|| Some(signature(verdict)))
}

/// explore.py `verdict_kind`.
pub fn verdict_kind(v: &str) -> &'static str {
    hydo_engine::protocol::verdict_kind(v)
}

/// explore.py `signature`: the verdict without its prefix, every number as '#'. In a violation
/// of a page invariant ([`page_violation`]), a generated id is `<id>` first (Milestone 264): a
/// page of one note and the same page of another (`/memos/b4R4VE2u3vE8gqW7sZTJtL`) are one
/// failure, so the shrinker can take a candidate whose walk made another note.
pub fn signature(v: &str) -> String {
    static D: OnceLock<Regex> = OnceLock::new();
    let v = v.replace("HYDO-ORACLE: ", "");
    let v = if page_violation(&v) { blank_ids(&v) } else { v };
    D.get_or_init(|| Regex::new(r"\d+").expect("p"))
        .replace_all(&v, "#")
        .into_owned()
}

/// Whether a verdict is the violation of one of `browser:`'s page invariants
/// (`VIOLATION [<browser>.no_page_errors] ...`, hydo-manifest's `browser` expansion): only their
/// signatures blank ids, so that every other finding keeps the signature it had.
pub fn page_violation(v: &str) -> bool {
    const PAGE: [&str; 7] = [
        "no_page_errors",
        "busy_ends",
        "failures_shown",
        "no_bad_text",
        "screens_match",
        "tests_pass",
        // Milestone 334: its sentence names the object's id, as `the task 4` or `the order o1`
        "acked_shown",
    ];
    verdict_kind(v) == "violation"
        && v.split_once("VIOLATION [")
            .and_then(|(_, r)| r.split_once(']'))
            .and_then(|(id, _)| id.rsplit_once('.'))
            .is_some_and(|(_, p)| PAGE.contains(&p))
}

/// Every generated id of a text as `<id>`: a token of letters and digits that the error
/// templates take for an id (`hydo_control::schedule::errlog::id_like`), or a token of sixteen
/// letters or more whose case changes at every third letter or more often, as an id without
/// digits does (`bRxVEuvEgqWsZTJtLk`; `UpsertMemoReaction` keeps its letters).
pub fn blank_ids(v: &str) -> String {
    static T: OnceLock<Regex> = OnceLock::new();
    let t = T.get_or_init(|| Regex::new(r"[A-Za-z0-9]+").expect("p"));
    t.replace_all(v, |c: &regex::Captures<'_>| {
        let w = c[0].as_bytes();
        let changes = w
            .windows(2)
            .filter(|p| p[0].is_ascii_uppercase() != p[1].is_ascii_uppercase())
            .count();
        let lettered = w.len() >= 16
            && w.iter().all(u8::is_ascii_alphabetic)
            && w.iter().any(u8::is_ascii_lowercase)
            && w.iter().any(u8::is_ascii_uppercase)
            && 3 * changes >= w.len();
        if hydo_control::schedule::errlog::id_like(w) || lettered {
            "<id>".to_string()
        } else {
            c[0].to_string()
        }
    })
    .into_owned()
}

/// The grouping key of a failure: the signature up to " order=" for violations.
pub fn sig_key(v: &str) -> String {
    let s = signature(v);
    if verdict_kind(v) == "violation" {
        s.split(" order=").next().unwrap_or("").to_string()
    } else {
        s
    }
}

/// explore.py `same_failure`.
pub fn same_failure(v_ref: &str, v: &str) -> bool {
    verdict_kind(v) == "violation" && sig_key(v) == sig_key(v_ref)
}

/// Whether the explorer shrinks a failure with this verdict, of a first run of this class
/// (Milestone 339). A violation always is. An error is when it is the system's own failure,
/// whose signature repeats:
///
/// - a service that does not start again: `ERROR <svc> restart failed`, with or without its
///   reason, and `ERROR restart after the faults`;
/// - a workload or a check that does not end: `TIMEOUT workload`, `TIMEOUT check`, `TIMEOUT
///   recovery`, and the Mac engine's `ERROR workload: exit budget exhausted` and `ERROR check:
///   exit budget exhausted`.
///
/// Every other error is hydo's or its engine's (a lost worker, a refused schedule, a guest that
/// could not be set up) and is not shrunk. A service that does not start again after hydo damaged
/// its files (`G=`, the class `corrupted-files`) or its disk's blocks (the class `damaged-disk`),
/// or whose restart names `its files were corrupted`, is the system's documented detection of
/// the damage. It is shrunk only with `damage` (`hydo explore --shrink-detected-damage`).
pub fn shrinks(v: &str, class: &str, damage: bool) -> bool {
    match verdict_kind(v) {
        "violation" => true,
        "error" => {
            let b = v.trim();
            let b = b.strip_prefix("HYDO-ORACLE: ").unwrap_or(b);
            let restart = b.strip_prefix("ERROR ").is_some_and(|r| {
                r.starts_with("restart after the faults")
                    || r.split_once(" restart failed")
                        .is_some_and(|(svc, _)| !svc.is_empty() && !svc.contains(' '))
            });
            let endless = [
                "TIMEOUT workload",
                "TIMEOUT check",
                "TIMEOUT recovery",
                "ERROR workload: exit budget exhausted",
                "ERROR check: exit budget exhausted",
            ]
            .iter()
            .any(|p| b.starts_with(p));
            (restart || endless) && (damage || !detected_damage(v, class))
        }
        _ => false,
    }
}

/// Whether a failure is a system's detection of damage that hydo made (Milestone 339): a service
/// that does not start again in a run of the class `corrupted-files` or `damaged-disk`, or whose
/// restart names `its files were corrupted`. Such a failure, and an error kind first new in such
/// a run, is not shrunk without `hydo explore --shrink-detected-damage`.
pub fn detected_damage(v: &str, class: &str) -> bool {
    let b = v.trim();
    let b = b.strip_prefix("HYDO-ORACLE: ").unwrap_or(b);
    let restart = b.strip_prefix("ERROR ").is_some_and(|r| {
        r.starts_with("restart after the faults")
            || r.split_once(" restart failed")
                .is_some_and(|(svc, _)| !svc.is_empty() && !svc.contains(' '))
    });
    restart
        && (class == "corrupted-files"
            || class == "damaged-disk"
            || b.contains("its files were corrupted"))
}

/// Whether a shrink candidate's verdict keeps the failure being shrunk: a violation of the same
/// signature ([`same_failure`]), or for an error an error of the same signature (Milestone 339).
pub fn same_outcome(v_ref: &str, v: &str) -> bool {
    if verdict_kind(v_ref) == "error" {
        verdict_kind(v) == "error" && sig_key(v) == sig_key(v_ref)
    } else {
        same_failure(v_ref, v)
    }
}

/// A value as `"%s" % v` prints it in a report table.
pub fn cell(v: &Value) -> String {
    match v {
        Value::Number(n) if n.is_f64() => float_repr(n.as_f64().unwrap_or(0.0)),
        other => py_str(other),
    }
}
