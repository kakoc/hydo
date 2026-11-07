//! The control transcript (docs/design/guest-control.md, "The control transcript"): every command
//! the host typed and every engine action it took, with the number of the stop it happened at and
//! the run's cadence, so that a run can be typed again exactly (M118 found that an hvx guest
//! depends on where the host's run calls return, so times and bytes alone do not define a run).
//!
//! One file per conversation (`control.jsonl` for the run, `control.recovery.jsonl` for a
//! recovery boot, `control.boot.jsonl` for the boot to S1), one canonical JSON object per line
//! (sorted keys, integers only):
//!
//! ```text
//! {"cadence":"rb=16","control":1,"controller":"schedule","conversation":"run","schedule":"disk=none | run K=redis@5+2","start":"resume S1"}
//! {"at":969127,"n":1,"seen":31,"t":"stop","why":"resume"}
//! {"at":969127,"by":"runner","bytes":"\u0018{\"id\":40,\"nonce\":969127,\"op\":\"sync\",\"v\":1}\n","id":40,"n":1,"t":"send"}
//! {"at":969127,"by":"main","bytes":"{\"argv\":[...],...}\n","id":41,"n":1,"t":"send"}
//! {"at":969131,"n":2,"seen":33,"t":"stop","why":"cadence"}
//! {"id":40,"n":2,"send":1,"t":"ack"}
//! {"by":"main","line":"HYDO-ASSERT-DECL {...}","n":2,"t":"report"}
//! {"by":"main","line":"hydo-ob: KILLED redis before step 5","n":7,"t":"note"}
//! {"action":"disk-faults-off","n":40,"t":"engine"}
//! {"n":44,"name":"S1","t":"snap"}
//! {"end":{"verdict":"HYDO-ORACLE: OK invariants=1 held (...)"},"n":52,"t":"end"}
//! ```
//!
//! - The header names the controller, the conversation, the schedule, the cadence (`rb=<n>` from
//!   the schedule's host part, or `default`) and where the conversation started (`boot`, or
//!   `resume <snapshot>`).
//! - A `stop` record precedes the records of every stop at which the host did something; `seen` is
//!   the number of agent lines read by then (a replay checks it, so a divergence is found at the
//!   first stop that differs). Stops at which the host only continued are not written.
//! - `send` has the agent command id, the virtual time it was asked for (`at`; the stop's own time
//!   for "now") and the exact bytes; `ack` the engine's number of the send it acknowledges; `end`
//!   one of `{"verdict": line}`, `{"cut": {"acked": [...], "failed": [...]}}`,
//!   `{"snapshot": name}`.
//! - [`Transcript::hash`] (16 hex digits, `ctl=` on the RESULT line) covers every line except the
//!   notes, which are for people.

use crate::control::{Ending, EngineAction, VTime, Why};
use hydo_agent_proto::json::{self, Obj, Value};
use sha2::{Digest, Sha256};

/// The format version of a transcript file.
pub const FORMAT: u64 = 1;

/// The first line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub controller: String,
    pub conversation: String,
    pub schedule: Option<String>,
    pub cadence: String,
    pub start: String,
}

/// One record after the header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Stop {
        n: u64,
        at: VTime,
        why: Why,
        seen: u64,
    },
    Send {
        n: u64,
        id: u64,
        at: VTime,
        bytes: String,
        by: String,
    },
    Ack {
        n: u64,
        id: u64,
        send: u64,
    },
    Report {
        n: u64,
        by: String,
        line: String,
    },
    Note {
        n: u64,
        by: String,
        line: String,
    },
    Snap {
        n: u64,
        name: String,
    },
    Engine {
        n: u64,
        action: EngineAction,
    },
    End {
        n: u64,
        end: Ending,
    },
}

impl Record {
    /// The stop it belongs to.
    pub fn stop(&self) -> u64 {
        match self {
            Record::Stop { n, .. }
            | Record::Send { n, .. }
            | Record::Ack { n, .. }
            | Record::Report { n, .. }
            | Record::Note { n, .. }
            | Record::Snap { n, .. }
            | Record::Engine { n, .. }
            | Record::End { n, .. } => *n,
        }
    }
}

/// A conversation's transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    pub header: Header,
    pub records: Vec<Record>,
}

fn int(v: u64) -> Value {
    Value::Int(v as i128)
}

fn s(x: &str) -> Value {
    Value::Str(x.to_string())
}

fn strs(x: &[String]) -> Value {
    Value::Arr(x.iter().map(|i| s(i)).collect())
}

