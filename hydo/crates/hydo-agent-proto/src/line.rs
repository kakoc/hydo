//! The two kinds of line and their envelopes.
//!
//! A **host line** (host to agent, one canonical JSON text and `\n` on the serial input):
//! `{"v":1,"id":<n>,"op":"<op>", <the command's fields>}`.
//!
//! An **agent line** (framed on the console, [`crate::frame`]) is one of
//! - a reply: `{"v":1,"seq":<n>,"re":<id>,"op":"<op>","ok":true, <the result's fields>}`,
//! - a refusal: `{"v":1,"seq":<n>,"ok":false,"err":"<code>","msg":"<words>"}`, with `re` and
//!   `op` when the agent could read them, and `errno` for a failed system call,
//! - an event: `{"v":1,"seq":<n>,"ev":"<name>", <the event's fields>}`.
//!
//! `seq` counts every agent line from 0 (the hello), so a lost line is seen.
//!
//! Decoding is strict for host lines (the agent refuses an unknown field: ignoring it would
//! change the meaning of a run silently) and lenient for agent lines (the host ignores unknown
//! fields and reports an unknown event or error code as such), so that an agent built later with
//! additions of the same version is still read.

use crate::field::FieldError;
use crate::json::{self, Obj, Value};
use crate::msg::{Command, ErrCode, Event, Reply, PROTO};

/// A command with its envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLine {
    /// The protocol version the command is written in.
    pub v: u64,
    /// The command's id; ids increase.
    pub id: u64,
    pub cmd: Command,
}

/// Why the agent refuses a host line: what goes into its refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub re: Option<u64>,
    pub op: Option<String>,
    pub err: ErrCode,
    pub msg: String,
}

/// A refusal on the wire, as the host reads it (the code is kept as text, so that a code added
/// later is still read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorLine {
    pub v: u64,
    pub seq: u64,
    pub re: Option<u64>,
    pub op: Option<String>,
    pub err: String,
    pub msg: String,
    pub errno: Option<i64>,
}

impl ErrorLine {
    /// The code, if this version knows it.
    pub fn code(&self) -> Option<ErrCode> {
        ErrCode::from_name(&self.err)
    }
}

/// A line from the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLine {
    Reply {
        v: u64,
        seq: u64,
        re: u64,
        body: Reply,
    },
    Error(ErrorLine),
    Event {
        v: u64,
        seq: u64,
        ev: Event,
    },
    /// An event this version does not know (from an agent with later additions).
    UnknownEvent {
        v: u64,
        seq: u64,
        ev: String,
        fields: Obj,
    },
}

impl AgentLine {
    pub fn seq(&self) -> u64 {
        match self {
            AgentLine::Reply { seq, .. }
            | AgentLine::Event { seq, .. }
            | AgentLine::UnknownEvent { seq, .. } => *seq,
            AgentLine::Error(e) => e.seq,
        }
    }

    pub fn v(&self) -> u64 {
        match self {
            AgentLine::Reply { v, .. }
            | AgentLine::Event { v, .. }
            | AgentLine::UnknownEvent { v, .. } => *v,
            AgentLine::Error(e) => e.v,
        }
    }
}

fn int(v: u64) -> Value {
    Value::Int(v as i128)
}

fn s(x: &str) -> Value {
    Value::Str(x.to_string())
}

impl HostLine {
    /// A version-1 command.
    pub fn new(id: u64, cmd: Command) -> Self {
        HostLine { v: PROTO, id, cmd }
    }

    /// The canonical JSON text (no newline).
    pub fn encode(&self) -> String {
        let mut m = Obj::new();
        self.cmd.put(&mut m);
        m.insert("v".into(), int(self.v));
        m.insert("id".into(), int(self.id));
        m.insert("op".into(), s(self.cmd.name()));
        Value::Obj(m).to_canonical()
    }

