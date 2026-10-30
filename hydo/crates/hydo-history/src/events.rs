//! Reading the consoles: HYDO-HIST records into events, HYDO-CHECK / HYDO-CHECK-FINAL lines into
//! check specs, and events into operations (an invocation and maybe its completion).

use crate::py::{self, BigInt, Crash, KeyRepr, PyVal};
use std::collections::HashMap;

pub const TAG: &str = "HYDO-HIST ";
pub const CHECK_TAG: &str = "HYDO-CHECK ";
pub const FINAL_TAG: &str = "HYDO-CHECK-FINAL ";

/// The record types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Invoke,
    Ok,
    Fail,
    Info,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "invoke" => Kind::Invoke,
            "ok" => Kind::Ok,
            "fail" => Kind::Fail,
            "info" => Kind::Info,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Invoke => "invoke",
            Kind::Ok => "ok",
            Kind::Fail => "fail",
            Kind::Info => "info",
        }
    }
}

/// One HYDO-HIST record. `idx` is its position over all consoles (the order that counts).
#[derive(Debug, Clone)]
pub struct Event {
    pub idx: usize,
    pub hist: String,
    pub proc_: String,
    pub kind: Kind,
    /// Guest time in ns (None for "-").
    pub time: Option<BigInt>,
    pub f: String,
    pub args: Vec<String>,
    /// Index into [`Events::labels`]: the console it came from.
    pub src: usize,
    /// Line number in that console (from 1).
    pub line: usize,
}

/// Everything read from the consoles of one run.
#[derive(Debug, Clone, Default)]
pub struct Events {
    pub events: Vec<Event>,
    /// The console labels, in run order (e.g. "one", "one.recovery").
    pub labels: Vec<String>,
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

/// `hydohist.parse_sources`: sources = [(label, text)] in run order (workload console first,
/// recovery console second). Returns the events, the check specs (HYDO-CHECK lines, the first of
/// each id; `_final_rc` = the exit status its final: command reported in a HYDO-CHECK-FINAL
/// line) and the malformed-line warnings.
pub fn parse_sources<S: AsRef<str>, T: AsRef<str>>(
    sources: &[(S, T)],
) -> Result<(Events, Vec<PyVal>, Vec<String>), Crash> {
    let mut ev = Events::default();
    let mut warn = Vec::new();
    let mut specs: Vec<(KeyRepr, PyVal)> = Vec::new();
    let mut finals: Vec<(KeyRepr, BigInt)> = Vec::new();
    for (si, (label, text)) in sources.iter().enumerate() {
        let label = label.as_ref();
        ev.labels.push(label.to_string());
        for (ln0, line) in py::splitlines(text.as_ref()).into_iter().enumerate() {
            let ln = ln0 + 1;
            if let Some(k) = line.find(FINAL_TAG) {
                match py::json_loads(py::strip(&line[k + FINAL_TAG.len()..])) {
                    Ok(fr) => {
                        // int(fr.get("rc", 0)) runs first, then fr["id"], then hashing it
                        if !fr.is_dict() {
                            return Err(Crash(format!(
                                "AttributeError: '{}' object has no attribute 'get'",
                                fr.type_name()
                            )));
                        }
                        let rc = match fr.get("rc") {
                            None => Some(BigInt::from_i64(0)),
                            Some(v) => v.int_of()?,
                        };
                        let key = fr.get("id").map(PyVal::key);
                        match (rc, key) {
                            (Some(rc), Some(Ok(key))) => {
                                if let Some(e) = finals.iter_mut().find(|(k, _)| *k == key) {
                                    e.1 = rc;
                                } else {
                                    finals.push((key, rc));
                                }
                            }
                            _ => warn.push(format!("{label}:{ln}: bad HYDO-CHECK-FINAL line")),
                        }
                    }
                    Err(py::JsonError) => {
                        warn.push(format!("{label}:{ln}: bad HYDO-CHECK-FINAL line"))
                    }
                }
                continue;
            }
            if let Some(k) = line.find(CHECK_TAG) {
                match py::json_loads(py::strip(&line[k + CHECK_TAG.len()..])) {
                    Ok(sp) => {
                        if let Some(id) = sp.get("id") {
                            let key = id.key()?;
                            if !specs.iter().any(|(k, _)| *k == key) {
                                specs.push((key, sp));
                            }
                        }
                    }
                    Err(py::JsonError) => warn.push(format!("{label}:{ln}: bad HYDO-CHECK line")),
                }
                continue;
            }
            let Some(k) = line.find(TAG) else {
                continue;
            };
            let tok = py::split_ws(&line[k + TAG.len()..]);
            let kind = tok.get(2).and_then(|t| Kind::parse(t));
            let (Some(kind), true) = (kind, tok.len() >= 5 && name_ok(tok[0])) else {
                warn.push(format!("{label}:{ln}: malformed HYDO-HIST record"));
                continue;
            };
            let time = if tok[3] == "-" {
                None
            } else {
                match py::parse_int(tok[3]) {
                    Some(t) => Some(t),
                    None => {
                        warn.push(format!("{label}:{ln}: bad time {}", py::repr_str(tok[3])));
                        continue;
                    }
                }
            };
            let idx = ev.events.len();
            ev.events.push(Event {
                idx,
                hist: tok[0].to_string(),
                proc_: tok[1].to_string(),
                kind,
                time,
                f: tok[4].to_string(),
                args: tok[5..].iter().map(|s| s.to_string()).collect(),
                src: si,
                line: ln,
            });
        }
    }
    for (key, rc) in finals {
        if let Some((_, sp)) = specs.iter_mut().find(|(k, _)| *k == key) {
            sp.set("_final_rc", PyVal::Int(rc));
        }
    }
    Ok((ev, specs.into_iter().map(|(_, s)| s).collect(), warn))
}

/// An operation's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Fail,
    Info,
}

