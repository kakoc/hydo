//! The finding key (docs/design/ci.md section 4.2): what identifies "the same finding" across
//! commits. Fingerprints cannot (a changed line of a service changes them all) and schedules
//! cannot (several schedules hit one bug, and the minimal one moves when the code changes). What
//! stays is **what the check said**: the key is made from the finding's kind and its normalized
//! statement, `f1:<kind>:<first 10 hex of sha256(statement)>`. `f1` is the key's version, raised
//! if the normalization ever changes, so that old and new keys are never compared silently.
//!
//! The statement:
//!
//! | kind | statement |
//! |---|---|
//! | `invariant`, `history-check`, `assertion` (a VIOLATION verdict) | the verdict's signature as the explorer forms it (numbers as `#`, up to ` order=`); when the verdict names a bracketed property id (`[shop.no_double_charge]`), only `VIOLATION [<id>]`, so rewording a check's message keeps its key |
//! | `run-error` (an ERROR or TIMEOUT verdict) | the verdict's signature |
//! | `error-log` | the error kind's 12-hex signature |
//!
//! The fault classes that led to a finding are attributes of it, not part of the key.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The key version.
pub const VERSION: &str = "f1";

/// A failing verdict's finding: (kind, title, key). None for a passing verdict.
pub fn of_verdict(
    verdict: &str,
    findings: &[hydo_results::Finding],
) -> Option<(String, String, String)> {
    let vk = hydo_engine::protocol::verdict_kind(verdict);
    if vk == "ok" {
        return None;
    }
    let title = hydo_explore::report::sig_key(verdict);
    let kind = if vk == "violation" {
        // the verdict's own source when the run says which check failed (an invariant first)
        let failed = |k: &str| {
            findings
                .iter()
                .any(|f| f.kind == k && f.status == hydo_results::status::VIOLATION)
        };
        if failed(hydo_results::kind::INVARIANT) {
            hydo_results::kind::INVARIANT
        } else if failed(hydo_results::kind::HISTORY_CHECK) {
            hydo_results::kind::HISTORY_CHECK
        } else if failed(hydo_results::kind::ASSERTION) {
            hydo_results::kind::ASSERTION
        } else {
            hydo_results::kind::INVARIANT
        }
    } else {
        "run-error"
    };
    let statement = statement(kind, &title);
    Some((kind.to_string(), title, key(kind, &statement)))
}

/// The statement a key is made from (the table above).
pub fn statement(kind: &str, title: &str) -> String {
    if kind != "run-error" && kind != hydo_results::kind::ERROR_LOG {
        if let Some(id) = property_id(title) {
            return format!("VIOLATION [{id}]");
        }
    }
    title.to_string()
}

/// The first bracketed property id in a verdict (`[shop.no_double_charge]`): letters, digits and
/// `._-`, with at least one letter. `[#]` (a number made `#`) is not one.
pub fn property_id(text: &str) -> Option<&str> {
    let mut rest = text;
    while let Some(i) = rest.find('[') {
        let after = &rest[i + 1..];
        let j = after.find(']')?;
        let id = &after[..j];
        if !id.is_empty()
            && id.chars().any(|c| c.is_ascii_alphabetic())
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Some(id);
        }
        rest = &after[j + 1..];
    }
    None
}

/// `f1:<kind>:<first 10 hex of sha256(statement)>`.
pub fn key(kind: &str, statement: &str) -> String {
    let h = Sha256::digest(statement.as_bytes());
    let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
    format!("{VERSION}:{kind}:{}", &hex[..10])
}

/// The words of fault token kinds, for people.
pub fn fault_word(k: &str) -> &str {
    match k {
        "K" => "kill",
        "X" => "timed kill",
        "P" => "pause",
        "N" => "network",
        "O" => "one-way loss",
        "C" => "power cut",
        "T" => "clock jump",
        "H" => "protocol",
        "F" => "freeze",
        "A" => "reaction",
        "S" => "storage",
        "disk" => "disk",
        "cf" => "cut at a flush",
        "tj" => "tick jitter",
        other => other,
    }
}

