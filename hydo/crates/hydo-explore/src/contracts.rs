//! Function contracts on the host (Milestone 241, docs/design/contracts.md): a run's records
//! (`<tag>.contracts.jsonl`, written by the engine) judged against the image's plan. A broken
//! clause is a finding of kind `contract`, and it turns an `OK` verdict into a `VIOLATION` that
//! names the function, the clause and its line; the guest's own verdict is kept.

use crate::pool::Res;
use hydo_contracts::eval::{judge, Report};
use hydo_contracts::plan::Plan;
use hydo_results::{kind, status, Finding};
use serde_json::{json, Map, Value};
use std::path::Path;

/// The verdict a broken clause gives a run that passed otherwise.
pub fn verdict_text(o: &hydo_contracts::eval::Outcome) -> String {
    format!(
        "VIOLATION [contract {}] {} {} ({}:{})",
        o.function, o.kind, o.text, o.file, o.line
    )
}

/// The findings of a report: one per broken clause.
pub fn findings(r: &Report) -> Vec<Finding> {
    r.violated()
        .map(|o| {
            let mut d = Map::new();
            d.insert("function".into(), json!(o.function));
            d.insert("file".into(), json!(o.file));
            d.insert("line".into(), json!(o.line));
            d.insert("clause".into(), json!(o.kind));
            d.insert("calls".into(), json!(o.calls));
            d.insert("violations".into(), json!(o.violations));
            d.insert("undecided".into(), json!(o.undecided));
            // calls past the run's first 100,000, which no clause was judged on (M255)
            d.insert("not_recorded".into(), json!(r.not_recorded));
            if let Some(f) = &o.first {
                d.insert(
                    "first".into(),
                    json!({"call": f.call, "at": f.at, "values": f.values}),
                );
            }
            Finding {
                kind: kind::CONTRACT.into(),
                id: format!("{} {} ({}:{})", o.function, o.kind, o.file, o.line),
                status: status::VIOLATION.into(),
                message: o.text.clone(),
                details: d,
            }
        })
        .collect()
}

/// Judge the records at `records` with `plan` and fold the outcome into a run's result: the
/// findings under [`crate::results::CONTRACTS`], the calls checked under
/// [`crate::results::CONTRACT_CALLS`], and an `OK` verdict made a `VIOLATION` by the first
/// broken clause. Returns the report.
pub fn fold(plan: &Plan, records: &Path, res: &mut Res) -> Result<Report, String> {
    let (recs, end) = hydo_contracts::plan::read_run(records)?;
    let mut rep = judge(plan, &recs);
    rep.not_recorded = end.not_recorded;
    let fs: Vec<Value> = findings(&rep)
        .into_iter()
        .map(|f| serde_json::to_value(f).expect("a finding"))
        .collect();
    res.insert(
        crate::results::CONTRACT_CALLS.into(),
        json!({
            "calls": rep.calls,
            "without_return": rep.without_return,
            "not_recorded": rep.not_recorded,
        }),
    );
    if fs.is_empty() {
        return Ok(rep);
    }
    res.insert(crate::results::CONTRACTS.into(), Value::Array(fs));
    let verdict = res
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if hydo_engine::protocol::verdict_kind(&verdict) == "ok" {
        if let Some(o) = rep.violated().next() {
            if !res.contains_key("guest_verdict") {
                res.insert("guest_verdict".into(), json!(verdict));
            }
            res.insert("verdict".into(), json!(verdict_text(o)));
        }
    }
    Ok(rep)
}

/// The calls a run judged and the calls it did not (Milestone 255): every call past the run's
/// first [`hydo_contracts::plan::MAX_RECORDS`], and the calls whose return the run did not reach
/// (their `ensures` clauses are undecided).
pub fn summary(r: &Report) -> Vec<String> {
    let mut out = vec![format!(
        "hydo: function contracts: {} calls checked against {} clauses, {} broken",
        r.calls,
        r.outcomes.len(),
        r.violated().count()
    )];
    if r.not_recorded > 0 {
        out.push(format!(
            "hydo: function contracts: {} calls in all; {} of them were not judged, because a \
             run records its first {} calls only",
            r.calls + r.not_recorded,
            r.not_recorded,
            hydo_contracts::plan::MAX_RECORDS
        ));
    }
    if r.without_return > 0 {
        out.push(format!(
            "hydo: function contracts: {} calls without their return (the run ended inside \
             them, or no breakpoint was free): their `ensures` clauses were not judged",
            r.without_return
        ));
    }
    out
}

/// One line per broken clause for people, with the first broken call's values.
pub fn lines(r: &Report) -> Vec<String> {
    let mut out = Vec::new();
    for o in r.violated() {
        out.push(format!(
            "hydo: contract broken: {} {} {} ({}:{}) at {} of {} calls",
            o.function, o.kind, o.text, o.file, o.line, o.violations, o.calls
        ));
        if let Some(f) = &o.first {
            out.push(format!(
                "hydo:   first at call {} (guest clock {}): {}",
                f.call,
                f.at,
                f.values.join(", ")
            ));
        }
    }
    out
}
