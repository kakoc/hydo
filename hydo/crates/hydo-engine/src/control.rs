//! Lock-step control of a run (M119; docs/design/guest-control.md, "Explorer API").
//!
//! In a control session the engine types nothing by itself. The host decides every byte that goes
//! into the guest's serial input and the exact virtual time at which it goes in, and the engine
//! stops at fixed points to show the host what the guest wrote. The engine waits at every stop
//! until the host says how to go on. This module is the typed client side; it holds no controller
//! logic (what to send when is the caller's business).
//!
//! Virtual time ([`VTime`]) is the engine's own clock: hvm's exit count (`M.exits`), hvx's
//! instruction clock (`icount`). A stop's time is a pure function of the run, so a decision taken
//! at a stop is deterministic.
//!
//! The text protocol (one line each way; `hv/hvm/explore/serve.c`, "lock-step control"):
//!
//! ```text
//! host:   CTRL <tag> [every=<n>] [lines=<prefix>|lines=*] [<host tokens>]   a session from S1
//!         CTRL <tag> from=<snapshot> [plan=drop]                           a session from a SNAP
//! engine: TYPED <tag> at=<t> req=<t> len=<n>   SEND bytes went into the receive queue at t
//!         EVENT <tag> at=<t> <escaped line>    a complete console line, seen at cadence point t
//!         MATCH <tag> at=<t> <escaped text>    a WATCH text appeared, seen at cadence point t
//!         STOP <tag> at=<t>[ end=<why>]        the engine waits for the host
//! host at a stop (one OK/ERR answer each):
//!         SEND at=<t> <escaped bytes>   WAKE at=<t>   WATCH <escaped text>   SNAP <name>   FP
//!         CONT (run to the next stop)   END (RESULT <tag> fp= wfp= secs= ctl= stops= verdict=)
//! ```
//!
//! Escapes: `\\`, `\n`, `\r`, `\t` and `\xHH`; the engine prints every byte outside 0x20..0x7e
//! as `\xHH` ([`escape`], [`unescape`]).
//!
//! Two engine rules a controller relies on:
//!
//! - **Snapshots never hold half-delivered input.** `SNAP` is refused while typed bytes still wait
//!   in the receive queue ([`SnapAnswer::InputPending`]); a later stop can try again. Whether the
//!   guest's program has *acted* on the bytes is the controller's business (the agent's
//!   acknowledgement, in the design).
//! - **A snapshot carries the plan.** The SENDs, WAKEs and WATCHes still pending at a `SNAP` are
//!   part of the snapshot, and a session from it re-arms them (so input planned for after the
//!   snapshot point is typed again after the restore) unless it starts with `plan=drop`. Bytes a
//!   SEND at the snapshot's own time would type are typed at `CONT`, after the snapshot, so they
//!   are pending at the `SNAP` and re-typed too.

use crate::protocol::RunResult;
use crate::EngineError;

/// The engine's virtual time: hvm's exit count, hvx's icount.
pub type VTime = u64;

/// Which console lines the engine reports as [`Event`]s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineFilter {
    /// Lines that start with this text (no spaces). The engine's default is `HYDO-AG`, the
    /// agent's frame prefix in the design.
    Prefix(String),
    /// Every line.
    All,
}

/// Where a session starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlStart {
    /// From S1 (or, for the first request of a fresh worker, the boot's own state at S1).
    S1 {
        /// The observation cadence: the engine looks at the console every `every` units of virtual
        /// time, counted from the session's start (hvm's default and finest: 1, every exit).
        every: u64,
        /// None: the engine's default (`HYDO-AG`).
        lines: Option<LineFilter>,
        /// Host tokens as in a schedule's host part (`disk=`, `dseed=`, `tj*`, `pct*`); they apply
        /// for the whole session.
        host: Vec<String>,
    },
    /// From a snapshot taken with [`LockStep::snap`] in an earlier session of the same worker.
    /// The cadence, the filter and the host tokens are the snapshot's.
    Snapshot {
        name: String,
        /// false: drop the plan the snapshot carries (`plan=drop`).
        keep_plan: bool,
    },
}

/// One `CTRL` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlRequest {
    /// One word of at most 63 bytes; the console goes to `<outdir>/<tag>.log`.
    pub tag: String,
    pub start: ControlStart,
}

