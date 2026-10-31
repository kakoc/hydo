//! Lock-step control of one run (hvx; milestone 118, docs/design/guest-control.md "The channel").
//!
//! The typed client side of the protocol `hv/hvx/hvx_explore.c` speaks for a `CTL`/`RESUME`
//! request in serve mode and for `--control one` (the protocol is written down in that file's
//! header comment, with an example transcript; real transcripts are in `tests/lockstep/`). This
//! module carries the protocol only: it decides nothing. A controller (wave 2: `hydo-control`)
//! reads each [`StopReport`] and answers with [`Lockstep::send`], [`Lockstep::ack`],
//! [`Lockstep::snap`], then [`Lockstep::cont`] or [`Lockstep::end`].
//!
//! ```text
//! engine -> host, before every stop:  EVENT <tag> at=<icount> n=<k> <escaped line>
//!                                     SENT <tag> id=<id> at=<icount>[ retype=1]
//!                                     DROP <tag> id=<id>
//!                                     STOP <tag> at=<icount> n=<k> phase=<p> why=<w>
//! host -> engine, at a stop:          SEND at=<icount> <escaped bytes>   -> OK send id=<id> sent=|queued=<n>
//!                                     ACK <id>                           -> OK ack <id> | ERR ack ...
//!                                     SNAP <name> [retype]               -> OK snap ... | ERR snap ...
//!                                     CONT | END [<verdict>]
//! the run ends with:                  RESULT <tag> fp=.. wfp=.. secs=.. stops=.. typed=.. inh=.. verdict=..
//! ```
//!
//! Times are the engine's instruction clock `icount` (hvx: +1 per retired deterministic exit,
//! +2048 per PMU tick). A send is typed at the first stop with `icount >= at`; every run call is
//! capped so that none runs past a pending send, which makes the point exact (branch-exact with
//! the PMU clock). The observation point of a console line is the stop at which the engine reports
//! it: with the schedule's `rb=<n>` the engine stops after at most n events.

use crate::protocol::RunResult;
use crate::EngineError;
use std::io::{BufRead, Write};

/// Which part of the run a stop belongs to. Each phase that starts a VM has its own clock:
/// the recovery boot after a power cut restarts from S0's icount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// An agent image's boot under control, from its first instruction (M121, `--control boot`):
    /// the host takes S0 and S1 here with SNAP.
    Boot,
    /// From S1 (or a resumed snapshot) to the end of the workload.
    Workload,
    /// The same VM after the engine typed `go` (shell-init images: heal and check).
    Check,
    /// The recovery VM after a power cut (restored from S0 with the flushed disk).
    Recovery,
}

impl Phase {
    fn parse(s: &str) -> Option<Phase> {
        Some(match s {
            "boot" => Phase::Boot,
            "workload" => Phase::Workload,
            "check" => Phase::Check,
            "recovery" => Phase::Recovery,
            _ => return None,
        })
    }
}

/// Why the engine stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopWhy {
    /// Before the first run call of a phase (the engine's own input is already typed).
    Start,
    /// Right after a RESUME restore.
    Resume,
    /// A run call returned after its event budget (`rb=`) or on its own.
    Cadence,
    /// A pending send became due at a capped return.
    Send,
    /// The shell init's workload or check marker appeared (the engine types its next input after
    /// this stop, as a plain RUN does).
    Marker,
    /// The guest stopped (the run ends with an error RESULT after this stop).
    Exit,
}

impl StopWhy {
    fn parse(s: &str) -> Option<StopWhy> {
        Some(match s {
            "start" => StopWhy::Start,
            "resume" => StopWhy::Resume,
            "cadence" => StopWhy::Cadence,
            "send" => StopWhy::Send,
            "marker" => StopWhy::Marker,
            "exit" => StopWhy::Exit,
            _ => return None,
        })
    }
}

/// One stop: the engine waits for the host's commands here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stop {
    pub at: u64,
    /// The stop's number in the run (1 = the start stop; a resumed run continues the count).
    pub n: u64,
    pub phase: Phase,
    pub why: StopWhy,
}

/// One complete console line the guest wrote since the previous stop (without its `\n` and a
/// trailing `\r`), observed at the stop's icount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub at: u64,
    pub n: u64,
    pub text: Vec<u8>,
}

