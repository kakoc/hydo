//! The host side of one conversation with an agent.
//!
//! A [`Session`] starts from the agent's hello ([`Session::from_hello`]), which fixes the
//! protocol version and what the agent can do. The version rule: the host reads every protocol
//! version ever released ([`HOST_PROTOCOLS`]) and refuses an agent that speaks a newer one, with a
//! message that names both, because the agent is inside the image and the image was built by a
//! newer hydo. The session then
//! - numbers the host's commands and writes their lines ([`Session::command`]), refusing an op
//!   or a feature the hello did not list, so the agent is never sent something it would refuse;
//! - reads the agent's lines ([`Session::receive`], or [`Session::console_line`] for a raw console
//!   line), checking that their numbers follow each other (a lost line is an error of the run) and
//!   pairing every reply or refusal with the command it answers.
//!
//! It holds no clock and makes no decision: when to send what is the controller's business.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::frame::{deframe_line, Deframed, CAN};
use crate::json::Value;
use crate::line::{AgentLine, ErrorLine, HostLine};
use crate::msg::{Command, Event, Hello, Reply, Sync, HOST_PROTOCOLS};

/// The hydo release this crate belongs to (named in the version message).
pub const HYDO_VERSION: &str = env!("CARGO_PKG_VERSION");

/// What can go wrong in a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// The agent speaks a protocol this host does not read.
    Skew { agent_proto: u64, agent: String },
    /// The first line was not a hello.
    NoHello(String),
    /// An agent line could not be read.
    Decode { line: String, why: String },
    /// A console line had the agent's prefix but a wrong length or CRC.
    Corrupt { line: String, why: &'static str },
    /// An agent line's number is not the next one.
    SeqGap { expected: u64, got: u64 },
    /// A line of another protocol version than the session's.
    WrongVersion { expected: u64, got: u64 },
    /// A reply or refusal for no command in flight.
    Unexpected { re: u64 },
    /// A reply whose op is not the op of the command it names.
    WrongOp { re: u64, sent: String, got: String },
    /// The host asked for an op the hello did not list.
    NotOffered { what: String },
    /// The host asked for a command field the agent announces with a feature its hello did not
    /// list (an agent built before the field was added).
    MissingFeature { feature: String, agent: String },
}

/// The message for an agent whose hello lacks `feature` (also the controller's, when it checks
/// a hello before it sends).
pub fn missing_feature_message(feature: &str, agent: &str) -> String {
    format!(
        "this image's agent (hydo-agent {agent}) does not offer the feature {feature} ({}); \
         rebuild the image with this hydo ({HYDO_VERSION}), whose agent does",
        crate::msg::feature_purpose(feature)
    )
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (lo, hi) = (HOST_PROTOCOLS.start(), HOST_PROTOCOLS.end());
        match self {
            SessionError::Skew { agent_proto, agent } if agent_proto > hi => write!(
                f,
                "this image's agent (hydo-agent {agent}) speaks agent protocol {agent_proto}; this hydo \
                 ({HYDO_VERSION}) knows agent protocol {lo}..{hi}; upgrade hydo"
            ),
            SessionError::Skew { agent_proto, agent } => write!(
                f,
                "this image's agent (hydo-agent {agent}) speaks agent protocol {agent_proto}, which no \
                 hydo release used; this hydo ({HYDO_VERSION}) knows agent protocol {lo}..{hi}"
            ),
            SessionError::NoHello(l) => write!(f, "the agent's first line is not a hello: {l}"),
            SessionError::Decode { line, why } => write!(f, "unreadable agent line ({why}): {line}"),
            SessionError::Corrupt { line, why } => write!(f, "corrupt agent line ({why}): {line}"),
            SessionError::SeqGap { expected, got } => {
                write!(f, "agent line {got} where {expected} was expected (a line was lost)")
            }
            SessionError::WrongVersion { expected, got } => {
                write!(f, "agent line of protocol {got} in a protocol {expected} session")
            }
            SessionError::Unexpected { re } => write!(f, "an answer to command {re}, which is not in flight"),
            SessionError::WrongOp { re, sent, got } => {
                write!(f, "command {re} was {sent}, but the answer is for {got}")
            }
            SessionError::NotOffered { what } => write!(f, "the agent does not offer {what}"),
            SessionError::MissingFeature { feature, agent } => {
                f.write_str(&missing_feature_message(feature, agent))
            }
        }
    }
}

