//! hydo-history: operation histories recorded by guest programs, and the host-side checkers
//! (format and semantics: hydo/HISTORIES.md). It replaces hydo/history/hydohist.py and the
//! `hydo-check` script around it, with the same verdicts, witnesses, records and texts.
//!
//! A workload records every operation as HYDO-HIST lines on the guest console. After the run the
//! host reads the saved consoles (the workload VM's and, after a power cut, the recovery VM's),
//! rebuilds the history and checks it:
//!
//! - `linearizable`: a register or compare-and-set object per key, the Wing-Gong-Lowe search with
//!   memoization; unknown outcomes may take effect at any later time or never; optional store
//!   versions. The witness is the earliest non-linearizable prefix, delta-debugged.
//! - `list-append`: Elle-style direct checks and a G0 / G1c / G-single / G2 cycle search, with
//!   real-time and process edges for the strict and strong-session levels.
//!
//! Events are ordered by their position in the consoles (workload first, then recovery), not by
//! their time stamps. [`run_checks`] turns the results into HYDO-ASSERT records and a combined
//! HYDO-ORACLE verdict, as `hydo run`, `hydo repro` and the explorer use them.
//!
//! Library use (what the explorer's host-side check does after a run):
//!
//! ```no_run
//! use std::path::Path;
//! let dir = Path::new("out/consoles");
//! let srcs = hydo_history::console_sources(dir, "s1-42");
//! if hydo_history::has_checks(&srcs) {
//!     let r = hydo_history::run_checks(&srcs, Some("HYDO-ORACLE: OK invariants=0 held ()"),
//!                                      hydo_history::DEFAULT_BUDGET, None).unwrap();
//!     hydo_history::write_checks_files(dir, "s1-42", &r).unwrap();
//!     println!("{}", r.verdict.unwrap_or_default());
//! }
//! ```

pub mod cli;
#[doc(hidden)]
pub mod equiv;
pub mod events;
mod fx;
pub mod listappend;
pub mod py;
pub mod register;
mod unitab;

pub use events::{pair_ops, parse_sources, Event, Events, Kind, Op, Status};
pub use py::{Crash, PyVal};
pub use register::{pdec, penc};

use std::path::Path;

/// WGL search steps per key before a key counts as undecided.
pub const DEFAULT_BUDGET: i64 = 2_000_000;
/// The check types.
pub const CHECK_TYPES: [&str; 2] = ["linearizable", "list-append"];

/// A check's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Violation,
    Undecided,
    /// A history the check cannot read (a malformed record for its model, a bad spec).
    Error,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Violation => "violation",
            Outcome::Undecided => "undecided",
            Outcome::Error => "error",
        }
    }
}

/// One check's result (hydohist's result dict).
#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub status: Outcome,
    /// The details dict, in insertion order (records print it with sorted keys).
    pub details: Vec<(String, PyVal)>,
    /// One sentence; for a violation it names the witness.
    pub summary: String,
    /// The full explanation (the witness's operations), or "".
    pub explanation: String,
    /// Pairing warnings (completions without an invocation and the like).
    pub warnings: Vec<String>,
}

/// Why a check stopped: a history its model cannot read (HistError, a result with status
/// error), or an exception the reference does not catch.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckErr {
    Hist(String),
    Crash(Crash),
}

impl From<Crash> for CheckErr {
    fn from(c: Crash) -> CheckErr {
        CheckErr::Crash(c)
    }
}

