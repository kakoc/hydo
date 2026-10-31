//! Drives one conversation: reads the engine's stops, turns the console lines into what the
//! controller sees (through the agent session of `hydo-agent-proto`, with its version rule and
//! line numbering), acknowledges every command the agent answered, asks the controller what to
//! do, carries it out on the engine, and writes the control transcript.
//!
//! The engine is behind [`Link`] (the lock-step protocol of M118/M119; `crate::lockstep_link`
//! adapts `hydo_engine::lockstep::Lockstep`, and the tests use a scripted engine and agent). The
//! runner decides nothing about the run: after a restore it sends the protocol's
//! resynchronisation (a CAN byte and a `sync`) before the controller's first command, and it
//! acknowledges a send to the engine at the stop where the agent's answer to it is seen (the
//! proof that its bytes left the serial queue, M118's snapshot rule).

use crate::control::{Action, Controller, EngineAction, Ending, Out, Seen, StopInfo, VTime, When};
use crate::transcript::{Header, Record, Transcript};
use hydo_agent_proto::frame::{deframe_line, Deframed};
use hydo_agent_proto::line::{AgentLine, HostLine};
use hydo_agent_proto::msg::{Command, Event};
use hydo_agent_proto::{Incoming, Session};
use std::collections::BTreeMap;
use std::fmt;

/// An engine error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkError(pub String);

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LinkError {}

/// What the engine reported next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkNext {
    /// A stop, with the complete console lines written since the previous one.
    Stop { stop: StopInfo, lines: Vec<Vec<u8>> },
    /// The run ended without the host ending it (the engine's RESULT line, or why).
    Done(String),
}

/// One controlled run on an engine, in lock step.
pub trait Link {
    fn next_stop(&mut self) -> Result<LinkNext, LinkError>;
    /// Type `bytes` at the first stop at or after `at`; the engine's number of the send.
    fn send(&mut self, at: VTime, bytes: &[u8]) -> Result<u64, LinkError>;
    /// The guest has read send `send`.
    fn ack(&mut self, send: u64) -> Result<(), LinkError>;
    fn snap(&mut self, name: &str) -> Result<(), LinkError>;
    fn engine(&mut self, a: &EngineAction) -> Result<(), LinkError>;
    /// Run on to the next stop.
    fn cont(&mut self) -> Result<(), LinkError>;
    /// End the run here; the engine's RESULT line.
    fn end(&mut self, e: &Ending) -> Result<String, LinkError>;
}

/// Where the agent conversation stands at a snapshot: what a conversation that resumes from it
/// needs (the hello fixes the protocol version and the ops; the numbers continue).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPoint {
    pub snapshot: String,
    /// The hello as the agent sent it (its JSON text).
    pub hello: String,
    pub next_id: u64,
    pub next_seq: u64,
}

/// How the conversation starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Start {
    /// A fresh boot: the agent's hello comes first.
    Boot,
    /// From a snapshot: the runner resynchronises first.
    Resume(AgentPoint),
}

/// What a conversation gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub ending: Option<Ending>,
    pub transcript: Transcript,
    /// The engine's RESULT line (or why the run ended).
    pub result: String,
    /// The agent's position at each snapshot taken.
    pub snapshots: Vec<AgentPoint>,
}

/// Why a conversation could not be carried out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunError {
    Link(LinkError),
    /// The controller asked for something the protocol does not allow (an op the agent does not
    /// offer, a command before the hello, a command id out of order).
    Protocol(String),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Link(e) => write!(f, "engine: {e}"),
            RunError::Protocol(e) => write!(f, "control: {e}"),
        }
    }
}

impl std::error::Error for RunError {}

impl From<LinkError> for RunError {
    fn from(e: LinkError) -> RunError {
        RunError::Link(e)
    }
}

/// The cadence as the transcript names it: the schedule's `rb=` host token, or `default`.
pub fn cadence_of(schedule: Option<&hydo_schedule::Schedule>) -> String {
    schedule
        .and_then(|s| s.host_token("rb"))
        .map(|t| t.to_string())
        .unwrap_or_else(|| "default".into())
}

fn hello_text(s: &Session) -> String {
    AgentLine::Event {
        v: s.proto(),
        seq: 0,
        ev: Event::Hello(s.hello().clone()),
    }
    .encode()
}

struct Writer {
    t: Transcript,
    pending_stop: Option<Record>,
}

impl Writer {
    fn push(&mut self, r: Record) {
        if let Some(s) = self.pending_stop.take() {
            self.t.records.push(s);
        }
        self.t.records.push(r);
    }
}