impl std::error::Error for SessionError {}

/// A command ready to type: its id and its bytes (with the newline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub id: u64,
    pub bytes: Vec<u8>,
}

/// What an agent line meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// The answer to command `id`.
    Reply { id: u64, body: Reply },
    /// Command `id` was refused (or a line without a readable id, `id` = None).
    Refused { id: Option<u64>, error: ErrorLine },
    /// An event.
    Event(Event),
    /// An event this version does not know (kept, not understood).
    UnknownEvent { ev: String },
}

/// The host side of a conversation.
#[derive(Debug, Clone)]
pub struct Session {
    hello: Hello,
    proto: u64,
    next_id: u64,
    next_seq: u64,
    in_flight: BTreeMap<u64, String>,
}

impl Session {
    /// Starts a session from the agent's first line (its JSON text). Ids start at 1.
    pub fn from_hello(json: &str) -> Result<Session, SessionError> {
        // A hello of another version may have fields this one cannot read: the version message
        // comes first, from the two members every version keeps (`proto`, `agent`).
        if let Ok(Value::Obj(m)) = crate::json::parse(json.as_bytes()) {
            if let (Some(Value::Str(ev)), Some(Value::Int(p))) = (m.get("ev"), m.get("proto")) {
                let p = (*p).clamp(0, u64::MAX as i128) as u64;
                if ev == "hello" && !HOST_PROTOCOLS.contains(&p) {
                    let agent = match m.get("agent") {
                        Some(Value::Str(a)) => a.clone(),
                        _ => "?".into(),
                    };
                    return Err(SessionError::Skew {
                        agent_proto: p,
                        agent,
                    });
                }
            }
        }
        let line = AgentLine::decode(json).map_err(|why| SessionError::Decode {
            line: json.into(),
            why,
        })?;
        let AgentLine::Event {
            v,
            seq,
            ev: Event::Hello(hello),
        } = line
        else {
            return Err(SessionError::NoHello(json.into()));
        };
        if !HOST_PROTOCOLS.contains(&hello.proto) {
            return Err(SessionError::Skew {
                agent_proto: hello.proto,
                agent: hello.agent,
            });
        }
        if v != hello.proto {
            return Err(SessionError::WrongVersion {
                expected: hello.proto,
                got: v,
            });
        }
        if seq != 0 {
            return Err(SessionError::SeqGap {
                expected: 0,
                got: seq,
            });
        }
        Ok(Session {
            proto: hello.proto,
            hello,
            next_id: 1,
            next_seq: 1,
            in_flight: BTreeMap::new(),
        })
    }

    /// The agent's hello.
    pub fn hello(&self) -> &Hello {
        &self.hello
    }

    /// The protocol version of the session.
    pub fn proto(&self) -> u64 {
        self.proto
    }

    /// Whether the agent executes `op`.
    pub fn offers(&self, op: &str) -> bool {
        self.hello.ops.iter().any(|o| o == op)
    }

    /// Whether the agent announced `feature`.
    pub fn has_feature(&self, feature: &str) -> bool {
        self.hello.features.iter().any(|x| x == feature)
    }