fn ending_value(e: &Ending) -> Value {
    let mut m = Obj::new();
    match e {
        Ending::Verdict(v) => {
            m.insert("verdict".into(), s(v));
        }
        Ending::PowerCut {
            acked,
            failed,
            world,
            outcomes,
        } => {
            let mut c = Obj::new();
            c.insert("acked".into(), strs(acked));
            c.insert("failed".into(), strs(failed));
            // an addition (M125): only a cut that carries a fake world has it
            if !world.is_empty() {
                c.insert("world".into(), strs(world));
            }
            // an addition (guest.outcomes_after_cut): only a cut that carries outcomes has it
            if !outcomes.is_empty() {
                c.insert("outcomes".into(), strs(outcomes));
            }
            m.insert("cut".into(), Value::Obj(c));
        }
        Ending::AtSnapshot(n) => {
            m.insert("snapshot".into(), s(n));
        }
    }
    Value::Obj(m)
}

fn ending_of(v: &Value) -> Result<Ending, String> {
    let Value::Obj(m) = v else {
        return Err("end: not an object".into());
    };
    if let Some(Value::Str(x)) = m.get("verdict") {
        return Ok(Ending::Verdict(x.clone()));
    }
    if let Some(Value::Str(x)) = m.get("snapshot") {
        return Ok(Ending::AtSnapshot(x.clone()));
    }
    if let Some(Value::Obj(c)) = m.get("cut") {
        let list = |k: &str| -> Result<Vec<String>, String> {
            match c.get(k) {
                Some(Value::Arr(a)) => a
                    .iter()
                    .map(|x| match x {
                        Value::Str(t) => Ok(t.clone()),
                        _ => Err(format!("end.cut.{k}: not a string")),
                    })
                    .collect(),
                _ => Err(format!("end.cut.{k} missing")),
            }
        };
        return Ok(Ending::PowerCut {
            acked: list("acked")?,
            failed: list("failed")?,
            world: match c.get("world") {
                None => Vec::new(),
                Some(_) => list("world")?,
            },
            outcomes: match c.get("outcomes") {
                None => Vec::new(),
                Some(_) => list("outcomes")?,
            },
        });
    }
    Err("end: no verdict, cut or snapshot".into())
}

impl Header {
    fn value(&self) -> Value {
        let mut m = Obj::new();
        m.insert("control".into(), int(FORMAT));
        m.insert("controller".into(), s(&self.controller));
        m.insert("conversation".into(), s(&self.conversation));
        if let Some(x) = &self.schedule {
            m.insert("schedule".into(), s(x));
        }
        m.insert("cadence".into(), s(&self.cadence));
        m.insert("start".into(), s(&self.start));
        Value::Obj(m)
    }
}

impl Record {
    fn value(&self) -> Value {
        let mut m = Obj::new();
        let (t, n) = match self {
            Record::Stop { n, at, why, seen } => {
                m.insert("at".into(), int(*at));
                m.insert("why".into(), s(why.name()));
                m.insert("seen".into(), int(*seen));
                ("stop", n)
            }
            Record::Send {
                n,
                id,
                at,
                bytes,
                by,
            } => {
                m.insert("id".into(), int(*id));
                m.insert("at".into(), int(*at));
                m.insert("bytes".into(), s(bytes));
                m.insert("by".into(), s(by));
                ("send", n)
            }
            Record::Ack { n, id, send } => {
                m.insert("id".into(), int(*id));
                m.insert("send".into(), int(*send));
                ("ack", n)
            }
            Record::Report { n, by, line } => {
                m.insert("by".into(), s(by));
                m.insert("line".into(), s(line));
                ("report", n)
            }
            Record::Note { n, by, line } => {
                m.insert("by".into(), s(by));
                m.insert("line".into(), s(line));
                ("note", n)
            }
            Record::Snap { n, name } => {
                m.insert("name".into(), s(name));
                ("snap", n)
            }
            Record::Engine { n, action } => {
                m.insert("action".into(), s(action.name()));
                ("engine", n)
            }
            Record::End { n, end } => {
                m.insert("end".into(), ending_value(end));
                ("end", n)
            }
        };
        m.insert("t".into(), s(t));
        m.insert("n".into(), int(*n));
        Value::Obj(m)
    }