impl ControlRequest {
    /// A session from S1 with the default filter.
    pub fn from_s1(tag: &str, every: u64, host: &[&str]) -> ControlRequest {
        ControlRequest {
            tag: tag.into(),
            start: ControlStart::S1 {
                every,
                lines: None,
                host: host.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    /// A session from a snapshot, keeping its plan.
    pub fn from_snapshot(tag: &str, name: &str) -> ControlRequest {
        ControlRequest {
            tag: tag.into(),
            start: ControlStart::Snapshot {
                name: name.into(),
                keep_plan: true,
            },
        }
    }

    /// The request line (without the newline).
    pub fn line(&self) -> String {
        let mut l = format!("CTRL {}", self.tag);
        match &self.start {
            ControlStart::S1 { every, lines, host } => {
                l.push_str(&format!(" every={every}"));
                match lines {
                    Some(LineFilter::All) => l.push_str(" lines=*"),
                    Some(LineFilter::Prefix(p)) => l.push_str(&format!(" lines={p}")),
                    None => {}
                }
                for h in host {
                    l.push(' ');
                    l.push_str(h);
                }
            }
            ControlStart::Snapshot { name, keep_plan } => {
                l.push_str(&format!(" from={name}"));
                if !keep_plan {
                    l.push_str(" plan=drop");
                }
            }
        }
        l
    }

    fn check(&self) -> Result<(), EngineError> {
        let word = |s: &str| !s.is_empty() && s.len() <= 63 && !s.contains(char::is_whitespace);
        if !word(&self.tag) {
            return Err(EngineError::Protocol(format!(
                "bad tag {:?} (one word, at most 63 bytes)",
                self.tag
            )));
        }
        match &self.start {
            ControlStart::S1 { every, lines, host } => {
                if *every == 0 {
                    return Err(EngineError::Protocol("every must be at least 1".into()));
                }
                if let Some(LineFilter::Prefix(p)) = lines {
                    if !word(p) || p == "*" {
                        return Err(EngineError::Protocol(format!("bad line prefix {p:?}")));
                    }
                }
                if let Some(h) = host.iter().find(|h| !word(h)) {
                    return Err(EngineError::Protocol(format!("bad host token {h:?}")));
                }
            }
            ControlStart::Snapshot { name, .. } => {
                if !snapshot_name_ok(name) {
                    return Err(EngineError::Protocol(format!("bad snapshot name {name:?}")));
                }
            }
        }
        Ok(())
    }
}

/// The names the engine accepts for snapshots: 1-63 of `A-Z a-z 0-9 . _ -`.
pub fn snapshot_name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// A console line the engine reported (`EVENT`): its bytes without the line end, and the cadence
/// point at which the engine first saw it complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub at: VTime,
    pub text: Vec<u8>,
}

impl Event {
    pub fn text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.text).into_owned()
    }
}

/// A WATCH text that appeared (`MATCH`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub at: VTime,
    pub text: Vec<u8>,
}

/// A SEND that the engine typed (`TYPED`): at `at`, requested for `req` (hvm types exactly at the
/// requested exit; hvx at the first stop at or after it), `len` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typed {
    pub at: VTime,
    pub req: VTime,
    pub len: u32,
}

/// A stop: what the engine reported since the last one, and where it waits now.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Stop {
    pub at: VTime,
    /// Some when the guest cannot go on (`halt`, `idle`, `limit`, `spin`, `input-overflow`): then
    /// only [`LockStep::fingerprint`], [`LockStep::snap`] and [`LockStep::end`] make sense.
    pub end: Option<String>,
    pub typed: Vec<Typed>,
    pub events: Vec<Event>,
    pub matches: Vec<Match>,
}

/// The answer to `SNAP`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapAnswer {
    /// Taken at this time.
    Taken(VTime),
    /// Refused: this many typed bytes still wait in the receive queue.
    InputPending(u32),
}

/// One engine line of a control session.
#[derive(Clone, Debug, PartialEq)]
pub enum EngineLine {
    Typed(String, Typed),
    Event(String, Event),
    Match(String, Match),
    Stop(String, VTime, Option<String>),
    Ok(String),
    Err(String),
    Result(RunResult),
}