    /// Reads a host line as the agent does: strictly. The refusal names what is wrong and, when
    /// they could be read, the id and the op.
    pub fn decode(text: &[u8], speaks: &[u64]) -> Result<HostLine, Refusal> {
        let refuse = |re, op: Option<&str>, err, msg: String| Refusal {
            re,
            op: op.map(String::from),
            err,
            msg,
        };
        let v =
            json::parse(text).map_err(|e| refuse(None, None, ErrCode::BadJson, e.to_string()))?;
        let Value::Obj(mut m) = v else {
            return Err(refuse(
                None,
                None,
                ErrCode::BadLine,
                format!("{} is not a command object", v.kind()),
            ));
        };
        let id = match m.remove("id") {
            Some(Value::Int(n)) if (0..=u64::MAX as i128).contains(&n) => n as u64,
            Some(x) => {
                return Err(refuse(
                    None,
                    None,
                    ErrCode::BadLine,
                    format!("id: {} where a whole number was expected", x.kind()),
                ))
            }
            None => return Err(refuse(None, None, ErrCode::BadLine, "no id".into())),
        };
        let op = match m.remove("op") {
            Some(Value::Str(x)) => x,
            Some(x) => {
                return Err(refuse(
                    Some(id),
                    None,
                    ErrCode::BadLine,
                    format!("op: {} where a string was expected", x.kind()),
                ))
            }
            None => return Err(refuse(Some(id), None, ErrCode::BadLine, "no op".into())),
        };
        let v = match m.remove("v") {
            Some(Value::Int(n)) if n >= 0 && speaks.contains(&(n as u64)) => n as u64,
            Some(Value::Int(n)) => {
                return Err(refuse(
                    Some(id),
                    Some(&op),
                    ErrCode::BadVersion,
                    format!("protocol version {n}; this agent speaks {}", list(speaks)),
                ))
            }
            Some(x) => {
                return Err(refuse(
                    Some(id),
                    Some(&op),
                    ErrCode::BadLine,
                    format!("v: {} where a whole number was expected", x.kind()),
                ))
            }
            None => return Err(refuse(Some(id), Some(&op), ErrCode::BadLine, "no v".into())),
        };
        let cmd = match Command::take(&op, &mut m) {
            None => {
                return Err(refuse(
                    Some(id),
                    Some(&op),
                    ErrCode::UnknownOp,
                    format!("unknown op {op:?}"),
                ))
            }
            Some(Err(FieldError { field, what })) => {
                return Err(refuse(
                    Some(id),
                    Some(&op),
                    ErrCode::BadField,
                    format!("{field}: {what}"),
                ))
            }
            Some(Ok(c)) => c,
        };
        if let Some(k) = m.keys().next() {
            return Err(refuse(
                Some(id),
                Some(&op),
                ErrCode::BadField,
                format!("{k}: unknown field of {op}"),
            ));
        }
        Ok(HostLine { v, id, cmd })
    }
}

fn list(v: &[u64]) -> String {
    v.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")
}

impl AgentLine {
    /// The canonical JSON text (no newline, no frame).
    pub fn encode(&self) -> String {
        let mut m = Obj::new();
        match self {
            AgentLine::Reply { v, seq, re, body } => {
                body.put(&mut m);
                m.insert("v".into(), int(*v));
                m.insert("seq".into(), int(*seq));
                m.insert("re".into(), int(*re));
                m.insert("op".into(), s(body.name()));
                m.insert("ok".into(), Value::Bool(true));
            }
            AgentLine::Error(e) => {
                m.insert("v".into(), int(e.v));
                m.insert("seq".into(), int(e.seq));
                m.insert("ok".into(), Value::Bool(false));
                if let Some(re) = e.re {
                    m.insert("re".into(), int(re));
                }
                if let Some(op) = &e.op {
                    m.insert("op".into(), s(op));
                }
                m.insert("err".into(), s(&e.err));
                m.insert("msg".into(), s(&e.msg));
                if let Some(n) = e.errno {
                    m.insert("errno".into(), Value::Int(n as i128));
                }
            }
            AgentLine::Event { v, seq, ev } => {
                ev.put(&mut m);
                m.insert("v".into(), int(*v));
                m.insert("seq".into(), int(*seq));
                m.insert("ev".into(), s(ev.name()));
            }
            AgentLine::UnknownEvent { v, seq, ev, fields } => {
                m = fields.clone();
                m.insert("v".into(), int(*v));
                m.insert("seq".into(), int(*seq));
                m.insert("ev".into(), s(ev));
            }
        }
        Value::Obj(m).to_canonical()
    }

