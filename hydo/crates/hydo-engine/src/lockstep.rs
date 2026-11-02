//! Lock-step control of one run (milestone 118, docs/design/guest-control.md "The channel").
//!
//! The typed client of a controlled run: a `CTL`/`RESUME` request of a serving engine, a
//! `--control one` run, or an agent image's controlled boot. It carries the protocol only and
//! decides nothing: a controller (`hydo-control`) reads each [`StopReport`] and answers with
//! [`Lockstep::send`], [`Lockstep::ack`], [`Lockstep::snap`], [`Lockstep::diskoff`],
//! [`Lockstep::cut`], then [`Lockstep::cont`] or [`Lockstep::end`].
//!
//! The session runs over a [`Link`]: in this process the values go to the hvx engine thread as
//! they are; to an engine program they are the protocol's lines (`hydo-engine-proto`, which also
//! writes the protocol down):
//!
//! ```text
//! engine -> host, before every stop:  EVENT <tag> at=<icount> n=<k> <escaped line>
//!                                     SENT <tag> id=<id> at=<icount>[ retype=1]
//!                                     DROP <tag> id=<id>
//!                                     STOP <tag> at=<icount> n=<k> phase=<p> why=<w>
//! host -> engine, at a stop:          SEND at=<icount> <escaped bytes>   -> OK send id=<id> sent=|queued=<n>
//!                                     ACK <id>                           -> OK ack <id> | ERR ack ...
//!                                     SNAP <name> [retype]               -> OK snap ... | ERR snap ...
//!                                     DISKOFF (agent images, M126)       -> OK diskoff at=<n> cut=<0|1>
//!                                     CUT     (agent images, M126)       -> OK cut at=<n>, ends the stop
//!                                     CONT | END [<verdict>]
//! the run ends with:                  RESULT <tag> fp=.. wfp=.. secs=.. stops=.. typed=.. inh=.. verdict=..
//! ```
//!
//! Times are the engine's instruction clock `icount` (hvx: +1 per retired deterministic exit,
//! +2048 per PMU tick). A send is typed at the first stop with `icount >= at`; every run call is
//! capped so that none runs past a pending send, which makes the point exact (branch-exact with
//! the PMU clock). The observation point of a console line is the stop at which the engine reports
//! it: with the schedule's `rb=<n>` the engine stops after at most n events.
//!
//! A transcript ([`Lockstep::with_log`]) has one line per value, `< ` for the host's and `> ` for
//! the engine's, in the protocol's text: the line an engine program printed, or the value's line
//! ([`EngineMsg::line`]) in this process. Both are the same text.

pub use crate::link::{ChanLink, Link, TextLink};
use crate::protocol::{EngineMsg, HostMsg, Request, RunResult};
use crate::EngineError;
pub use hydo_engine_proto::{
    escape, unescape, Answer, DiskOff, Event, HostCmd, Phase, SendAnswer, Sent, Stop, StopWhy,
};
use std::io::Write;

/// Everything the engine reported up to and including one stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopReport {
    pub events: Vec<Event>,
    pub sent: Vec<Sent>,
    /// Sends still queued when the run moved to a new VM (the recovery boot): never typed.
    pub dropped: Vec<u64>,
    pub stop: Stop,
}

/// What [`Lockstep::next_stop`] read: a stop, or the end of the run.
#[derive(Clone, Debug, PartialEq)]
pub enum Next {
    Stop(StopReport),
    Done(RunResult),
}

/// The answer to a SNAP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapAnswer {
    /// Taken at `at`; `retype` lists the unacknowledged sends a RESUME types again.
    Taken { at: u64, retype: Vec<u64> },
    /// Refused, with the engine's reason (for example `snap s unacked=1,2`).
    Refused(String),
}

