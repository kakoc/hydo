//! `type: messages`: the checker for message-passing systems (docs/design/messaging.md). hydo
//! knows nothing about any particular system: the project's declaration ([`decl`]) says what its
//! system promises, and the checker judges exactly those claims on the history the project's
//! client records (send, recv, ack, nack, extend, commit, sub and the drain's `drained`), and
//! measures the history whatever the system claims.
//!
//! Each claim is a property of its own, reported as `<check id>.<claim>`: held, violated with a
//! small sound witness (the records that together prove it), or undecided when the records it
//! needs are missing (no drain, no system ids, no offsets). Pairs a time-dependent claim cannot
//! judge (a `T=` clock step inside, records of two boots) are counted and never decide.

pub mod claims;
pub mod decl;
pub mod hist;
pub mod index;
pub mod measures;
pub mod queue;

use crate::events::Events;
use crate::py::{BigInt, PyVal};
pub use decl::Decl;
use hist::MHist;

/// A claim's status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Held,
    Violated,
    Undecided,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Held => "held",
            Status::Violated => "violated",
            Status::Undecided => "undecided",
        }
    }
}

/// The records of a witness, in the order the history wrote them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Witness {
    /// Indices into the history's operations.
    pub ops: Vec<usize>,
    /// Indices into its drained records.
    pub drained: Vec<usize>,
}

impl Witness {
    pub fn op(&mut self, _ix: &index::Ix<'_>, i: usize) {
        if !self.ops.contains(&i) {
            self.ops.push(i);
        }
    }
    pub fn drained(&mut self, _ix: &index::Ix<'_>, i: usize) {
        if !self.drained.contains(&i) {
            self.drained.push(i);
        }
    }
}

/// One claim's result.
#[derive(Clone, Debug, PartialEq)]
pub struct Claim {
    pub name: &'static str,
    pub status: Status,
    /// Candidates judged (messages, pairs or deliveries, by claim).
    pub judged: u64,
    /// Candidates the history could not decide.
    pub undecided: u64,
    /// One sentence; for a violation it names the witness.
    pub summary: String,
    pub witness: Witness,
}

/// What the checker found in one history.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub claims: Vec<Claim>,
    /// The measures (a JSON object).
    pub measures: PyVal,
    /// The declaration with every default filled in (a JSON object).
    pub declaration: PyVal,
    /// Operations of the history.
    pub ops: usize,
    pub warnings: Vec<String>,
    /// Each claim's witness as text (one line per record), by claim name.
    pub explanations: Vec<(String, String)>,
    /// Each claim's witness records as JSON, by claim name.
    pub witness_records: Vec<(String, PyVal)>,
}

fn secs(t: Option<i128>) -> String {
    match t {
        Some(t) => format!("{:.3}", t as f64 / 1e9),
        None => "?".into(),
    }
}

fn op_line(h: &MHist, i: usize) -> String {
    let op = &h.ops[i];
    let items: Vec<String> = op.items.iter().map(|x| x.token()).collect();
    let mut args = vec![hist::enc(&op.q)];
    if op.kind != hist::OpKind::Send {
        args.push(hist::enc(&op.g));
    }
    args.extend(items);
    for (k, o) in &op.commits {
        args.push(format!("{}@{}", hist::enc(k), hist::enc(o)));
    }
    if let Some(f) = &op.from {
        args.push(format!("from={f}"));
    }
    let boot = if op.inv.boot > 0 {
        format!(
            " ({})",
            h.labels.get(op.inv.boot).cloned().unwrap_or_default()
        )
    } else {
        String::new()
    };
    format!(
        "  process {} {} {} -> {} (t={}-{}s, positions {}-{}){boot}",
        op.proc_,
        op.kind.name(),
        args.join(" "),
        match op.out {
            hist::Out::Ok => "ok",
            hist::Out::Fail => "failed",
            hist::Out::Info => "unknown outcome",
        },
        secs(op.inv.shown),
        secs(op.ret.and_then(|r| r.shown)),
        op.inv.pos,
        op.ret.map(|r| r.pos.to_string()).unwrap_or("-".into())
    )
}

