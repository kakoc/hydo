//! The Rust side of the equivalence proof (tests/equiv.py): one case in, one answer out, in the
//! exact text tests/equiv.py writes for hydohist.py, so the two can be compared byte for byte.
//!
//!   {"kind":"checks","srcs":[[label,text],...],"verdict":v|null,"budget":n,"specs":[...]|null}
//!   {"kind":"pdec","toks":[...]}            {"kind":"penc","strs":[...],"extra":","}
//!   {"kind":"decode","hex":"..."}           (Python's open(errors="replace").read() of the bytes)

use crate::py::{self, BigInt, PyVal};
use crate::{run_checks, CheckResult};
use std::time::Instant;

fn s(x: &str) -> PyVal {
    PyVal::str(x)
}

fn strs(xs: &[String]) -> PyVal {
    PyVal::List(xs.iter().map(|x| s(x)).collect())
}

fn dict(pairs: Vec<(&str, PyVal)>) -> PyVal {
    PyVal::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn result(r: &CheckResult) -> PyVal {
    dict(vec![
        ("status", s(r.status.name())),
        ("summary", s(&r.summary)),
        ("explanation", s(&r.explanation)),
        ("details", PyVal::Dict(r.details.clone())),
        ("warnings", strs(&r.warnings)),
    ])
}

fn list_of_str(v: Option<&PyVal>) -> Vec<String> {
    match v {
        Some(PyVal::List(a)) => a
            .iter()
            .map(|x| x.as_str().unwrap_or("").to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// The answer to one case line, and for a "checks" case the time run_checks took (ns).
pub fn answer(line: &str) -> (String, Option<u128>) {
    let Ok(c) = py::json_loads(line) else {
        return ("{\"bad-case\":true}".into(), None);
    };
    let mut took = None;
    let res = match c.get("kind").and_then(PyVal::as_str).unwrap_or("") {
        "checks" => {
            let srcs: Vec<(String, String)> = match c.get("srcs") {
                Some(PyVal::List(a)) => a
                    .iter()
                    .map(|p| {
                        let x = list_of_str(Some(p));
                        (
                            x.first().cloned().unwrap_or_default(),
                            x.get(1).cloned().unwrap_or_default(),
                        )
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let verdict = c.get("verdict").and_then(PyVal::as_str);
            let budget = match c.get("budget") {
                Some(PyVal::Int(b)) => b.saturating_i64(),
                _ => crate::DEFAULT_BUDGET,
            };
            let specs: Option<Vec<PyVal>> = match c.get("specs") {
                Some(PyVal::List(a)) => Some(a.clone()),
                _ => None,
            };
            let t0 = Instant::now();
            let r = run_checks(&srcs, verdict, budget, specs.as_deref());
            took = Some(t0.elapsed().as_nanos());
            match r {
                Err(_) => dict(vec![("crash", PyVal::Bool(true))]),
                Ok(r) => dict(vec![
                    ("records", strs(&r.records)),
                    (
                        "verdict",
                        r.verdict.as_deref().map(s).unwrap_or(PyVal::None),
                    ),
                    ("report", s(&r.report)),
                    ("warnings", strs(&r.warnings)),
                    (
                        "results",
                        PyVal::List(
                            r.results
                                .iter()
                                .map(|(id, x)| PyVal::List(vec![id.clone(), result(x)]))
                                .collect(),
                        ),
                    ),
                ]),
            }
        }
        "pdec" => strs(
            &list_of_str(c.get("toks"))
                .iter()
                .map(|t| crate::pdec(t))
                .collect::<Vec<_>>(),
        ),
        "penc" => {
            let extra = c.get("extra").and_then(PyVal::as_str).unwrap_or("");
            strs(
                &list_of_str(c.get("strs"))
                    .iter()
                    .map(|x| crate::penc(x, extra))
                    .collect::<Vec<_>>(),
            )
        }
        "decode" => {
            let hex = c.get("hex").and_then(PyVal::as_str).unwrap_or("");
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0))
                .collect();
            s(&crate::decode_console(&bytes))
        }
        _ => dict(vec![("unknown", PyVal::Int(BigInt::from_i64(1)))]),
    };
    (py::dumps_sorted(&res), took)
}