impl EngineLine {
    pub fn parse(line: &str) -> Option<EngineLine> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(r) = line.strip_prefix("OK ") {
            return Some(EngineLine::Ok(r.to_string()));
        }
        if let Some(r) = line.strip_prefix("ERR ") {
            return Some(EngineLine::Err(r.to_string()));
        }
        if line.starts_with("RESULT ") {
            return RunResult::parse(line).map(EngineLine::Result);
        }
        let (kind, rest) = line.split_once(' ')?;
        let (tag, rest) = rest.split_once(' ')?;
        let rest = rest.strip_prefix("at=")?;
        let (at, rest) = match rest.split_once(' ') {
            Some((a, r)) => (a, Some(r)),
            None => (rest, None),
        };
        let at: VTime = at.parse().ok()?;
        let tag = tag.to_string();
        match kind {
            "STOP" => {
                let end = match rest {
                    None => None,
                    Some(r) => Some(r.strip_prefix("end=")?.to_string()),
                };
                Some(EngineLine::Stop(tag, at, end))
            }
            "EVENT" => Some(EngineLine::Event(
                tag,
                Event {
                    at,
                    text: unescape(rest.unwrap_or(""))?,
                },
            )),
            "MATCH" => Some(EngineLine::Match(
                tag,
                Match {
                    at,
                    text: unescape(rest.unwrap_or(""))?,
                },
            )),
            "TYPED" => {
                let mut req = None;
                let mut len = None;
                for w in rest?.split(' ') {
                    if let Some(v) = w.strip_prefix("req=") {
                        req = v.parse().ok();
                    } else if let Some(v) = w.strip_prefix("len=") {
                        len = v.parse().ok();
                    } else {
                        return None;
                    }
                }
                Some(EngineLine::Typed(
                    tag,
                    Typed {
                        at,
                        req: req?,
                        len: len?,
                    },
                ))
            }
            _ => None,
        }
    }
}

/// The protocol's escaping of bytes into one line (what the engine prints for EVENT and MATCH).
pub fn escape(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\\' => s.push_str("\\\\"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            0x20..=0x7e => s.push(b as char),
            _ => s.push_str(&format!("\\x{b:02x}")),
        }
    }
    s
}

/// The inverse of [`escape`] (also accepts any byte that needs no escape); None if malformed.
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

/// The receive queue of both engines holds 4,096 bytes; one SEND must fit in it.
pub const MAX_SEND: usize = 4096;

/// A worker that can run control sessions (see the module documentation). One session at a time:
/// [`begin`](LockStep::begin), then any number of commands and [`cont`](LockStep::cont), then
/// [`end`](LockStep::end).
pub trait LockStep {
    /// Start a session; returns its first stop (at the start point). A request the engine refuses
    /// (a host token it cannot apply, an unknown snapshot) is `EngineError::Refused` with the
    /// engine's verdict text.
    fn begin(&mut self, req: &ControlRequest) -> Result<Stop, EngineError>;

    /// Type `bytes` at virtual time `at` (not before the current stop).
    fn send(&mut self, at: VTime, bytes: &[u8]) -> Result<(), EngineError>;

    /// Stop at `at` (after the current stop) even if nothing is reported there.
    fn wake(&mut self, at: VTime) -> Result<(), EngineError>;

    /// Stop at the first later cadence point at which `text` occurs in the session's console.
    fn watch(&mut self, text: &[u8]) -> Result<(), EngineError>;

    /// Snapshot here (see the module documentation for when the engine refuses).
    fn snap(&mut self, name: &str) -> Result<SnapAnswer, EngineError>;

    /// The fingerprint of the state at this stop (hex, 16 digits).
    fn fingerprint(&mut self) -> Result<String, EngineError>;

    /// Run to the next stop.
    fn cont(&mut self) -> Result<Stop, EngineError>;

    /// End the session: the RESULT line (`fp` = the state now, `wfp` = the last
    /// [`fingerprint`](LockStep::fingerprint) or zeros, extras `ctl=` (a hash of what was typed
    /// and when) and `stops=`).
    fn end(&mut self) -> Result<RunResult, EngineError>;
}

/// The two directions of a line protocol (the engine process, or a scripted engine in tests).
pub(crate) trait Lines {
    fn send_line(&mut self, line: &str) -> Result<(), EngineError>;
    fn read_line(&mut self) -> Result<String, EngineError>;
}

/// [`LockStep`] over any [`Lines`]: the session's tag and whether a session is open.
pub(crate) struct Session<'a, L: Lines + ?Sized> {
    pub(crate) io: &'a mut L,
    pub(crate) tag: &'a mut Option<String>,
}