fn op_json(h: &MHist, i: usize) -> PyVal {
    let op = &h.ops[i];
    let n = |x: usize| PyVal::Int(BigInt::from_i64(x as i64));
    let t = |x: Option<i128>| match x {
        Some(v) => PyVal::Int(BigInt::from_digits(v < 0, &v.unsigned_abs().to_string())),
        None => PyVal::None,
    };
    PyVal::Dict(vec![
        ("process".into(), PyVal::str(&op.proc_)),
        ("op".into(), PyVal::str(op.kind.name())),
        ("channel".into(), PyVal::str(&op.q)),
        ("group".into(), PyVal::str(&op.g)),
        ("outcome".into(), PyVal::str(op.out.word())),
        ("position".into(), n(op.inv.pos)),
        (
            "done".into(),
            op.ret.map(|r| n(r.pos)).unwrap_or(PyVal::None),
        ),
        ("time_ns".into(), t(op.inv.shown)),
        ("done_ns".into(), t(op.ret.and_then(|r| r.shown))),
        ("boot".into(), n(op.inv.boot)),
        (
            "items".into(),
            PyVal::List(op.items.iter().map(|x| PyVal::Str(x.token())).collect()),
        ),
    ])
}

/// Checks history `name` of the events against the declaration. `clock` = the positions of the
/// run's clock steps ([`hist::clock_positions`]).
pub fn check(ev: &Events, name: &str, d: &Decl, clock: &[usize]) -> Report {
    let h = hist::read(ev, name, clock);
    let ix = index::build(&h, d);
    let mut claims = Vec::new();
    for c in decl::claim_names(d) {
        claims.push(match c {
            "no-phantom" => claims::no_phantom(&ix),
            "no-loss" => claims::no_loss(&ix, false),
            "at-most-once" => claims::at_most_once(&ix),
            "duplicates" => claims::duplicates(&ix),
            "after-ack" => claims::after_ack(&ix),
            "processing" => claims::processing(&ix),
            "lease" => claims::lease(&ix),
            "order" => claims::order(&ix),
            "dead-letter" => claims::dead_letter(&ix),
            "delay" => claims::delay(&ix),
            "fan-out" => claims::no_loss(&ix, true),
            "members" => claims::members(&ix),
            _ => queue::claim(&ix),
        });
    }
    let mut explanations = Vec::new();
    let mut witness_records = Vec::new();
    for c in &claims {
        if c.status != Status::Violated {
            continue;
        }
        let mut ops = c.witness.ops.clone();
        ops.sort_by_key(|&i| h.ops[i].inv.pos);
        let mut lines: Vec<String> = ops.iter().map(|&i| op_line(&h, i)).collect();
        let mut recs: Vec<PyVal> = ops.iter().map(|&i| op_json(&h, i)).collect();
        for &i in &c.witness.drained {
            let dr = &h.drained[i];
            lines.push(format!(
                "  process {} drained {} {} (t={}s, position {})",
                dr.proc_,
                hist::enc(&dr.q),
                hist::enc(&dr.g),
                secs(dr.at.shown),
                dr.at.pos
            ));
            recs.push(PyVal::Dict(vec![
                ("process".into(), PyVal::str(&dr.proc_)),
                ("op".into(), PyVal::str("drained")),
                ("channel".into(), PyVal::str(&dr.q)),
                ("group".into(), PyVal::str(&dr.g)),
                (
                    "position".into(),
                    PyVal::Int(BigInt::from_i64(dr.at.pos as i64)),
                ),
            ]));
        }
        explanations.push((c.name.to_string(), lines.join("\n")));
        witness_records.push((c.name.to_string(), PyVal::List(recs)));
    }
    let mut warnings = h.warnings.clone();
    for c in &claims {
        if c.status == Status::Held && c.judged == 0 {
            warnings.push(format!(
                "claim {}: nothing to judge ({} undecided); it holds vacuously",
                c.name, c.undecided
            ));
        }
    }
    Report {
        measures: measures::measures(&ix),
        declaration: decl::to_pyval(d),
        ops: h.ops.len() + h.drained.len(),
        claims,
        warnings,
        explanations,
        witness_records,
    }
}
