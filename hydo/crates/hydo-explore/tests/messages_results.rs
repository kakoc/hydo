//! A `type: messages` check in a run's result (docs/design/messaging.md "Results"): each claim is
//! a `history-check` finding under `<check>.<claim>`, the verdict names the first violated claim,
//! and `Run.messages` carries the declaration, the claims' statuses and the measures.
//!
//! The permanent results example `hydo/schema/examples/results/v1/scripted-messages-run/` is the
//! document this test makes from a scripted console (the host-side path of `hydo run` after the
//! VM: the checks, the findings, the run record; no VM). `HYDO_BLESS=1` writes it when it does
//! not exist; an existing example is never rewritten (hydo-results tests/examples.rs).

use hydo_explore::pool::Res;
use hydo_explore::results::{keep_checks, run_of};
use serde_json::{json, Value};
use std::path::PathBuf;

/// A work queue: process 3 sends m1..m3, process 5 receives and acknowledges m1 and m2; m3 was
/// acknowledged to its sender and never delivered, then the drain finds the queue empty.
const CONSOLE: &str = "\
HYDO-CHECK {\"claims\":{\"after_ack\":\"no-redelivery\",\"delivery\":\"at-least-once\",\"lease\":{\"ms\":30000}},\"history\":\"jobs\",\"id\":\"jobs\",\"msg\":\"the job queue loses no job and leases them\",\"type\":\"messages\"}
HYDO-HIST jobs 3 invoke 1000000000 send jobs m1 m2
HYDO-HIST jobs 3 ok 1004000000 send jobs m1;o=1 m2;o=2
HYDO-HIST jobs 3 invoke 1010000000 send jobs m3
HYDO-HIST jobs 3 ok 1012000000 send jobs m3;o=3
HYDO-HIST jobs 5 invoke 1020000000 recv jobs -
HYDO-HIST jobs 5 ok 1025000000 recv jobs - m1;o=1;n=1;h=a1 m2;o=2;n=1;h=a2
HYDO-HIST jobs 5 invoke 1100000000 ack jobs - m1;h=a1 m2;h=a2
HYDO-HIST jobs 5 ok 1102000000 ack jobs - m1;h=a1 m2;h=a2
HYDO-ORACLE: OK invariants=0 held ()
HYDO-HIST jobs 9 invoke 2000000000 recv jobs -
HYDO-HIST jobs 9 ok 2001000000 recv jobs -
HYDO-HIST jobs 9 ok 2002000000 drained jobs -
HYDO-CHECK-FINAL {\"id\":\"jobs\",\"rc\":0}
";

fn example() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema/examples/results/v1/scripted-messages-run/result.json")
}

#[test]
fn a_messages_check_in_a_run_document() {
    let srcs = vec![("one".to_string(), CONSOLE.to_string())];
    let guest = "HYDO-ORACLE: OK invariants=0 held ()";
    let checks =
        hydo_history::run_checks(&srcs, Some(guest), hydo_history::DEFAULT_BUDGET, None).unwrap();
    let mut res = Res::new();
    res.insert("fp".into(), json!("0000000000000000"));
    keep_checks(&mut res, &checks);
    res.insert("guest_verdict".into(), json!(guest));
    res.insert("verdict".into(), json!(checks.verdict));
    let run = run_of(
        "one",
        "disk=cut | run C=12",
        &res,
        &[("one.log".into(), CONSOLE.into())],
    );
    assert!(
        run.verdict
            .text
            .starts_with("HYDO-ORACLE: VIOLATION [jobs.no-loss] m3 was acknowledged"),
        "{}",
        run.verdict.text
    );
    let ids: Vec<(&str, &str)> = run
        .findings
        .iter()
        .filter(|f| f.kind == "history-check")
        .map(|f| (f.id.as_str(), f.status.as_str()))
        .collect();
    assert_eq!(
        ids,
        vec![
            ("jobs.no-phantom", "ok"),
            ("jobs.no-loss", "violation"),
            ("jobs.after-ack", "ok"),
            ("jobs.lease", "ok"),
        ]
    );
    assert_eq!(run.messages.len(), 1);
    let m = &run.messages[0];
    let lost = m.measures.as_ref().unwrap().lost.as_ref().unwrap();
    assert_eq!(
        (lost[0].count, lost[0].first.clone()),
        (1, vec!["m3".to_string()])
    );
    let doc = serde_json::to_string_pretty(&run).unwrap() + "\n";
    let p = example();
    if !p.exists() && std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1") {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, &doc).unwrap();
    }
    let want = std::fs::read_to_string(&p).unwrap_or_default();
    let want: Value = serde_json::from_str(&want).unwrap_or(Value::Null);
    let got: Value = serde_json::from_str(&doc).unwrap();
    assert_eq!(got, want, "{doc}");
}