impl Event {
    pub fn text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.text).into_owned()
    }
}

/// A queued send the engine typed at this stop (`retype`: typed again after a RESUME because it
/// was not acknowledged when the snapshot was taken).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sent {
    pub id: u64,
    pub at: u64,
    pub retype: bool,
}

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

/// The answer to a SEND.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendAnswer {
    /// Typed at this stop (its `at` was not after the stop's icount).
    Typed { id: u64, at: u64 },
    /// Queued; a later [`StopReport::sent`] says when it was typed.
    Queued { id: u64, at: u64 },
}

impl SendAnswer {
    pub fn id(&self) -> u64 {
        match *self {
            SendAnswer::Typed { id, .. } | SendAnswer::Queued { id, .. } => id,
        }
    }
}

/// The answer to a SNAP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapAnswer {
    /// Taken at `at`; `retype` lists the unacknowledged sends a RESUME types again.
    Taken { at: u64, retype: Vec<u64> },
    /// Refused, with the engine's reason (for example `unacked=1,2`).
    Refused(String),
}

/// One host line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostCmd {
    Send { at: u64, bytes: Vec<u8> },
    Ack(u64),
    Snap { name: String, retype: bool },
    Cont,
    End(Option<String>),
}

impl HostCmd {
    /// The line as the engine reads it (without the newline).
    pub fn line(&self) -> String {
        match self {
            HostCmd::Send { at, bytes } => format!("SEND at={at} {}", escape(bytes)),
            HostCmd::Ack(id) => format!("ACK {id}"),
            HostCmd::Snap { name, retype } => {
                format!("SNAP {name}{}", if *retype { " retype" } else { "" })
            }
            HostCmd::Cont => "CONT".into(),
            HostCmd::End(None) => "END".into(),
            HostCmd::End(Some(v)) => format!("END {v}"),
        }
    }

    /// Parse a host line (the inverse of [`HostCmd::line`]; used to replay transcripts).
    pub fn parse(line: &str) -> Option<HostCmd> {
        if line == "CONT" {
            return Some(HostCmd::Cont);
        }
        if line == "END" {
            return Some(HostCmd::End(None));
        }
        if let Some(v) = line.strip_prefix("END ") {
            return Some(HostCmd::End(Some(v.to_string())));
        }
        if let Some(r) = line.strip_prefix("SEND at=") {
            let (at, text) = r.split_once(' ')?;
            return Some(HostCmd::Send {
                at: at.parse().ok()?,
                bytes: unescape(text)?,
            });
        }
        if let Some(id) = line.strip_prefix("ACK ") {
            return Some(HostCmd::Ack(id.parse().ok()?));
        }
        if let Some(r) = line.strip_prefix("SNAP ") {
            let mut w = r.split(' ');
            let name = w.next()?.to_string();
            let retype = match w.next() {
                None => false,
                Some("retype") => true,
                Some(_) => return None,
            };
            return Some(HostCmd::Snap { name, retype });
        }
        None
    }
}

/// One engine line of a controlled run.
#[derive(Clone, Debug, PartialEq)]
pub enum EngineLine {
    Event(Event),
    Sent(Sent),
    Drop(u64),
    Stop(Stop),
    /// `OK <rest>` (an answer).
    Ok(String),
    /// `ERR <rest>` (an answer).
    Err(String),
    Result(Box<RunResult>),
}