/// `hydohist.run_check`: one check spec (a dict) over the events.
pub fn run_check(ev: &Events, spec: &PyVal, budget: i64) -> Result<CheckResult, Crash> {
    if !spec.is_dict() {
        return Err(Crash(format!(
            "AttributeError: '{}' object has no attribute 'get'",
            spec.type_name()
        )));
    }
    let t = spec.get("type").cloned().unwrap_or(PyVal::None);
    let res = (|| -> Result<CheckResult, CheckErr> {
        if !CHECK_TYPES.iter().any(|c| t.eq_str(c)) {
            return Err(CheckErr::Hist(format!(
                "check type {}: one of {}",
                t.repr(),
                CHECK_TYPES.join(", ")
            )));
        }
        let (ops, warn) = pair_ops(ev, spec.get("history"));
        let mut res = if t.eq_str("linearizable") {
            register::check_linearizable(&ops, ev, spec, budget)?
        } else {
            listappend::check_list_append(&ops, ev, spec)?
        };
        res.warnings = warn;
        Ok(res)
    })();
    match res {
        Ok(r) => Ok(r),
        Err(CheckErr::Hist(e)) => Ok(CheckResult {
            status: Outcome::Error,
            details: Vec::new(),
            summary: e.clone(),
            explanation: e,
            warnings: Vec::new(),
        }),
        Err(CheckErr::Crash(c)) => Err(c),
    }
}

/// What [`run_checks`] returns (hydohist.run_checks's dict).
#[derive(Debug, Clone, PartialEq)]
pub struct Checks {
    /// HYDO-ASSERT-DECL / HYDO-ASSERT lines, one per element (no newline).
    pub records: Vec<String>,
    /// The combined `HYDO-ORACLE: ...` verdict; None when there are no checks.
    pub verdict: Option<String>,
    /// Each spec's id and result, in declaration order; a spec whose id equals an earlier one
    /// (as a dict key: `1 == 1.0 == true`) replaces that result and keeps its place and id.
    pub results: Vec<(PyVal, CheckResult)>,
    /// The full report (explanations), lines joined with "\n".
    pub report: String,
    /// Malformed-line warnings of the consoles.
    pub warnings: Vec<String>,
    /// Each check as the results format reports it (one per id, in the order of `results`).
    pub checks: Vec<CheckInfo>,
}

/// One check of [`Checks`]: what its spec says and what it found.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckInfo {
    /// The spec's id, type (`linearizable`, `list-append`), history name and message.
    pub id: String,
    pub check: String,
    pub history: String,
    pub msg: String,
    pub status: Outcome,
    pub summary: String,
    pub explanation: String,
    pub warnings: Vec<String>,
    /// The details of its HYDO-ASSERT record (JSON, sorted keys: the checker's details, `check`
    /// and `out`).
    pub details: String,
}