/// Carry out one conversation. The header's `controller` is the controller's name.
pub fn run(
    link: &mut dyn Link,
    ctl: &mut dyn Controller,
    start: Start,
    mut header: Header,
) -> Result<Finished, RunError> {
    header.controller = ctl.name().to_string();
    let mut session: Option<Session> = None;
    let mut need_sync = false;
    if let Start::Resume(p) = &start {
        let mut s = Session::from_hello(&p.hello)
            .map_err(|e| RunError::Protocol(format!("the snapshot's hello: {e}")))?;
        s.resume_at(p.next_id, p.next_seq);
        session = Some(s);
        need_sync = true;
    }
    let mut w = Writer {
        t: Transcript::new(header),
        pending_stop: None,
    };
    let mut send_of: BTreeMap<u64, u64> = BTreeMap::new();
    let mut snapshots = Vec::new();
    loop {
        let (stop, lines) = match link.next_stop()? {
            LinkNext::Done(r) => {
                return Ok(Finished {
                    ending: None,
                    transcript: w.t,
                    result: r,
                    snapshots,
                })
            }
            LinkNext::Stop { stop, lines } => (stop, lines),
        };
        let mut seen = Vec::new();
        let mut acks = Vec::new();
        for l in &lines {
            let item = match &mut session {
                None => match deframe_line(l) {
                    Deframed::Other(t) => Seen::Console(t),
                    Deframed::Corrupt { line, why } => Seen::Broken(format!("{why}: {line}")),
                    Deframed::Agent(json) => match Session::from_hello(&json) {
                        Ok(s) => {
                            let h = s.hello().clone();
                            session = Some(s);
                            Seen::Hello(h)
                        }
                        Err(e) => Seen::Broken(e.to_string()),
                    },
                },
                Some(s) => match s.console_line(l) {
                    None => Seen::Console(String::from_utf8_lossy(l).into_owned()),
                    Some(Err(e)) => Seen::Broken(e.to_string()),
                    Some(Ok(Incoming::Reply { id, body })) => {
                        acks.push(id);
                        Seen::Reply { id, body }
                    }
                    Some(Ok(Incoming::Refused { id: Some(id), error })) => {
                        acks.push(id);
                        Seen::Refused { id, error }
                    }
                    Some(Ok(Incoming::Refused { id: None, error })) => Seen::Broken(format!(
                        "the agent refused a line it could not read: {} {}",
                        error.err, error.msg
                    )),
                    Some(Ok(Incoming::Event(ev))) => Seen::Event(ev),
                    Some(Ok(Incoming::UnknownEvent { ev })) => Seen::UnknownEvent(ev),
                },
            };
            seen.push(item);
        }
        let seen_n = session.as_ref().map(|s| s.position().1).unwrap_or(0);
        let stop = StopInfo {
            seen: seen_n,
            ..stop
        };
        w.pending_stop = Some(Record::Stop {
            n: stop.n,
            at: stop.at,
            why: stop.why,
            seen: seen_n,
        });
        for id in acks {
            if let Some(eid) = send_of.remove(&id) {
                link.ack(eid)?;
                w.push(Record::Ack {
                    n: stop.n,
                    id,
                    send: eid,
                });
            }
        }
        if need_sync {
            need_sync = false;
            let s = session.as_mut().expect("a resumed session");
            let o = s
                .sync(stop.at)
                .map_err(|e| RunError::Protocol(e.to_string()))?;
            let eid = link.send(stop.at, &o.bytes)?;
            send_of.insert(o.id, eid);
            w.push(Record::Send {
                n: stop.n,
                id: o.id,
                at: stop.at,
                bytes: String::from_utf8_lossy(&o.bytes).into_owned(),
                by: "runner".into(),
            });
        }
        let next_id = session.as_ref().map(|s| s.position().0).unwrap_or(1);
        let mut out = Out::new(next_id);
        ctl.step(&stop, &seen, &mut out);
        let mut ended = None;
        for a in out.into_actions() {
            match a {
                Action::Send { id, at, cmd, by } => {
                    let s = session.as_mut().ok_or_else(|| {
                        RunError::Protocol(format!(
                            "{by} sent {} before the agent's hello",
                            cmd.name()
                        ))
                    })?;
                    check_offered(s, &cmd)?;
                    if id != s.position().0 {
                        return Err(RunError::Protocol(format!(
                            "command id {id} where {} is next",
                            s.position().0
                        )));
                    }
                    s.note_sent(id, cmd.name());
                    let mut text = HostLine::new(id, cmd).encode();
                    text.push('\n');
                    let at = match at {
                        When::Now => stop.at,
                        When::At(t) => t,
                    };
                    let eid = link.send(at, text.as_bytes())?;
                    send_of.insert(id, eid);
                    w.push(Record::Send {
                        n: stop.n,
                        id,
                        at,
                        bytes: text,
                        by,
                    });
                }
                Action::Report { by, line } => w.push(Record::Report {
                    n: stop.n,
                    by,
                    line,
                }),
                Action::Note { by, line } => w.push(Record::Note {
                    n: stop.n,
                    by,
                    line,
                }),
                Action::Snapshot(name) => {
                    link.snap(&name)?;
                    if let Some(s) = &session {
                        let (next_id, next_seq) = s.position();
                        snapshots.push(AgentPoint {
                            snapshot: name.clone(),
                            hello: hello_text(s),
                            next_id,
                            next_seq,
                        });
                    }
                    w.push(Record::Snap { n: stop.n, name });
                }
                Action::Engine(a) => {
                    link.engine(&a)?;
                    w.push(Record::Engine {
                        n: stop.n,
                        action: a,
                    });
                }
                Action::End(e) => {
                    w.push(Record::End {
                        n: stop.n,
                        end: e.clone(),
                    });
                    ended = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = ended {
            let result = link.end(&e)?;
            return Ok(Finished {
                ending: Some(e),
                transcript: w.t,
                result,
                snapshots,
            });
        }
        link.cont()?;
    }
}

fn check_offered(s: &Session, cmd: &Command) -> Result<(), RunError> {
    if !s.offers(cmd.name()) {
        return Err(RunError::Protocol(format!(
            "the agent does not offer the op {}",
            cmd.name()
        )));
    }
    if let Some(f) = cmd.feature() {
        if !s.has_feature(f) {
            return Err(RunError::Protocol(format!(
                "the agent does not offer the feature {f}"
            )));
        }
    }
    Ok(())
}