impl<L: Lines + ?Sized> Session<'_, L> {
    fn tag(&self) -> Result<&str, EngineError> {
        self.tag
            .as_deref()
            .ok_or_else(|| EngineError::Protocol("no control session is open".into()))
    }

    fn line(&mut self) -> Result<EngineLine, EngineError> {
        let l = self.io.read_line()?;
        EngineLine::parse(&l).ok_or(EngineError::Protocol(l))
    }

    fn own(&self, tag: &str) -> Result<(), EngineError> {
        if tag != self.tag()? {
            return Err(EngineError::Protocol(format!(
                "a line of session {tag:?} during session {:?}",
                self.tag()?
            )));
        }
        Ok(())
    }

    /// Read TYPED/EVENT/MATCH lines up to the next STOP.
    fn next_stop(&mut self) -> Result<Stop, EngineError> {
        let mut st = Stop::default();
        loop {
            match self.line()? {
                EngineLine::Typed(t, x) => {
                    self.own(&t)?;
                    st.typed.push(x)
                }
                EngineLine::Event(t, e) => {
                    self.own(&t)?;
                    st.events.push(e)
                }
                EngineLine::Match(t, m) => {
                    self.own(&t)?;
                    st.matches.push(m)
                }
                EngineLine::Stop(t, at, end) => {
                    self.own(&t)?;
                    st.at = at;
                    st.end = end;
                    return Ok(st);
                }
                EngineLine::Result(r) if self.tag.is_some() && r.tag == self.tag()? => {
                    // the engine refused the request
                    *self.tag = None;
                    return Err(EngineError::Refused(r.verdict));
                }
                other => {
                    return Err(EngineError::Protocol(format!(
                        "expected a stop, got {other:?}"
                    )))
                }
            }
        }
    }

    /// One command with one OK/ERR answer: Ok(text after "OK "), or Refused(text after "ERR ").
    fn command(&mut self, line: &str) -> Result<String, EngineError> {
        self.tag()?;
        self.io.send_line(line)?;
        match self.line()? {
            EngineLine::Ok(r) => Ok(r),
            EngineLine::Err(r) => Err(EngineError::Refused(r)),
            other => Err(EngineError::Protocol(format!(
                "expected OK or ERR, got {other:?}"
            ))),
        }
    }
}

impl<L: Lines + ?Sized> LockStep for Session<'_, L> {
    fn begin(&mut self, req: &ControlRequest) -> Result<Stop, EngineError> {
        if let Some(t) = self.tag.as_deref() {
            return Err(EngineError::Protocol(format!(
                "control session {t:?} is still open"
            )));
        }
        req.check()?;
        self.io.send_line(&req.line())?;
        *self.tag = Some(req.tag.clone());
        self.next_stop()
    }

    fn send(&mut self, at: VTime, bytes: &[u8]) -> Result<(), EngineError> {
        if bytes.len() > MAX_SEND {
            return Err(EngineError::Protocol(format!(
                "a SEND of {} bytes does not fit in the {MAX_SEND}-byte receive queue",
                bytes.len()
            )));
        }
        self.command(&format!("SEND at={at} {}", escape(bytes)))
            .map(|_| ())
    }

    fn wake(&mut self, at: VTime) -> Result<(), EngineError> {
        self.command(&format!("WAKE at={at}")).map(|_| ())
    }

    fn watch(&mut self, text: &[u8]) -> Result<(), EngineError> {
        self.command(&format!("WATCH {}", escape(text))).map(|_| ())
    }

    fn snap(&mut self, name: &str) -> Result<SnapAnswer, EngineError> {
        if !snapshot_name_ok(name) {
            return Err(EngineError::Protocol(format!("bad snapshot name {name:?}")));
        }
        match self.command(&format!("SNAP {name}")) {
            Ok(r) => {
                let at = r
                    .split(' ')
                    .find_map(|w| w.strip_prefix("at="))
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| EngineError::Protocol(format!("OK {r}")))?;
                Ok(SnapAnswer::Taken(at))
            }
            Err(EngineError::Refused(r)) => {
                match r
                    .split(' ')
                    .find_map(|w| w.strip_prefix("input-pending="))
                    .and_then(|v| v.parse().ok())
                {
                    Some(n) => Ok(SnapAnswer::InputPending(n)),
                    None => Err(EngineError::Refused(r)),
                }
            }
            Err(e) => Err(e),
        }
    }

    fn fingerprint(&mut self) -> Result<String, EngineError> {
        let r = self.command("FP")?;
        r.split(' ')
            .find_map(|w| w.strip_prefix("fp="))
            .map(str::to_string)
            .ok_or(EngineError::Protocol(format!("OK {r}")))
    }

    fn cont(&mut self) -> Result<Stop, EngineError> {
        self.tag()?;
        self.io.send_line("CONT")?;
        self.next_stop()
    }

    fn end(&mut self) -> Result<RunResult, EngineError> {
        let tag = self.tag()?.to_string();
        self.io.send_line("END")?;
        let r = match self.line()? {
            EngineLine::Result(r) if r.tag == tag => r,
            other => {
                return Err(EngineError::Protocol(format!(
                    "expected the RESULT of {tag}, got {other:?}"
                )))
            }
        };
        *self.tag = None;
        Ok(r)
    }
}