fn dict(pairs: Vec<(&str, PyVal)>) -> PyVal {
    PyVal::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// `hydohist.run_checks`: sources = [(label, console text)] in run order. A guest verdict that is
/// already a violation or an error stays the verdict (the guest's own invariants come first);
/// otherwise the first failing check (in declaration order) makes it a VIOLATION, an undecided or
/// unreadable check an ERROR, and an OK verdict gets the checks' summary appended. `specs`
/// replaces the HYDO-CHECK declarations of the consoles (`hydo-check --check`). Err = an
/// exception the reference would not catch (only malformed declarations or specs cause one).
pub fn run_checks<S: AsRef<str>, T: AsRef<str>>(
    sources: &[(S, T)],
    guest_verdict: Option<&str>,
    budget: i64,
    specs: Option<&[PyVal]>,
) -> Result<Checks, Crash> {
    let (ev, found, warn) = parse_sources(sources)?;
    let mut finals: Vec<(py::KeyRepr, PyVal)> = Vec::new();
    for sp in &found {
        let id = sp.get("id").cloned().unwrap_or(PyVal::None);
        let rc = sp
            .get("_final_rc")
            .cloned()
            .unwrap_or(PyVal::Int(py::BigInt::from_i64(0)));
        finals.push((id.key()?, rc));
    }
    // a declared spec's own `_final_rc` is what `finals` holds for its id; a spec given by the
    // caller finds it by id (as the reference's dict lookup does)
    let own_finals = specs.is_none();
    let specs: Vec<PyVal> = match specs {
        Some(s) => s.to_vec(),
        None => found,
    };
    if specs.is_empty() {
        return Ok(Checks {
            records: Vec::new(),
            verdict: None,
            results: Vec::new(),
            report: String::new(),
            warnings: warn,
            checks: Vec::new(),
        });
    }
    let mut infos: Vec<CheckInfo> = Vec::new();
    let mut recs = Vec::new();
    let mut results: Vec<(PyVal, CheckResult)> = Vec::new();
    let mut result_keys: Vec<py::KeyRepr> = Vec::new();
    let mut rep: Vec<String> = Vec::new();
    let mut first_bad: Option<(PyVal, String)> = None;
    let mut first_err: Option<(PyVal, String)> = None;
    let mut oks: Vec<(PyVal, String)> = Vec::new();
    for sp in &specs {
        let mut r = run_check(&ev, sp, budget)?;
        let id = sp
            .get("id")
            .cloned()
            .ok_or_else(|| Crash("KeyError: 'id'".into()))?;
        let key = id.key()?;
        let zero = PyVal::Int(py::BigInt::from_i64(0));
        let frc = if own_finals {
            sp.get("_final_rc").unwrap_or(&zero)
        } else {
            finals
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v)
                .unwrap_or(&zero)
        };
        if frc.truthy() && r.status == Outcome::Ok {
            // the read-back did not complete: the history may miss the reads that matter
            r = CheckResult {
                status: Outcome::Error,
                summary: format!(
                    "its final: read-back exited with status {}, so the history is incomplete ({})",
                    frc.fmt_d()?,
                    r.summary
                ),
                explanation: String::new(),
                details: r.details,
                warnings: r.warnings,
            };
        }
        let typ = sp.get("type").cloned().unwrap_or(PyVal::None);
        let hist = sp.get("history").cloned().unwrap_or(PyVal::None);
        let msg = match sp.get("msg") {
            Some(m) if m.truthy() => m.clone(),
            _ => {
                let mc = sp
                    .get("model")
                    .or_else(|| sp.get("consistency"))
                    .map(PyVal::str_of)
                    .unwrap_or_default();
                PyVal::Str(format!(
                    "{} history {} is {}",
                    typ.str_of(),
                    hist.str_of(),
                    mc
                ))
            }
        };
        recs.push(format!(
            "HYDO-ASSERT-DECL {}",
            py::dumps_sorted(&dict(vec![
                ("t", PyVal::str("always")),
                ("id", id.clone()),
                ("msg", msg.clone()),
            ]))
        ));
        let mut det = PyVal::Dict(r.details.clone());
        det.set("check", typ.clone());
        det.set("out", PyVal::str(py::head(&r.summary, 400)));
        let info = CheckInfo {
            id: id.str_of(),
            check: typ.str_of(),
            history: hist.str_of(),
            msg: msg.str_of(),
            status: r.status,
            summary: r.summary.clone(),
            explanation: r.explanation.clone(),
            warnings: r.warnings.clone(),
            details: py::dumps_sorted(&det),
        };
        if matches!(r.status, Outcome::Ok | Outcome::Violation) {
            recs.push(format!(
                "HYDO-ASSERT {}",
                py::dumps_sorted(&dict(vec![
                    ("t", PyVal::str("always")),
                    ("id", id.clone()),
                    ("ok", PyVal::Bool(r.status == Outcome::Ok)),
                    ("msg", msg),
                    ("details", det),
                ]))
            ));
        }
        rep.push(format!(
            "== {} ({}, history {}): {}",
            id.str_of(),
            typ.str_of(),
            hist.str_of(),
            r.status.name().to_uppercase()
        ));
        rep.push(r.summary.clone());
        if !r.explanation.is_empty() {
            rep.push(r.explanation.clone());
        }
        for w in r.warnings.iter().take(20) {
            rep.push(format!("warning: {w}"));
        }
        rep.push(String::new());
        match r.status {
            Outcome::Violation if first_bad.is_none() => {
                first_bad = Some((id.clone(), r.summary.clone()))
            }
            Outcome::Violation => {}
            Outcome::Undecided | Outcome::Error => {
                if first_err.is_none() {
                    first_err = Some((id.clone(), r.summary.clone()))
                }
            }
            Outcome::Ok => oks.push((id.clone(), r.summary.clone())),
        }
        match result_keys.iter().position(|k| *k == key) {
            Some(i) => {
                results[i].1 = r;
                // the result keeps the earlier spec's place and id (a dict key)
                infos[i] = CheckInfo {
                    id: infos[i].id.clone(),
                    ..info
                };
            }
            None => {
                result_keys.push(key);
                results.push((id, r));
                infos.push(info);
            }
        }
    }
    let gv = guest_verdict.unwrap_or("");
    let verdict = if !gv.is_empty() && !gv.contains("HYDO-ORACLE: OK") {
        gv.to_string()
    } else if let Some((id, s)) = &first_bad {
        format!(
            "HYDO-ORACLE: VIOLATION [{}] {}",
            id.str_of(),
            py::head(s, 600)
        )
    } else if let Some((id, s)) = &first_err {
        format!(
            "HYDO-ORACLE: ERROR check {} could not be evaluated: {}",
            id.str_of(),
            py::head(s, 300)
        )
    } else {
        let (id, s) = &oks[0];
        let tail = format!("checks={} held ({}: {})", oks.len(), id.str_of(), s);
        if gv.is_empty() || py::strip(gv) == "HYDO-ORACLE: OK invariants=0 held ()" {
            format!("HYDO-ORACLE: OK {tail}")
        } else {
            format!("{gv} | {tail}")
        }
    };
    Ok(Checks {
        records: recs,
        verdict: Some(verdict),
        results,
        report: rep.join("\n"),
        warnings: warn,
        checks: infos,
    })
}