impl EngineLine {
    /// Parse one engine line (without its newline; a trailing `\r` is not part of the protocol).
    pub fn parse(line: &str) -> Option<EngineLine> {
        if let Some(r) = line.strip_prefix("OK ") {
            return Some(EngineLine::Ok(r.to_string()));
        }
        if let Some(r) = line.strip_prefix("ERR ") {
            return Some(EngineLine::Err(r.to_string()));
        }
        if line.starts_with("RESULT ") {
            return RunResult::parse(line).map(|r| EngineLine::Result(Box::new(r)));
        }
        if let Some(r) = line.strip_prefix("EVENT ") {
            // EVENT <tag> at=<n> n=<k> <text>: the text may be empty and may contain spaces
            let (_tag, r) = r.split_once(' ')?;
            let (at, r) = r.split_once(' ')?;
            let (n, text) = r.split_once(' ')?;
            return Some(EngineLine::Event(Event {
                at: at.strip_prefix("at=")?.parse().ok()?,
                n: n.strip_prefix("n=")?.parse().ok()?,
                text: unescape(text)?,
            }));
        }
        let mut w = line.split(' ');
        let kind = w.next()?;
        let _tag = w.next()?;
        let kv: Vec<(&str, &str)> = w.map(|x| x.split_once('=')).collect::<Option<_>>()?;
        let get = |k: &str| kv.iter().find(|(a, _)| *a == k).map(|(_, v)| *v);
        let num = |k: &str| get(k).and_then(|v| v.parse::<u64>().ok());
        match kind {
            "SENT" => Some(EngineLine::Sent(Sent {
                id: num("id")?,
                at: num("at")?,
                retype: get("retype") == Some("1"),
            })),
            "DROP" => Some(EngineLine::Drop(num("id")?)),
            "STOP" => Some(EngineLine::Stop(Stop {
                at: num("at")?,
                n: num("n")?,
                phase: Phase::parse(get("phase")?)?,
                why: StopWhy::parse(get("why")?)?,
            })),
            _ => None,
        }
    }
}

/// The engine's escaping of console bytes and send payloads: printable ASCII as is, `\\`, `\n`,
/// `\r`, `\t`, every other byte as `\xHH`.
pub fn escape(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len());
    for &c in b {
        match c {
            b'\\' => s.push_str("\\\\"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            0x20..=0x7e => s.push(c as char),
            _ => s.push_str(&format!("\\x{c:02x}")),
        }
    }
    s
}

/// The inverse of [`escape`]; None on a malformed escape.
pub fn unescape(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        match b.get(i + 1)? {
            b'\\' => out.push(b'\\'),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'x' => {
                let h = std::str::from_utf8(b.get(i + 2..i + 4)?).ok()?;
                if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 2;
            }
            _ => return None,
        }
        i += 2;
    }
    Some(out)
}

/// What a controlled run starts from.
#[derive(Clone, Debug, PartialEq)]
pub enum LockstepStart {
    /// `CTL <tag> <schedule>`: from S1, like a RUN (an empty guest part types nothing). On an
    /// agent image's worker (M121) S1 is the snapshot the host took under that name in the boot,
    /// the guest part must be just `run`, and nothing is typed.
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

    pub fn line(&self) -> String {
        match self {
            LockstepStart::Run { tag, schedule } => format!("CTL {tag} {schedule}"),
            LockstepStart::Resume { tag, snapshot } => format!("RESUME {tag} {snapshot}"),
        }
    }
}

/// One controlled run in progress over an engine's stdin/stdout.
pub struct Lockstep<'a> {
    r: &'a mut dyn BufRead,
    w: &'a mut dyn Write,
    log: Option<&'a mut dyn Write>,
    done: bool,
}

