//! Function contracts on the host (Milestone 241, docs/design/contracts.md): a run's records
//! (`<tag>.contracts.jsonl`, written by the engine) judged against the image's plan. A broken
//! clause is a finding of kind `contract`, and it turns an `OK` verdict into a `VIOLATION` that
//! names the function, the clause and its line; the guest's own verdict is kept.
//!
//! Since Milestone 291 the same records also judge the plan's value properties (`// hydo:
//! keep` lines, docs/design/value-history.md) on the run's value histories. A broken one is a
//! finding of kind `values`, and it turns an `OK` verdict into a `VIOLATION` that names the
//! property as written.

use crate::pool::Res;
use hydo_contracts::eval::{judge, Report};
use hydo_contracts::plan::{Keep, Plan};
use hydo_contracts::values::{History, KeepOutcome};
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

/// What a run's records gave: the contracts' report, and the value properties (`// hydo: keep`,
/// Milestone 291) judged on the run's value histories.
#[derive(Clone, Debug)]
pub struct Judged {
    pub report: Report,
    pub histories: Vec<History>,
    pub keeps: Vec<KeepOutcome>,
}

/// The verdict a broken value property gives a run that passed otherwise: the property as
/// written and what broke it, without a key or a value, so that every run that breaks it has
/// one signature.
pub fn values_verdict(k: &Keep) -> String {
    format!(
        "VIOLATION [{}] {}",
        values_id(k),
        hydo_contracts::values::break_words(k)
    )
}

/// The id of a value property's finding: `values ` and the line as written.
pub fn values_id(k: &Keep) -> String {
    format!("values {}", k.text)
}

/// The findings of the value properties: one per broken property (kind `values`), with the
/// first broken key's history up to the break as its output.
pub fn values_findings(hs: &[History], outs: &[KeepOutcome]) -> Vec<Finding> {
    let mut out = Vec::new();
    for o in outs {
        let Some(b) = o.breaks.first() else { continue };
        let h = &hs[b.history];
        let p = |i: usize| {
            let x = &h.points[i];
            json!({
                "function": x.function,
                "when": x.when,
                "call": x.call,
                "caller": x.caller,
                "key": x.key,
                "at": x.at,
                "boot": x.boot,
            })
        };
        let mut d = Map::new();
        d.insert("property".into(), json!(o.keep.text));
        d.insert("file".into(), json!(o.keep.file));
        d.insert("line".into(), json!(o.keep.line));
        d.insert("name".into(), json!(o.keep.name));
        d.insert("keys".into(), json!(o.keys));
        d.insert("stops".into(), json!(o.stops));
        d.insert("broken_keys".into(), json!(o.breaks.len()));
        d.insert(
            "first".into(),
            json!({
                "key": h.key,
                "value": b.value,
                "expected": b.expected,
                "stop": p(b.point),
                "reference": p(b.reference),
                "changed": p(b.changed),
            }),
        );
        d.insert(
            "output".into(),
            json!(hydo_contracts::values::break_lines(h, b, &o.keep)),
        );
        out.push(Finding {
            kind: kind::VALUES.into(),
            id: values_id(&o.keep),
            status: status::VIOLATION.into(),
            message: hydo_contracts::values::keep_words(&o.keep),
            details: d,
        });
    }
    out
}

fn fold_verdict(res: &mut Res, text: String) {
    let verdict = res
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if hydo_engine::protocol::verdict_kind(&verdict) == "ok" {
        if !res.contains_key("guest_verdict") {
            res.insert("guest_verdict".into(), json!(verdict));
        }
        res.insert("verdict".into(), json!(text));
    }
}

/// Judge the records at `records` with `plan` and fold the outcome into a run's result: the
/// contracts' findings under [`crate::results::CONTRACTS`], the value properties' under
/// [`crate::results::VALUES`], the calls checked under [`crate::results::CONTRACT_CALLS`], and
/// an `OK` verdict made a `VIOLATION` by the first broken clause, else by the first broken value
/// property. Returns what was judged.
pub fn fold(plan: &Plan, records: &Path, res: &mut Res) -> Result<Judged, String> {
    let (recs, end) = hydo_contracts::plan::read_run(records)?;
    // the error paths (Milestone 306): which places ran; they never change the verdict
    crate::paths::fold(plan, records, res)?;
    // every function (Milestone 394): which functions ran, for the live map
    crate::funcs::fold(plan, records, res)?;
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
    // the value properties (Milestone 291): only for a plan with `keep` lines
    let (histories, keeps) = if plan.programs.iter().any(|p| !p.keeps.is_empty()) {
        let hs = hydo_contracts::values::histories(hydo_contracts::values::points(plan, &recs));
        let ks = hydo_contracts::values::judge_keeps(plan, &hs);
        (hs, ks)
    } else {
        (Vec::new(), Vec::new())
    };
    if !fs.is_empty() {
        res.insert(crate::results::CONTRACTS.into(), Value::Array(fs));
        if let Some(o) = rep.violated().next() {
            fold_verdict(res, verdict_text(o));
        }
    }
    let vs: Vec<Value> = values_findings(&histories, &keeps)
        .into_iter()
        .map(|f| serde_json::to_value(f).expect("a finding"))
        .collect();
    if !vs.is_empty() {
        res.insert(crate::results::VALUES.into(), Value::Array(vs));
        if let Some(o) = keeps.iter().find(|o| !o.breaks.is_empty()) {
            fold_verdict(res, values_verdict(&o.keep));
        }
    }
    Ok(Judged {
        report: rep,
        histories,
        keeps,
    })
}

/// One line per value property for people: how many keys and stops it judged and how many keys
/// broke it, and for a broken one the first broken key's history up to the break.
pub fn values_lines(j: &Judged) -> Vec<String> {
    let mut out = Vec::new();
    for o in &j.keeps {
        let (f, l) = (&o.keep.file, o.keep.line);
        match o.breaks.first() {
            None => out.push(format!(
                "hydo: value property held: {} ({f}:{l}), {} stops of {} keys judged",
                o.keep.text, o.stops, o.keys
            )),
            Some(b) => {
                out.push(format!(
                    "hydo: value property broken: {} ({f}:{l}) for {} of {} keys ({} stops \
                     judged)",
                    o.keep.text,
                    o.breaks.len(),
                    o.keys,
                    o.stops
                ));
                for x in hydo_contracts::values::break_lines(&j.histories[b.history], b, &o.keep) {
                    out.push(format!("hydo:   {x}"));
                }
            }
        }
    }
    out
}

/// The calls a run judged and the calls it did not (Milestone 255): every call past the run's
/// first [`hydo_contracts::plan::MAX_RECORDS`], and the calls whose return the run did not reach
/// (their `ensures` clauses are undecided).
pub fn summary(r: &Report) -> Vec<String> {
    // a plan of `record` lines only (Milestone 285): no clause to check
    if r.outcomes.is_empty() {
        return vec![format!(
            "hydo: recorded values: {} calls recorded (`hydo inspect values <run dir> --text` \
             shows their history)",
            r.calls
        )];
    }
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
    let undecided: u64 = r.outcomes.iter().map(|o| o.undecided).sum();
    if undecided > 0 {
        out.push(format!(
            "hydo: function contracts: {undecided} checks of a clause at a call were undecided: \
             a value the stop could not read (memory the process had not mapped yet, an index \
             past a length, another variant) or a return the run did not reach"
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