/// Counts of a schedule's fault tokens by kind and by the service they target (docs/design/ci.md
/// section 4.1, `coverage`): every guest token but the workload's own (`W=`, the composition) and
/// the durability knobs (`M=`, `R=`), the concurrency (`Q=`) and BUGGIFY's seed (`B=`); on the
/// host side `disk=` (but `none`), `cf=` and tick jitter.
pub fn count_faults(
    schedule: &str,
    faults: &mut BTreeMap<String, u64>,
    services: &mut BTreeMap<String, u64>,
) {
    let (host, guest) = schedule.split_once('|').unwrap_or(("", schedule));
    for t in host.split_whitespace() {
        let (k, val) = t.split_once('=').unwrap_or((t, ""));
        let k = if k.starts_with("tj") { "tj" } else { k };
        if (k == "disk" && val != "none") || k == "cf" || (k == "tj" && t.starts_with("tj=")) {
            *faults.entry(k.into()).or_default() += 1;
        }
    }
    for t in guest.split_whitespace() {
        let Some((k, val)) = t.split_once('=') else {
            continue;
        };
        if matches!(k, "W" | "M" | "R" | "Q" | "B") {
            continue;
        }
        *faults.entry(k.into()).or_default() += 1;
        // the target: K=redis@11+1, H=payments.charge@0-1:drop, N=redis@3-6:delay, O=a>b@..
        if matches!(k, "K" | "X" | "P" | "N" | "O" | "F" | "H") {
            let end = val
                .find(['@', ':', '.', '>', ',', '-', '+'])
                .unwrap_or(val.len());
            let svc = &val[..end];
            if !svc.is_empty() && svc.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                *services.entry(svc.into()).or_default() += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_come_from_what_the_check_said() {
        let v = "HYDO-ORACLE: VIOLATION [payments.charged_at_most_once] o0 charged 3 times";
        let (kind, title, k) = of_verdict(v, &[]).unwrap();
        assert_eq!(kind, "invariant");
        assert_eq!(
            title,
            "VIOLATION [payments.charged_at_most_once] o# charged # times"
        );
        // the bracketed id decides the key: a reworded message keeps it
        let (_, _, k2) = of_verdict(
            "HYDO-ORACLE: VIOLATION [payments.charged_at_most_once] order 7 taken twice",
            &[],
        )
        .unwrap();
        assert_eq!(k, k2);
        assert_eq!(
            k,
            key("invariant", "VIOLATION [payments.charged_at_most_once]")
        );
        assert!(k.starts_with("f1:invariant:") && k.len() == "f1:invariant:".len() + 10);
        // without an id, the signature (numbers as #) is the statement
        let (_, t, k3) = of_verdict("HYDO-ORACLE: VIOLATION lost=3 acked=20", &[]).unwrap();
        assert_eq!(t, "VIOLATION lost=# acked=#");
        assert_eq!(
            k3,
            of_verdict("HYDO-ORACLE: VIOLATION lost=1 acked=19", &[])
                .unwrap()
                .2
        );
        assert!(of_verdict("HYDO-ORACLE: OK invariants=6 held", &[]).is_none());
        let (rk, rt, _) = of_verdict("ERROR shop restart failed", &[]).unwrap();
        assert_eq!(
            (rk.as_str(), rt.as_str()),
            ("run-error", "ERROR shop restart failed")
        );
        assert_eq!(property_id("x [#] y [a.b-c_1] z"), Some("a.b-c_1"));
        assert_eq!(property_id("k:# lost"), None);
    }

    #[test]
    fn a_failed_history_check_is_its_own_kind() {
        let f = hydo_results::Finding {
            kind: "history-check".into(),
            id: "linearizable".into(),
            status: "violation".into(),
            ..Default::default()
        };
        let (kind, _, k) =
            of_verdict("VIOLATION [history] linearizable: stale read", &[f]).unwrap();
        assert_eq!(kind, "history-check");
        assert!(k.starts_with("f1:history-check:"));
    }

    #[test]
    fn faults_are_counted_by_kind_and_service() {
        let (mut f, mut s) = (BTreeMap::new(), BTreeMap::new());
        count_faults(
            "disk=cut dseed=1 | run K=redis@11+1 C=24 H=payments.charge@0-1:drop W=1,p1 R=no",
            &mut f,
            &mut s,
        );
        count_faults(
            "disk=none dseed=2 | run P=shop@3+2 N=redis@3-6:delay,300ms",
            &mut f,
            &mut s,
        );
        assert_eq!(
            f.into_iter().collect::<Vec<_>>(),
            vec![
                ("C".to_string(), 1),
                ("H".to_string(), 1),
                ("K".to_string(), 1),
                ("N".to_string(), 1),
                ("P".to_string(), 1),
                ("disk".to_string(), 1),
            ]
        );
        assert_eq!(
            s.into_iter().collect::<Vec<_>>(),
            vec![
                ("payments".to_string(), 1),
                ("redis".to_string(), 2),
                ("shop".to_string(), 1)
            ]
        );
    }
}