    /// Reads an agent line as the host does: unknown fields are ignored, an unknown event is
    /// [`AgentLine::UnknownEvent`]; a reply needs `op` to be read (its fields depend on it).
    pub fn decode(text: &str) -> Result<AgentLine, String> {
        let v = json::parse(text.as_bytes()).map_err(|e| e.to_string())?;
        let Value::Obj(mut m) = v else {
            return Err(format!("{} is not an agent line", v.kind()));
        };
        let v = take_u64(&mut m, "v")?.ok_or("no v")?;
        let seq = take_u64(&mut m, "seq")?.ok_or("no seq")?;
        if let Some(ev) = m.remove("ev") {
            let Value::Str(ev) = ev else {
                return Err(format!("ev: {} where a string was expected", ev.kind()));
            };
            return match Event::take(&ev, &mut m) {
                None => Ok(AgentLine::UnknownEvent {
                    v,
                    seq,
                    ev,
                    fields: m,
                }),
                Some(Ok(ev)) => Ok(AgentLine::Event { v, seq, ev }),
                Some(Err(e)) => Err(format!("event {ev}: {}: {}", e.field, e.what)),
            };
        }
        let ok = match m.remove("ok") {
            Some(Value::Bool(b)) => b,
            Some(x) => return Err(format!("ok: {} where a boolean was expected", x.kind())),
            None => return Err("neither ev nor ok".into()),
        };
        let re = take_u64(&mut m, "re")?;
        let op = match m.remove("op") {
            None => None,
            Some(Value::Str(x)) => Some(x),
            Some(x) => return Err(format!("op: {} where a string was expected", x.kind())),
        };
        if ok {
            let re = re.ok_or("a reply without re")?;
            let op = op.ok_or("a reply without op")?;
            return match Reply::take(&op, &mut m) {
                None => Err(format!("a reply to an unknown op {op:?}")),
                Some(Ok(body)) => Ok(AgentLine::Reply { v, seq, re, body }),
                Some(Err(e)) => Err(format!("reply to {op}: {}: {}", e.field, e.what)),
            };
        }
        let str_of = |m: &mut Obj, k: &str| -> Result<String, String> {
            match m.remove(k) {
                Some(Value::Str(x)) => Ok(x),
                Some(x) => Err(format!("{k}: {} where a string was expected", x.kind())),
                None => Err(format!("a refusal without {k}")),
            }
        };
        let err = str_of(&mut m, "err")?;
        let msg = str_of(&mut m, "msg")?;
        let errno = match m.remove("errno") {
            None => None,
            Some(Value::Int(n)) if n >= i64::MIN as i128 && n <= i64::MAX as i128 => Some(n as i64),
            Some(x) => return Err(format!("errno: {} where an integer was expected", x.kind())),
        };
        Ok(AgentLine::Error(ErrorLine {
            v,
            seq,
            re,
            op,
            err,
            msg,
            errno,
        }))
    }
}