/// A console file as Python's `open(p, errors="replace").read()` reads it: UTF-8 with invalid
/// bytes replaced, and universal newlines ("\r\n" and "\r" become "\n").
pub fn read_console(p: &Path) -> std::io::Result<String> {
    Ok(decode_console(&std::fs::read(p)?))
}

/// The text [`read_console`] makes of a console's bytes.
pub fn decode_console(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    if s.contains('\r') {
        s.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        s.into_owned()
    }
}

/// The consoles of one run in `dir`: `<tag>.log` and, after a power cut, `<tag>.recovery.log`,
/// labelled `<tag>` and `<tag>.recovery` (what `hydo run` and the explorer pass).
pub fn console_sources(dir: &Path, tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for suffix in ["", ".recovery"] {
        let p = dir.join(format!("{tag}{suffix}.log"));
        if p.exists() {
            out.push((
                format!("{tag}{suffix}"),
                read_console(&p).unwrap_or_default(),
            ));
        }
    }
    out
}

/// Whether any console declares a history check (runs without one are left alone).
pub fn has_checks<S: AsRef<str>, T: AsRef<str>>(sources: &[(S, T)]) -> bool {
    sources
        .iter()
        .any(|(_, t)| t.as_ref().contains(events::CHECK_TAG))
}

/// `<tag>.checks.log` (the records, one per line) and `<tag>.checks.txt` (the report, then the
/// verdict) in `dir`, as `hydo run` and the explorer write them. A missing verdict is written as
/// "None", as the Python CLI did.
pub fn write_checks_files(dir: &Path, tag: &str, r: &Checks) -> std::io::Result<()> {
    let recs: String = r.records.iter().map(|x| format!("{x}\n")).collect();
    std::fs::write(dir.join(format!("{tag}.checks.log")), recs)?;
    std::fs::write(
        dir.join(format!("{tag}.checks.txt")),
        format!("{}\n{}\n", r.report, r.verdict.as_deref().unwrap_or("None")),
    )
}