    /// The commands sent and not answered yet, by id.
    pub fn in_flight(&self) -> impl Iterator<Item = (u64, &str)> + '_ {
        self.in_flight.iter().map(|(k, v)| (*k, v.as_str()))
    }

    /// The next command's line.
    pub fn command(&mut self, cmd: Command) -> Result<Outgoing, SessionError> {
        if !self.offers(cmd.name()) {
            return Err(SessionError::NotOffered {
                what: format!("the op {}", cmd.name()),
            });
        }
        for f in cmd.features() {
            if !self.has_feature(f) {
                return Err(SessionError::MissingFeature {
                    feature: f.to_string(),
                    agent: self.hello.agent.clone(),
                });
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        self.in_flight.insert(id, cmd.name().to_string());
        let mut bytes = HostLine {
            v: self.proto,
            id,
            cmd,
        }
        .encode()
        .into_bytes();
        bytes.push(b'\n');
        Ok(Outgoing { id, bytes })
    }

    /// A resynchronisation: a CAN byte (the agent drops a partly read line) and a sync with
    /// `nonce`, to send first after a restore.
    pub fn sync(&mut self, nonce: u64) -> Result<Outgoing, SessionError> {
        let mut o = self.command(Command::Sync(Sync { nonce }))?;
        o.bytes.insert(0, CAN);
        Ok(o)
    }

    /// Continues numbering after a restore from a snapshot taken by another session object:
    /// the next command gets `next_id`, the next agent line must be number `next_seq`.
    pub fn resume_at(&mut self, next_id: u64, next_seq: u64) {
        self.next_id = next_id;
        self.next_seq = next_seq;
        self.in_flight.clear();
    }

    /// Records a command typed by someone else (a transcript being checked, a raw test line):
    /// its answer is then expected. The next command's id follows it.
    pub fn note_sent(&mut self, id: u64, op: &str) {
        self.in_flight.insert(id, op.to_string());
        self.next_id = self.next_id.max(id + 1);
    }

    /// The id the next command gets and the number the next agent line must have.
    pub fn position(&self) -> (u64, u64) {
        (self.next_id, self.next_seq)
    }

    /// Reads one agent line (its JSON text, unframed).
    pub fn receive(&mut self, json: &str) -> Result<Incoming, SessionError> {
        let line = AgentLine::decode(json).map_err(|why| SessionError::Decode {
            line: json.into(),
            why,
        })?;
        if line.v() != self.proto {
            return Err(SessionError::WrongVersion {
                expected: self.proto,
                got: line.v(),
            });
        }
        if line.seq() != self.next_seq {
            return Err(SessionError::SeqGap {
                expected: self.next_seq,
                got: line.seq(),
            });
        }
        self.next_seq += 1;
        Ok(match line {
            AgentLine::Reply { re, body, .. } => {
                let sent = self
                    .in_flight
                    .remove(&re)
                    .ok_or(SessionError::Unexpected { re })?;
                if sent != body.name() {
                    return Err(SessionError::WrongOp {
                        re,
                        sent,
                        got: body.name().into(),
                    });
                }
                Incoming::Reply { id: re, body }
            }
            AgentLine::Error(error) => {
                if let Some(re) = error.re {
                    let sent = self
                        .in_flight
                        .remove(&re)
                        .ok_or(SessionError::Unexpected { re })?;
                    if let Some(op) = &error.op {
                        if *op != sent {
                            return Err(SessionError::WrongOp {
                                re,
                                sent,
                                got: op.clone(),
                            });
                        }
                    }
                }
                Incoming::Refused {
                    id: error.re,
                    error,
                }
            }
            AgentLine::Event { ev, .. } => Incoming::Event(ev),
            AgentLine::UnknownEvent { ev, .. } => Incoming::UnknownEvent { ev },
        })
    }

    /// Reads one console line (without its newline): `None` for text that is not the agent's
    /// (kernel messages, a service's output), otherwise what the agent line meant.
    pub fn console_line(&mut self, line: &[u8]) -> Option<Result<Incoming, SessionError>> {
        match deframe_line(line) {
            Deframed::Other(_) => None,
            Deframed::Corrupt { line, why } => Some(Err(SessionError::Corrupt { line, why })),
            Deframed::Agent(json) => Some(self.receive(&json)),
        }
    }
}

/// The set of ops of protocol version 1, for a hello.
pub fn all_ops() -> Vec<String> {
    Command::NAMES.iter().map(|s| s.to_string()).collect()
}

/// Ops a hello lists that this host does not know (an agent with later additions).
pub fn unknown_ops(hello: &Hello) -> BTreeSet<String> {
    hello
        .ops
        .iter()
        .filter(|o| !Command::NAMES.contains(&o.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::frame;
    use crate::line::AgentLine;
    use crate::msg::*;

    fn hello(proto: u64, ops: &[&str]) -> String {
        AgentLine::Event {
            v: proto,
            seq: 0,
            ev: Event::Hello(Hello {
                proto,
                agent: "0.9.0".into(),
                ops: ops.iter().map(|s| s.to_string()).collect(),
                features: vec![],
                pid1: true,
            }),
        }
        .encode()
    }

    #[test]
    fn a_newer_agent_is_refused_by_name() {
        let e = Session::from_hello(&hello(2, &["sync"])).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "this image's agent (hydo-agent 0.9.0) speaks agent protocol 2; this hydo ({HYDO_VERSION}) \
                 knows agent protocol 1..1; upgrade hydo"
            )
        );
        // a hello of a later version with fields version 1 cannot read is still named
        let e = Session::from_hello(r#"{"agent":"0.9.0","ev":"hello","proto":3,"seq":0,"v":3}"#)
            .unwrap_err();
        assert!(
            matches!(e, SessionError::Skew { agent_proto: 3, .. }),
            "{e}"
        );
        assert!(Session::from_hello(&hello(0, &[])).is_err());
    }

    #[test]
    fn a_field_behind_a_feature_needs_it() {
        let freeze = |netns: Option<&str>| {
            Command::Freeze(Freeze {
                h: "a".into(),
                queue: 7,
                sip: None,
                port: Some(1),
                own_port: None,
                dir: Dir::Req,
                n: 1,
                pattern: None,
                dur_ms: 9,
                log: None,
                netns: netns.map(str::to_string),
            })
        };
        let with = |features: &[&str]| {
            let h = Hello {
                proto: 1,
                agent: "0.1.0".into(),
                ops: vec!["freeze".into()],
                features: features.iter().map(|s| s.to_string()).collect(),
                pid1: true,
            };
            Session::from_hello(
                &AgentLine::Event {
                    v: 1,
                    seq: 0,
                    ev: Event::Hello(h),
                }
                .encode(),
            )
            .unwrap()
        };
        // an agent of milestone 117 (only "freeze") arms a freeze in its own namespace only
        let mut old = with(&["freeze"]);
        assert!(old.command(freeze(None)).is_ok());
        let e = old.command(freeze(Some("/tmp/hydo/ns/a"))).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "this image's agent (hydo-agent 0.1.0) does not offer the feature freeze-netns (the \
                 packet trigger inside a service's network namespace (F= with network: bridge)); \
                 rebuild the image with this hydo ({HYDO_VERSION}), whose agent does"
            )
        );
        assert!(matches!(e, SessionError::MissingFeature { .. }));
        let mut new = with(FEATURES);
        assert!(new.command(freeze(Some("/tmp/hydo/ns/a"))).is_ok());
        // the feature alone is not enough: the field needs the trigger too
        let mut odd = with(&["freeze-netns"]);
        assert!(odd.command(freeze(Some("/x"))).is_err());
    }

    #[test]
    fn commands_replies_and_numbers() {
        let mut s = Session::from_hello(&hello(1, &["sync", "signal", "syncfs"])).unwrap();
        assert!(matches!(
            s.command(Command::Mkdir(Mkdir {
                path: "/x".into(),
                mode: None
            })),
            Err(SessionError::NotOffered { .. })
        ));
        let o = s.sync(5).unwrap();
        assert_eq!(
            o.bytes,
            b"\x18{\"id\":1,\"nonce\":5,\"op\":\"sync\",\"v\":1}\n"
        );
        let o2 = s.command(Command::Syncfs(Syncfs {})).unwrap();
        assert_eq!(o2.id, 2);
        let r = AgentLine::Reply {
            v: 1,
            seq: 1,
            re: 2,
            body: Reply::Syncfs(SyncfsOk {}),
        }
        .encode();
        assert_eq!(
            s.receive(&r).unwrap(),
            Incoming::Reply {
                id: 2,
                body: Reply::Syncfs(SyncfsOk {})
            }
        );
        // the same line again: its number is old
        assert_eq!(
            s.receive(&r).unwrap_err(),
            SessionError::SeqGap {
                expected: 2,
                got: 1
            }
        );
        let framed = frame(
            &AgentLine::Reply {
                v: 1,
                seq: 2,
                re: 1,
                body: Reply::Sync(SyncOk { nonce: 5 }),
            }
            .encode(),
        );
        assert!(s.console_line(b"[ 1.0] kernel").is_none());
        let got = s
            .console_line(framed.trim_end().as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(
            got,
            Incoming::Reply {
                id: 1,
                body: Reply::Sync(SyncOk { nonce: 5 })
            }
        );
        assert_eq!(s.in_flight().count(), 0);
        s.command(Command::Syncfs(Syncfs {})).unwrap();
        let wrong = AgentLine::Reply {
            v: 1,
            seq: 3,
            re: 3,
            body: Reply::Sync(SyncOk { nonce: 1 }),
        }
        .encode();
        assert!(matches!(
            s.receive(&wrong),
            Err(SessionError::WrongOp { re: 3, .. })
        ));
    }
}