fn take_u64(m: &mut Obj, k: &str) -> Result<Option<u64>, String> {
    match m.remove(k) {
        None => Ok(None),
        Some(Value::Int(n)) if (0..=u64::MAX as i128).contains(&n) => Ok(Some(n as u64)),
        Some(x) => Err(format!(
            "{k}: {} where a whole number was expected",
            x.kind()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::*;

    #[test]
    fn host_lines_round_trip_and_are_strict() {
        let l = HostLine::new(
            7,
            Command::Signal(Signal {
                h: "redis".into(),
                sig: Sig::Kill,
                tree: true,
                wait_stopped: None,
            }),
        );
        let t = l.encode();
        assert_eq!(
            t,
            r#"{"h":"redis","id":7,"op":"signal","sig":"KILL","tree":true,"v":1}"#
        );
        assert_eq!(HostLine::decode(t.as_bytes(), &[1]).unwrap(), l);
        let bad = |t: &str| HostLine::decode(t.as_bytes(), &[1]).unwrap_err();
        let r = bad(r#"{"h":"redis","id":7,"op":"signal","sig":"KILL","tree":true,"v":1,"x":1}"#);
        assert_eq!(
            (r.re, r.op.as_deref(), r.err),
            (Some(7), Some("signal"), ErrCode::BadField)
        );
        assert_eq!(r.msg, "x: unknown field of signal");
        let r = bad(r#"{"h":"redis","id":7,"op":"signal","sig":"KIL","tree":true,"v":1}"#);
        assert_eq!(r.err, ErrCode::BadField);
        assert!(
            r.msg.starts_with("sig: \"KIL\" is not one of KILL, TERM"),
            "{}",
            r.msg
        );
        assert_eq!(bad(r#"{"id":8,"op":"fly","v":1}"#).err, ErrCode::UnknownOp);
        assert_eq!(
            bad(r#"{"id":8,"op":"syncfs","v":2}"#).err,
            ErrCode::BadVersion
        );
        assert_eq!(bad(r#"{"id":8,"op":"syncfs"}"#).err, ErrCode::BadLine);
        assert_eq!(bad(r#"{"op":"syncfs","v":1}"#).re, None);
        assert_eq!(bad("[1]").err, ErrCode::BadLine);
        assert_eq!(bad("{").err, ErrCode::BadJson);
        assert_eq!(
            bad(r#"{"id":8,"op":"spawn","v":1,"argv":["x"]}"#).msg,
            "h: missing"
        );
    }

    #[test]
    fn agent_lines_round_trip_and_are_lenient() {
        let lines = [
            AgentLine::Reply {
                v: 1,
                seq: 3,
                re: 7,
                body: Reply::Signal(SignalOk {
                    procs: 2,
                    stopped: None,
                }),
            },
            AgentLine::Event {
                v: 1,
                seq: 4,
                ev: Event::Exit(Exit {
                    h: "redis".into(),
                    id: 2,
                    status: 137,
                    sig: Some(9),
                    timed_out: None,
                }),
            },
            AgentLine::Error(ErrorLine {
                v: 1,
                seq: 5,
                re: Some(8),
                op: Some("signal".into()),
                err: "no-such-handle".into(),
                msg: "no process redis".into(),
                errno: None,
            }),
        ];
        for l in &lines {
            assert_eq!(&AgentLine::decode(&l.encode()).unwrap(), l);
        }
        assert_eq!(
            lines[0].encode(),
            r#"{"ok":true,"op":"signal","procs":2,"re":7,"seq":3,"v":1}"#
        );
        // a later addition: an unknown field, an unknown event, an unknown code
        let l = AgentLine::decode(
            r#"{"ok":true,"op":"signal","procs":2,"re":7,"seq":3,"v":1,"new":1}"#,
        )
        .unwrap();
        assert_eq!(l, lines[0]);
        let u = AgentLine::decode(r#"{"ev":"later","seq":9,"v":1,"x":1}"#).unwrap();
        assert!(matches!(u, AgentLine::UnknownEvent { ref ev, .. } if ev == "later"));
        assert_eq!(AgentLine::decode(&u.encode()).unwrap(), u);
        let AgentLine::Error(e) =
            AgentLine::decode(r#"{"err":"later","msg":"m","ok":false,"seq":1,"v":1}"#).unwrap()
        else {
            panic!()
        };
        assert_eq!((e.code(), e.re), (None, None));
        assert!(AgentLine::decode(r#"{"ok":true,"re":1,"seq":1,"v":1}"#).is_err());
    }
}