impl Status {
    /// The words the explanations use.
    pub fn words(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Info => "unknown outcome",
            Status::Fail => "failed",
        }
    }
}

/// One operation: an invocation and (maybe) its completion.
#[derive(Debug, Clone)]
pub struct Op {
    pub n: usize,
    pub proc_: String,
    pub f: String,
    pub inv: Vec<String>,
    pub out: Option<Vec<String>>,
    pub status: Status,
    /// Position of the invocation.
    pub call: usize,
    /// Position of the completion; None = never (an unknown outcome).
    pub ret: Option<usize>,
    pub ev_inv: usize,
    pub ev_done: Option<usize>,
}

/// `ret` as a number that compares like Python's INF.
pub fn ret_key(r: Option<usize>) -> usize {
    r.unwrap_or(usize::MAX)
}

impl Op {
    /// The arguments that count: the completion's for an ok operation that has some, else the
    /// invocation's.
    pub fn args(&self) -> &[String] {
        match (&self.out, self.status) {
            (Some(o), Status::Ok) if !o.is_empty() => o,
            _ => &self.inv,
        }
    }

    /// 't=12.34-12.41s' (guest time), with '(recovery boot)' for a console labelled *recovery.
    pub fn where_(&self, ev: &Events) -> Result<String, Crash> {
        let ts = |e: Option<usize>| -> Result<String, Crash> {
            match e.and_then(|i| ev.events[i].time.as_ref()) {
                None => Ok("?".into()),
                Some(t) => match t.to_f64() {
                    Some(f) => Ok(py::fmt_2f(f / 1e9)),
                    None => Err(Crash(
                        "OverflowError: int too large to convert to float".into(),
                    )),
                },
            }
        };
        let mut s = format!("t={}-{}s", ts(Some(self.ev_inv))?, ts(self.ev_done)?);
        let src = &ev.labels[ev.events[self.ev_inv].src];
        if !src.is_empty() && src.ends_with("recovery") {
            s.push_str(" (recovery boot)");
        }
        Ok(s)
    }
}

/// `hydohist.pair_ops`: pair invocations with completions per process (a process has at most one
/// operation in flight). An invocation still open when the same process invokes again, or at the
/// end of the history, has an unknown outcome (info).
pub fn pair_ops(ev: &Events, hist: Option<&PyVal>) -> (Vec<Op>, Vec<String>) {
    let mut ops: Vec<Op> = Vec::new();
    let mut open: HashMap<&str, usize> = HashMap::new();
    let mut warn = Vec::new();
    let hist = hist.and_then(PyVal::as_str);
    for e in &ev.events {
        if Some(e.hist.as_str()) != hist {
            continue;
        }
        if e.kind == Kind::Invoke {
            open.remove(e.proc_.as_str());
            let n = ops.len();
            ops.push(Op {
                n,
                proc_: e.proc_.clone(),
                f: e.f.clone(),
                inv: e.args.clone(),
                out: None,
                status: Status::Info,
                call: e.idx,
                ret: None,
                ev_inv: e.idx,
                ev_done: None,
            });
            open.insert(e.proc_.as_str(), n);
            continue;
        }
        let Some(n) = open.remove(e.proc_.as_str()) else {
            warn.push(format!(
                "{}:{}: {} {} by process {} without an open invocation (ignored)",
                ev.labels[e.src],
                e.line,
                e.kind.name(),
                e.f,
                e.proc_
            ));
            continue;
        };
        let op = &mut ops[n];
        if e.f != op.f {
            warn.push(format!(
                "{}:{}: completion {} does not match the invocation {} (ignored)",
                ev.labels[e.src], e.line, e.f, op.f
            ));
            open.insert(e.proc_.as_str(), n);
            continue;
        }
        op.status = match e.kind {
            Kind::Ok => Status::Ok,
            Kind::Fail => Status::Fail,
            _ => Status::Info,
        };
        op.ev_done = Some(e.idx);
        op.out = Some(e.args.clone());
        op.ret = matches!(e.kind, Kind::Ok | Kind::Fail).then_some(e.idx);
    }
    (ops, warn)
}