/// Carries out one controlled run over its lock-step session, to the run's RESULT: the shape of
/// the design's `Worker::run_controlled(tag, ctl)` (docs/design/guest-control.md "Explorer API").
/// The controller itself (`hydo_control::Controller`) lives in a crate that depends on this one,
/// so the engines take this object-safe trait instead; `hydo_control` implements it for its
/// conversations ([`crate::Worker::run_controlled`], [`crate::Worker::resume`]).
pub trait Driver {
    fn drive(&mut self, ls: &mut Lockstep<'_>) -> Result<RunResult, EngineError>;
}

/// What a controlled run starts from.
#[derive(Clone, Debug, PartialEq)]
pub enum LockstepStart {
    /// `CTL <tag> <schedule>`: from S1, like a RUN. On an agent image's worker (M121) S1 is the
    /// snapshot the host took under that name in the boot, the guest part must be just `run`,
    /// and nothing is typed.
    Run {
        tag: String,
        schedule: hydo_schedule::Schedule,
    },
    /// `RESUME <tag> <snapshot>`: from a snapshot an earlier controlled run of this worker took.
    Resume { tag: String, snapshot: String },
}

impl LockstepStart {
    pub fn tag(&self) -> &str {
        match self {
            LockstepStart::Run { tag, .. } | LockstepStart::Resume { tag, .. } => tag,
        }
    }

    /// The engine request.
    pub fn request(&self) -> Request {
        match self {
            LockstepStart::Run { tag, schedule } => Request::Ctl {
                tag: tag.clone(),
                schedule: schedule.clone(),
            },
            LockstepStart::Resume { tag, snapshot } => Request::Resume {
                tag: tag.clone(),
                snapshot: snapshot.clone(),
            },
        }
    }

    pub fn line(&self) -> String {
        self.request().line()
    }
}

/// One controlled run in progress over an engine link.
pub struct Lockstep<'a> {
    link: &'a mut dyn Link,
    log: Option<&'a mut dyn Write>,
    done: bool,
}