impl<'a> Lockstep<'a> {
    /// A session over an engine that has been sent the request already (or, for `--control one`,
    /// was started with the schedule).
    pub fn new(r: &'a mut dyn BufRead, w: &'a mut dyn Write) -> Lockstep<'a> {
        Lockstep {
            r,
            w,
            log: None,
            done: false,
        }
    }

    /// Also write every line of both directions to `log` (`< ` host, `> ` engine): a transcript.
    pub fn with_log(mut self, log: &'a mut dyn Write) -> Lockstep<'a> {
        self.log = Some(log);
        self
    }

    /// Write a raw request line (CTL/RESUME) and log it.
    pub(crate) fn request(&mut self, line: &str) -> Result<(), EngineError> {
        self.write(line)
    }

    fn write(&mut self, line: &str) -> Result<(), EngineError> {
        if let Some(l) = self.log.as_mut() {
            writeln!(l, "< {line}")?;
        }
        self.w.write_all(line.as_bytes())?;
        self.w.write_all(b"\n")?;
        self.w.flush()?;
        Ok(())
    }

    fn read(&mut self) -> Result<EngineLine, EngineError> {
        let mut buf = Vec::new();
        if self.r.read_until(b'\n', &mut buf)? == 0 {
            return Err(EngineError::Protocol("the engine exited".into()));
        }
        while buf.last() == Some(&b'\n') {
            buf.pop();
        }
        let line = String::from_utf8_lossy(&buf).into_owned();
        if let Some(l) = self.log.as_mut() {
            writeln!(l, "> {line}")?;
        }
        match EngineLine::parse(&line) {
            Some(EngineLine::Result(r)) => {
                self.done = true;
                Ok(EngineLine::Result(r))
            }
            Some(l) => Ok(l),
            // the engine prints nothing else on stdout during a controlled run
            None => Err(EngineError::Protocol(line)),
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
                EngineLine::Event(e) => events.push(e),
                EngineLine::Sent(s) => sent.push(s),
                EngineLine::Drop(id) => dropped.push(id),
                EngineLine::Stop(stop) => {
                    return Ok(Next::Stop(StopReport {
                        events,
                        sent,
                        dropped,
                        stop,
                    }))
                }
                EngineLine::Result(r) => return Ok(Next::Done(*r)),
                l => return Err(EngineError::Protocol(format!("{l:?} between stops"))),
            }
        }
    }

    fn answer(&mut self) -> Result<Result<String, String>, EngineError> {
        match self.read()? {
            EngineLine::Ok(s) => Ok(Ok(s)),
            EngineLine::Err(s) => Ok(Err(s)),
            l => Err(EngineError::Protocol(format!("{l:?} instead of an answer"))),
        }
    }

    /// Type `bytes` (1..=4096) into the serial input at the first stop with icount >= `at`.
    pub fn send(&mut self, at: u64, bytes: &[u8]) -> Result<SendAnswer, EngineError> {
        self.write(
            &HostCmd::Send {
                at,
                bytes: bytes.to_vec(),
            }
            .line(),
        )?;
        let a = self.answer()?.map_err(EngineError::Protocol)?;
        let mut w = a.split(' ');
        let bad = || EngineError::Protocol(format!("OK {a}"));
        if w.next() != Some("send") {
            return Err(bad());
        }
        let id = w
            .next()
            .and_then(|x| x.strip_prefix("id="))
            .and_then(|x| x.parse().ok())
            .ok_or_else(bad)?;
        let t = w.next().ok_or_else(bad)?;
        if let Some(v) = t.strip_prefix("sent=") {
            Ok(SendAnswer::Typed {
                id,
                at: v.parse().map_err(|_| bad())?,
            })
        } else if let Some(v) = t.strip_prefix("queued=") {
            Ok(SendAnswer::Queued {
                id,
                at: v.parse().map_err(|_| bad())?,
            })
        } else {
            Err(bad())
        }
    }

    /// Mark send `id` as consumed by the guest. Err(reason) when the engine refuses.
    pub fn ack(&mut self, id: u64) -> Result<Result<(), String>, EngineError> {
        self.write(&HostCmd::Ack(id).line())?;
        Ok(self.answer()?.map(|_| ()))
    }

    /// Snapshot at this stop under `name` (see the module and engine documentation for the rule
    /// on unacknowledged input).
    pub fn snap(&mut self, name: &str, retype: bool) -> Result<SnapAnswer, EngineError> {
        if name.is_empty() || name.len() > 63 || name.contains(char::is_whitespace) {
            return Err(EngineError::Protocol(format!("bad snapshot name {name:?}")));
        }
        self.write(
            &HostCmd::Snap {
                name: name.into(),
                retype,
            }
            .line(),
        )?;
        match self.answer()? {
            Err(e) => Ok(SnapAnswer::Refused(e)),
            Ok(a) => {
                let bad = || EngineError::Protocol(format!("OK {a}"));
                let mut at = None;
                let mut retype = None;
                for w in a.split(' ') {
                    if let Some(v) = w.strip_prefix("at=") {
                        at = v.parse().ok();
                    } else if let Some(v) = w.strip_prefix("retype=") {
                        retype = Some(if v == "-" {
                            Vec::new()
                        } else {
                            v.split(',')
                                .map(|x| x.parse().map_err(|_| bad()))
                                .collect::<Result<_, _>>()?
                        });
                    }
                }
                Ok(SnapAnswer::Taken {
                    at: at.ok_or_else(bad)?,
                    retype: retype.ok_or_else(bad)?,
                })
            }
        }
    }

    /// Run on to the next stop.
    pub fn cont(&mut self) -> Result<(), EngineError> {
        self.write(&HostCmd::Cont.line())
    }

    /// End the run at this stop; `verdict` (one line) becomes the RESULT's verdict, otherwise the
    /// engine's own (the last HYDO-ORACLE line).
    pub fn end(&mut self, verdict: Option<&str>) -> Result<RunResult, EngineError> {
        if verdict.is_some_and(|v| v.contains('\n')) {
            return Err(EngineError::Protocol("a verdict is one line".into()));
        }
        self.write(&HostCmd::End(verdict.map(str::to_string)).line())?;
        match self.read()? {
            EngineLine::Result(r) => Ok(*r),
            l => Err(EngineError::Protocol(format!("{l:?} instead of RESULT"))),
        }
    }

    /// True once the RESULT line was read.
    pub fn finished(&self) -> bool {
        self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn escaping_round_trips() {
        let all: Vec<u8> = (0..=255u8).collect();
        let e = escape(&all);
        assert!(e.bytes().all(|c| (0x20..=0x7e).contains(&c)));
        assert_eq!(unescape(&e).unwrap(), all);
        assert_eq!(escape(b"a b\\c\n"), "a b\\\\c\\n");
        assert_eq!(unescape("x\\x41\\t"), Some(b"xA\t".to_vec()));
        for bad in ["\\", "\\q", "\\x4", "\\xg0"] {
            assert_eq!(unescape(bad), None, "{bad}");
        }
    }

    #[test]
    fn engine_lines() {
        assert_eq!(
            EngineLine::parse("EVENT a at=7 n=2 KILLED redis"),
            Some(EngineLine::Event(Event {
                at: 7,
                n: 2,
                text: b"KILLED redis".to_vec()
            }))
        );
        assert_eq!(
            EngineLine::parse("EVENT a at=7 n=3 "),
            Some(EngineLine::Event(Event {
                at: 7,
                n: 3,
                text: Vec::new()
            }))
        );
        assert_eq!(
            EngineLine::parse("STOP a at=9 n=4 phase=check why=marker"),
            Some(EngineLine::Stop(Stop {
                at: 9,
                n: 4,
                phase: Phase::Check,
                why: StopWhy::Marker
            }))
        );
        assert_eq!(
            EngineLine::parse("SENT b id=1 at=969127 retype=1"),
            Some(EngineLine::Sent(Sent {
                id: 1,
                at: 969127,
                retype: true
            }))
        );
        assert_eq!(EngineLine::parse("DROP a id=5"), Some(EngineLine::Drop(5)));
        // an agent image's controlled boot (M121)
        assert_eq!(
            EngineLine::parse("STOP boot at=0 n=1 phase=boot why=start"),
            Some(EngineLine::Stop(Stop {
                at: 0,
                n: 1,
                phase: Phase::Boot,
                why: StopWhy::Start
            }))
        );
        assert!(EngineLine::parse("STOP a at=9 n=4 phase=later why=marker").is_none());
        assert!(EngineLine::parse("hvx_explore: S1").is_none());
    }

    #[test]
    fn host_lines_round_trip() {
        for c in [
            HostCmd::Send {
                at: 5,
                bytes: b"ping 1\n".to_vec(),
            },
            HostCmd::Ack(3),
            HostCmd::Snap {
                name: "s".into(),
                retype: true,
            },
            HostCmd::Snap {
                name: "t".into(),
                retype: false,
            },
            HostCmd::Cont,
            HostCmd::End(None),
            HostCmd::End(Some("HYDO-ORACLE: OK".into())),
        ] {
            assert_eq!(HostCmd::parse(&c.line()), Some(c.clone()), "{}", c.line());
        }
    }

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
        let mut r = Cursor::new(engine.as_bytes().to_vec());
        let mut w = Vec::new();
        let mut log = Vec::new();
        {
            let mut s = Lockstep::new(&mut r, &mut w).with_log(&mut log);
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
            String::from_utf8(w).unwrap(),
            "SEND at=300 x\nCONT\nCONT\nSNAP s\nACK 1\nSNAP s\nEND\n"
        );
        assert!(String::from_utf8(log)
            .unwrap()
            .contains("> SENT a id=1 at=301\n"));
    }
}