    fn of(v: &Value) -> Result<Record, String> {
        let Value::Obj(m) = v else {
            return Err("a record is an object".into());
        };
        let num = |k: &str| -> Result<u64, String> {
            match m.get(k) {
                Some(Value::Int(i)) if *i >= 0 && *i <= u64::MAX as i128 => Ok(*i as u64),
                _ => Err(format!("{k}: not a number")),
            }
        };
        let text = |k: &str| -> Result<String, String> {
            match m.get(k) {
                Some(Value::Str(x)) => Ok(x.clone()),
                _ => Err(format!("{k}: not a string")),
            }
        };
        let n = num("n")?;
        Ok(match text("t")?.as_str() {
            "stop" => Record::Stop {
                n,
                at: num("at")?,
                why: Why::from_name(&text("why")?).ok_or("why: unknown")?,
                seen: num("seen")?,
            },
            "send" => Record::Send {
                n,
                id: num("id")?,
                at: num("at")?,
                bytes: text("bytes")?,
                by: text("by")?,
            },
            "ack" => Record::Ack {
                n,
                id: num("id")?,
                send: num("send")?,
            },
            "report" => Record::Report {
                n,
                by: text("by")?,
                line: text("line")?,
            },
            "note" => Record::Note {
                n,
                by: text("by")?,
                line: text("line")?,
            },
            "snap" => Record::Snap {
                n,
                name: text("name")?,
            },
            "engine" => Record::Engine {
                n,
                action: EngineAction::from_name(&text("action")?).ok_or("action: unknown")?,
            },
            "end" => Record::End {
                n,
                end: ending_of(m.get("end").ok_or("end missing")?)?,
            },
            other => return Err(format!("unknown record type {other}")),
        })
    }
}

impl Transcript {
    pub fn new(header: Header) -> Transcript {
        Transcript {
            header,
            records: Vec::new(),
        }
    }

    /// The file's text: the header, then one line per record.
    pub fn to_jsonl(&self) -> String {
        let mut out = self.header.value().to_canonical();
        out.push('\n');
        for r in &self.records {
            out.push_str(&r.value().to_canonical());
            out.push('\n');
        }
        out
    }

    /// Reads a transcript file.
    pub fn parse(text: &str) -> Result<Transcript, String> {
        let mut lines = text.lines();
        let h = lines.next().ok_or("an empty transcript")?;
        let hv = json::parse(h.as_bytes()).map_err(|e| format!("header: {e}"))?;
        let Value::Obj(m) = &hv else {
            return Err("header: not an object".into());
        };
        match m.get("control") {
            Some(Value::Int(v)) if *v == FORMAT as i128 => {}
            Some(Value::Int(v)) => {
                return Err(format!(
                    "a control transcript of format {v}; this hydo reads format {FORMAT}"
                ))
            }
            _ => return Err("header: not a control transcript".into()),
        }
        let text_of = |k: &str| -> Result<String, String> {
            match m.get(k) {
                Some(Value::Str(x)) => Ok(x.clone()),
                _ => Err(format!("header.{k}: not a string")),
            }
        };
        let header = Header {
            controller: text_of("controller")?,
            conversation: text_of("conversation")?,
            schedule: text_of("schedule").ok(),
            cadence: text_of("cadence")?,
            start: text_of("start")?,
        };
        let mut records = Vec::new();
        for (i, l) in lines.enumerate() {
            if l.is_empty() {
                continue;
            }
            let v = json::parse(l.as_bytes()).map_err(|e| format!("line {}: {e}", i + 2))?;
            records.push(Record::of(&v).map_err(|e| format!("line {}: {e}", i + 2))?);
        }
        Ok(Transcript { header, records })
    }

    /// The transcript's hash: SHA-256 over every canonical line except the notes, first 16 hex
    /// digits.
    pub fn hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.header.value().to_canonical().as_bytes());
        h.update(b"\n");
        for r in &self.records {
            if matches!(r, Record::Note { .. }) {
                continue;
            }
            h.update(r.value().to_canonical().as_bytes());
            h.update(b"\n");
        }
        h.finalize()[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// The agent commands typed, in order.
    pub fn sends(&self) -> impl Iterator<Item = &Record> {
        self.records
            .iter()
            .filter(|r| matches!(r, Record::Send { .. }))
    }

    /// The report lines, in order.
    pub fn report_lines(&self) -> Vec<&str> {
        self.records
            .iter()
            .filter_map(|r| match r {
                Record::Report { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect()
    }

    /// How the conversation ended, if it did.
    pub fn ending(&self) -> Option<&Ending> {
        self.records.iter().rev().find_map(|r| match r {
            Record::End { end, .. } => Some(end),
            _ => None,
        })
    }

    /// The summary for the run's result document (hydo-results' `control`), with the
    /// transcript's file name relative to the document.
    pub fn to_results(&self, file: Option<&str>) -> hydo_results::Control {
        hydo_results::Control {
            conversation: self.header.conversation.clone(),
            controller: self.header.controller.clone(),
            cadence: self.header.cadence.clone(),
            hash: self.hash(),
            sends: self.sends().count() as u64,
            stops: self
                .records
                .iter()
                .filter(|r| matches!(r, Record::Stop { .. }))
                .count() as u64,
            transcript: file.map(str::to_string),
        }
    }
}