impl<'a> Lockstep<'a> {
    /// A session over an engine that has been sent the request already (or, for `--control one`
    /// and a controlled boot, was started with the schedule).
    pub fn new(link: &'a mut dyn Link) -> Lockstep<'a> {
        Lockstep {
            link,
            log: None,
            done: false,
        }
    }

    /// Also write every value of both directions to `log` (`< ` host, `> ` engine): a transcript.
    pub fn with_log(mut self, log: &'a mut dyn Write) -> Lockstep<'a> {
        self.log = Some(log);
        self
    }

    /// Send the request that starts the run (CTL/RESUME) and log it.
    pub(crate) fn request(&mut self, r: Request) -> Result<(), EngineError> {
        self.write(HostMsg::Request(r))
    }

    fn write(&mut self, m: HostMsg) -> Result<(), EngineError> {
        if let Some(l) = self.log.as_mut() {
            writeln!(l, "< {}", m.line())?;
        }
        self.link.send(m)
    }

    fn cmd(&mut self, c: HostCmd) -> Result<(), EngineError> {
        self.write(HostMsg::Cmd(c))
    }

    fn read(&mut self) -> Result<EngineMsg, EngineError> {
        let m = self.link.recv()?;
        let text = |link: &dyn Link, m: &EngineMsg| match link.last_text() {
            Some(t) => t.trim_end_matches('\n').to_string(),
            None => m.line(),
        };
        if let Some(l) = self.log.as_mut() {
            writeln!(l, "> {}", text(&*self.link, &m))?;
        }
        match m {
            EngineMsg::Result(_) => {
                self.done = true;
                Ok(m)
            }
            // the engine reports nothing else during a controlled run
            EngineMsg::Other(_) | EngineMsg::Ready(_) => {
                Err(EngineError::Protocol(text(&*self.link, &m)))
            }
            m => Ok(m),
        }
    }

    /// Read up to the next stop (or the end of the run).
    pub fn next_stop(&mut self) -> Result<Next, EngineError> {
        if self.done {
            return Err(EngineError::Protocol("the run has ended".into()));
        }
        let (mut events, mut sent, mut dropped) = (Vec::new(), Vec::new(), Vec::new());
        loop {
            match self.read()? {
                EngineMsg::Event { event, .. } => events.push(event),
                EngineMsg::Sent { sent: s, .. } => sent.push(s),
                EngineMsg::Drop { id, .. } => dropped.push(id),
                EngineMsg::Stop { stop, .. } => {
                    return Ok(Next::Stop(StopReport {
                        events,
                        sent,
                        dropped,
                        stop,
                    }))
                }
                EngineMsg::Result(r) => return Ok(Next::Done(*r)),
                l => return Err(EngineError::Protocol(format!("{l:?} between stops"))),
            }
        }
    }

    fn answer(&mut self) -> Result<Answer, EngineError> {
        match self.read()? {
            EngineMsg::Answer(a) => Ok(a),
            l => Err(EngineError::Protocol(format!("{l:?} instead of an answer"))),
        }
    }

    /// Type `bytes` (1..=4096) into the serial input at the first stop with icount >= `at`.
    pub fn send(&mut self, at: u64, bytes: &[u8]) -> Result<SendAnswer, EngineError> {
        self.cmd(HostCmd::Send {
            at,
            bytes: bytes.to_vec(),
        })?;
        match self.answer()? {
            Answer::Send(a) => Ok(a),
            Answer::Refused(r) => Err(EngineError::Protocol(r)),
            a => Err(EngineError::Protocol(a.line())),
        }
    }

    /// Mark send `id` as consumed by the guest. Err(reason) when the engine refuses.
    pub fn ack(&mut self, id: u64) -> Result<Result<(), String>, EngineError> {
        self.cmd(HostCmd::Ack(id))?;
        Ok(match self.answer()? {
            Answer::Refused(r) => Err(r),
            _ => Ok(()),
        })
    }

    /// Snapshot at this stop under `name` (see the module and engine documentation for the rule
    /// on unacknowledged input).
    pub fn snap(&mut self, name: &str, retype: bool) -> Result<SnapAnswer, EngineError> {
        if name.is_empty() || name.len() > 63 || name.contains(char::is_whitespace) {
            return Err(EngineError::Protocol(format!("bad snapshot name {name:?}")));
        }
        self.cmd(HostCmd::Snap {
            name: name.into(),
            retype,
        })?;
        match self.answer()? {
            Answer::Refused(e) => Ok(SnapAnswer::Refused(e)),
            Answer::Snap { at, retype, .. } => Ok(SnapAnswer::Taken { at, retype }),
            a => Err(EngineError::Protocol(a.line())),
        }
    }

    /// The workload has ended (an agent image's counterpart of the shell init's workload marker,
    /// M126). Err(reason) when the engine refuses.
    pub fn diskoff(&mut self) -> Result<Result<DiskOff, String>, EngineError> {
        self.cmd(HostCmd::DiskOff)?;
        match self.answer()? {
            Answer::Refused(e) => Ok(Err(e)),
            Answer::DiskOff(d) => Ok(Ok(d)),
            a => Err(EngineError::Protocol(a.line())),
        }
    }

    /// A power cut at this stop (agent images, M126). On Ok the stop is over: the next stop is
    /// the recovery VM's start stop. Err(reason) when the engine refuses (the stop goes on).
    pub fn cut(&mut self) -> Result<Result<u64, String>, EngineError> {
        self.cmd(HostCmd::Cut)?;
        match self.answer()? {
            Answer::Refused(e) => Ok(Err(e)),
            Answer::Cut { at } => Ok(Ok(at)),
            a => Err(EngineError::Protocol(a.line())),
        }
    }

    /// Run on to the next stop.
    pub fn cont(&mut self) -> Result<(), EngineError> {
        self.cmd(HostCmd::Cont)
    }

    /// End the run at this stop; `verdict` (one line) becomes the RESULT's verdict, otherwise the
    /// engine's own (the last HYDO-ORACLE line).
    pub fn end(&mut self, verdict: Option<&str>) -> Result<RunResult, EngineError> {
        if verdict.is_some_and(|v| v.contains('\n')) {
            return Err(EngineError::Protocol("a verdict is one line".into()));
        }
        self.cmd(HostCmd::End(verdict.map(str::to_string)))?;
        match self.read()? {
            EngineMsg::Result(r) => Ok(*r),
            l => Err(EngineError::Protocol(format!("{l:?} instead of RESULT"))),
        }
    }

    /// True once the RESULT was read.
    pub fn finished(&self) -> bool {
        self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_session() {
        let engine = "STOP a at=100 n=1 phase=workload why=start\n\
                      OK send id=1 queued=300\n\
                      EVENT a at=200 n=1 hello\n\
                      STOP a at=200 n=2 phase=workload why=cadence\n\
                      SENT a id=1 at=301\n\
                      STOP a at=301 n=3 phase=workload why=send\n\
                      ERR snap s unacked=1\n\
                      OK ack 1\n\
                      OK snap s at=301 retype=-\n\
                      RESULT a fp=0000000000000001 wfp=0000000000000002 secs=1.0 stops=3 typed=1 inh=00000000000000aa verdict=X\n";
        let mut link = TextLink::new(Cursor::new(engine.as_bytes().to_vec()), Vec::new());
        let mut log = Vec::new();
        {
            let mut s = Lockstep::new(&mut link).with_log(&mut log);
            let Next::Stop(st) = s.next_stop().unwrap() else {
                panic!()
            };
            assert_eq!(st.stop.why, StopWhy::Start);
            assert_eq!(
                s.send(300, b"x").unwrap(),
                SendAnswer::Queued { id: 1, at: 300 }
            );
            s.cont().unwrap();
            let Next::Stop(st) = s.next_stop().unwrap() else {
                panic!()
            };
            assert_eq!(st.events[0].text_lossy(), "hello");
            s.cont().unwrap();
            let Next::Stop(st) = s.next_stop().unwrap() else {
                panic!()
            };
            assert_eq!(
                st.sent,
                vec![Sent {
                    id: 1,
                    at: 301,
                    retype: false
                }]
            );
            assert_eq!(
                s.snap("s", false).unwrap(),
                SnapAnswer::Refused("snap s unacked=1".into())
            );
            assert_eq!(s.ack(1).unwrap(), Ok(()));
            assert_eq!(
                s.snap("s", false).unwrap(),
                SnapAnswer::Taken {
                    at: 301,
                    retype: vec![]
                }
            );
            let r = s.end(None).unwrap();
            assert_eq!(r.extra("stops"), Some("3"));
            assert!(s.finished());
        }
        assert_eq!(
            String::from_utf8(link.writer().clone()).unwrap(),
            "SEND at=300 x\nCONT\nCONT\nSNAP s\nACK 1\nSNAP s\nEND\n"
        );
        let log = String::from_utf8(log).unwrap();
        assert!(log.contains("> SENT a id=1 at=301\n"));
        assert!(log.contains("< SNAP s\n"));
    }

    #[test]
    fn a_session_in_process() {
        // the same values over channels: the transcript is the same text
        use std::sync::mpsc::channel;
        let (htx, hrx) = channel::<HostMsg>();
        let (etx, erx) = channel::<EngineMsg>();
        let mut link = ChanLink::new(htx, erx);
        for l in [
            "STOP a at=100 n=1 phase=workload why=start",
            "OK send id=1 sent=100",
            "RESULT a fp=0000000000000001 wfp=0000000000000002 secs=1.0 verdict=X",
        ] {
            etx.send(EngineMsg::parse(l)).unwrap();
        }
        let mut log = Vec::new();
        {
            let mut s = Lockstep::new(&mut link).with_log(&mut log);
            assert!(matches!(s.next_stop().unwrap(), Next::Stop(_)));
            assert_eq!(
                s.send(5, b"hi\n").unwrap(),
                SendAnswer::Typed { id: 1, at: 100 }
            );
            assert_eq!(s.end(Some("V")).unwrap().verdict, "X");
        }
        assert_eq!(
            hrx.try_iter().map(|m| m.line()).collect::<Vec<_>>(),
            ["SEND at=5 hi\\n", "END V"]
        );
        assert_eq!(
            String::from_utf8(log).unwrap(),
            "> STOP a at=100 n=1 phase=workload why=start\n< SEND at=5 hi\\n\n> OK send id=1 sent=100\n\
             < END V\n> RESULT a fp=0000000000000001 wfp=0000000000000002 secs=1.0 verdict=X\n"
        );
    }
}