/// hvx_explore's way of combining a run's two fingerprints into the RESULT's `fp`: 64-bit FNV-1a
/// over the 8 little-endian bytes of the final fingerprint, starting from the workload's. A
/// controller that reproduces a plain RUN (FP at the workload's end, then END) gets RUN's `fp`.
pub fn combine_fp(final_fp: u64, workload_fp: u64) -> u64 {
    let mut h = workload_fp;
    for b in final_fp.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A scripted engine: checks each host line against the script and answers with its lines.
    struct Scripted {
        script: VecDeque<(&'static str, Vec<&'static str>)>,
        out: VecDeque<String>,
    }

    impl Lines for Scripted {
        fn send_line(&mut self, line: &str) -> Result<(), EngineError> {
            let (want, answer) = self.script.pop_front().expect("unexpected host line");
            assert_eq!(line, want);
            self.out
                .extend(answer.into_iter().map(|s| format!("{s}\n")));
            Ok(())
        }
        fn read_line(&mut self) -> Result<String, EngineError> {
            self.out
                .pop_front()
                .ok_or(EngineError::Protocol("the engine exited".into()))
        }
    }

    #[test]
    fn escapes_round_trip() {
        let all: Vec<u8> = (0..=255).collect();
        let e = escape(&all);
        assert!(e.bytes().all(|b| (0x20..=0x7e).contains(&b)));
        assert_eq!(unescape(&e).unwrap(), all);
        assert_eq!(escape(b"run K=a\\b\n"), "run K=a\\\\b\\n");
        assert_eq!(unescape("a\\x41\\t").unwrap(), b"aA\t");
        assert!(unescape("\\q").is_none());
        assert!(unescape("\\x4").is_none());
        assert!(unescape("\\xzz").is_none());
        assert!(unescape("end\\").is_none());
    }

    #[test]
    fn engine_lines() {
        assert_eq!(
            EngineLine::parse("STOP a at=19899\n"),
            Some(EngineLine::Stop("a".into(), 19899, None))
        );
        assert_eq!(
            EngineLine::parse("STOP a at=5 end=halt"),
            Some(EngineLine::Stop("a".into(), 5, Some("halt".into())))
        );
        assert_eq!(
            EngineLine::parse("EVENT a at=7 hydo-ob: KILLED redis\\r"),
            Some(EngineLine::Event(
                "a".into(),
                Event {
                    at: 7,
                    text: b"hydo-ob: KILLED redis\r".to_vec()
                }
            ))
        );
        assert_eq!(
            EngineLine::parse("EVENT a at=7 "),
            Some(EngineLine::Event(
                "a".into(),
                Event {
                    at: 7,
                    text: vec![]
                }
            ))
        );
        assert_eq!(
            EngineLine::parse("TYPED a at=9 req=9 len=17"),
            Some(EngineLine::Typed(
                "a".into(),
                Typed {
                    at: 9,
                    req: 9,
                    len: 17
                }
            ))
        );
        assert!(EngineLine::parse("TYPED a at=9 len=17").is_none());
        assert!(EngineLine::parse("STOP a at=x").is_none());
        assert!(EngineLine::parse("STOP a at=5 why=halt").is_none());
        assert!(EngineLine::parse("hvmx: S1 at 123").is_none());
        assert!(matches!(
            EngineLine::parse("RESULT a fp=1 wfp=2 secs=0.1 ctl=3 stops=4 verdict=OK"),
            Some(EngineLine::Result(_))
        ));
    }

    #[test]
    fn request_lines() {
        let r = ControlRequest::from_s1("a", 1, &["disk=none", "tj=7"]);
        assert_eq!(r.line(), "CTRL a every=1 disk=none tj=7");
        let mut r = ControlRequest::from_s1("b", 16, &[]);
        if let ControlStart::S1 { lines, .. } = &mut r.start {
            *lines = Some(LineFilter::All);
        }
        assert_eq!(r.line(), "CTRL b every=16 lines=*");
        let mut s = ControlRequest::from_snapshot("v1", "br");
        assert_eq!(s.line(), "CTRL v1 from=br");
        s.start = ControlStart::Snapshot {
            name: "br".into(),
            keep_plan: false,
        };
        assert_eq!(s.line(), "CTRL v1 from=br plan=drop");
        assert!(ControlRequest::from_s1("two words", 1, &[])
            .check()
            .is_err());
        assert!(ControlRequest::from_s1("a", 0, &[]).check().is_err());
        assert!(ControlRequest::from_snapshot("a", "no/slash")
            .check()
            .is_err());
        assert!(snapshot_name_ok("br-1.x_y"));
    }

    #[test]
    fn a_session() {
        let mut io = Scripted {
            script: VecDeque::from(vec![
                ("CTRL a every=1 disk=none", vec!["STOP a at=100"]),
                ("SEND at=100 run\\n", vec!["OK SEND at=100 len=4"]),
                ("WATCH done", vec!["OK WATCH len=4"]),
                ("WAKE at=101", vec!["OK WAKE at=101"]),
                (
                    "CONT",
                    vec!["TYPED a at=100 req=100 len=4", "STOP a at=101"],
                ),
                ("SNAP s", vec!["ERR SNAP s input-pending=3"]),
                ("SEND at=99 x", vec!["ERR SEND at=99 is before now (101)"]),
                (
                    "CONT",
                    vec![
                        "EVENT a at=150 HYDO-AG 1 2 {}",
                        "MATCH a at=150 done",
                        "STOP a at=150",
                    ],
                ),
                ("SNAP s", vec!["OK SNAP s at=150"]),
                ("FP", vec!["OK FP at=150 fp=00000000000000ff"]),
                (
                    "END",
                    vec!["RESULT a fp=1 wfp=00000000000000ff secs=0.1 ctl=3 stops=3 verdict=CONTROL ended at=150"],
                ),
                ("CTRL b from=nope", vec!["RESULT b fp=0 wfp=0 secs=0 verdict=ERROR no snapshot nope"]),
            ]),
            out: VecDeque::new(),
        };
        let mut tag = None;
        let mut s = Session {
            io: &mut io,
            tag: &mut tag,
        };
        let st = s
            .begin(&ControlRequest::from_s1("a", 1, &["disk=none"]))
            .unwrap();
        assert_eq!(st.at, 100);
        s.send(100, b"run\n").unwrap();
        s.watch(b"done").unwrap();
        s.wake(101).unwrap();
        let st = s.cont().unwrap();
        assert_eq!(st.at, 101);
        assert_eq!(
            st.typed,
            vec![Typed {
                at: 100,
                req: 100,
                len: 4
            }]
        );
        assert_eq!(s.snap("s").unwrap(), SnapAnswer::InputPending(3));
        assert!(matches!(s.send(99, b"x"), Err(EngineError::Refused(_))));
        let st = s.cont().unwrap();
        assert_eq!(st.events[0].text, b"HYDO-AG 1 2 {}");
        assert_eq!(st.matches[0].text, b"done");
        assert_eq!(s.snap("s").unwrap(), SnapAnswer::Taken(150));
        assert_eq!(s.fingerprint().unwrap(), "00000000000000ff");
        let r = s.end().unwrap();
        assert_eq!(r.extra("stops"), Some("3"));
        assert!(s.cont().is_err(), "no session is open");
        let e = s
            .begin(&ControlRequest::from_snapshot("b", "nope"))
            .unwrap_err();
        assert!(matches!(e, EngineError::Refused(v) if v == "ERROR no snapshot nope"));
        assert!(s.tag.is_none());
    }

    #[test]
    fn combine_is_hvx_explore_s() {
        // the Redis quickstart's `disk=none | run` on the Mac: RESULT fp=0f6aac764c64aac2
        // wfp=c970e779eaca9d75; the final fingerprint is not printed, so check the function on
        // FNV-1a's own definition instead
        let mut h: u64 = 0xcbf29ce484222325;
        for b in 0x0102030405060708u64.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        assert_eq!(combine_fp(0x0102030405060708, 0xcbf29ce484222325), h);
    }
}
