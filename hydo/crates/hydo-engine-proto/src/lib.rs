#![doc = include_str!("../README.md")]
//!
//! ## Notes for hydo's developers
//!
//! The calls between hydo and its run engines, as Rust types, and their one text form.
//!
//! An engine boots an agent image under lock-step control (the host takes its snapshot points),
//! then serves requests: a run under lock-step control from a snapshot the host took
//! ([`Request::Ctl`], [`Request::Resume`]). Every run is controlled; the plain run `RUN` and the
//! timing branch `BRANCH` of the shell images were deleted in Milestone 145. During a run it
//! reports what happened up to each numbered stop ([`EngineMsg::Event`], [`EngineMsg::Sent`],
//! [`EngineMsg::Drop`], [`EngineMsg::Stop`]) and takes the host's commands there ([`HostCmd`]),
//! answering each but CONT and END ([`Answer`]); every run ends with one [`RunResult`].
//!
//! Inside one process these values travel as they are: hydo-engine runs the hvx engine
//! (`hydo-hvx`) on a thread and the two exchange [`HostMsg`] and [`EngineMsg`] over channels
//! (M142). Where a process boundary remains (the program `hydo-hvx` for users who cannot open
//! `/dev/hvx`, and the Mac engine `hydo-hvm`), each value is one line of text:
//! [`HostMsg::line`], [`EngineMsg::line`] and [`EngineMsg::parse`] are that text, the protocol
//! the C engine `hvx_explore.c` defined (M118) and both engines print. The text is the same in
//! both directions and for both engines, so a transcript (`< ` host lines, `> ` engine lines) reads
//! the same whichever way a run went.
//!
//! ```text
//! host -> engine (serve):     CTL <tag> <schedule> | RESUME <tag> <snapshot> | QUIT
//! engine -> host:             READY boot_secs=<s>
//!                             RESULT <tag> fp=<hex16> wfp=<hex16> secs=<s>[ <key>=<value>...] verdict=<text>
//! engine -> host, per stop:   EVENT <tag> at=<icount> n=<k> <escaped line>
//!                             SENT <tag> id=<id> at=<icount>[ retype=1]
//!                             DROP <tag> id=<id>
//!                             STOP <tag> at=<icount> n=<k> phase=<p> why=<w>
//! host -> engine, at a stop:  SEND at=<icount> <escaped bytes>   -> OK send id=<id> sent=|queued=<n>
//!                             ACK <id>                           -> OK ack <id>
//!                             SNAP <name> [retype]               -> OK snap <name> at=<n> retype=<ids>|-
//!                             DISKOFF                            -> OK diskoff at=<n> cut=<0|1>
//!                             CUT                                -> OK cut at=<n>
//!                             CONT | END [<verdict>]             (refusals: ERR <reason>)
//! ```
//!
//! # Adding to the protocol
//!
//! A new engine request is a variant of [`Request`] (with its line in [`Request::line`]); a new
//! command at a stop is a variant of [`HostCmd`] (line and parse) with its [`Answer`]; a new
//! engine report is a variant of [`EngineMsg`] (line and parse). The engines' handling follows:
//! `hydo-hvx`'s `engine.rs` (typed) and its text reader `text.rs` (lines of the program),
//! `hydo-engine`'s client ([`lockstep`](../hydo_engine/lockstep/index.html)), and, for the Mac,
//! `hydo-hvm`'s `serve.rs` (the Mac engine). The round-trip tests here and
//! `hydo-engine/tests/lockstep.rs` (every line of the recorded transcripts of both engines prints
//! back byte for byte) keep the two forms one.

use hydo_schedule::Schedule;

// ------------------------------------------------------------------------------------ escaping

/// The engines' escaping of console bytes and send payloads: printable ASCII as is, `\\`, `\n`,
/// `\r`, `\t`, every other byte as `\xHH`.
pub fn escape(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len());
    escape_into(b, &mut s);
    s
}

/// [`escape`] appended to `s`.
pub fn escape_into(b: &[u8], s: &mut String) {
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

// ------------------------------------------------------------------------------------ stops

/// Which part of the run a stop belongs to. Each phase that starts a VM has its own clock:
/// the recovery boot after a power cut restarts from S0's icount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// An agent image's boot under control, from its first instruction (M121, `--control boot`):
    /// the host takes S0 and S1 here with SNAP.
    Boot,
    /// From S1 (or a resumed snapshot) to the end of the workload.
    Workload,
    /// The same VM after the workload's end (heal and check).
    Check,
    /// The recovery VM after a power cut (restored from S0 with the flushed disk).
    Recovery,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Boot => "boot",
            Phase::Workload => "workload",
            Phase::Check => "check",
            Phase::Recovery => "recovery",
        }
    }

    pub fn parse(s: &str) -> Option<Phase> {
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
    /// The shell init's workload or check marker appeared.
    Marker,
    /// The guest stopped (the run ends with an error RESULT after this stop).
    Exit,
    /// M138: the guest committed a record on the host-call port; the run call returned there.
    Hostcall,
}

impl StopWhy {
    pub fn name(self) -> &'static str {
        match self {
            StopWhy::Start => "start",
            StopWhy::Resume => "resume",
            StopWhy::Cadence => "cadence",
            StopWhy::Send => "send",
            StopWhy::Marker => "marker",
            StopWhy::Exit => "exit",
            StopWhy::Hostcall => "hostcall",
        }
    }

    pub fn parse(s: &str) -> Option<StopWhy> {
        Some(match s {
            "start" => StopWhy::Start,
            "resume" => StopWhy::Resume,
            "cadence" => StopWhy::Cadence,
            "send" => StopWhy::Send,
            "marker" => StopWhy::Marker,
            "exit" => StopWhy::Exit,
            "hostcall" => StopWhy::Hostcall,
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

// ------------------------------------------------------------------------------------ host side

/// One command of the host at a stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostCmd {
    /// Type `bytes` (1..=4096) into the serial input at the first stop with icount >= `at`.
    Send {
        at: u64,
        bytes: Vec<u8>,
    },
    /// The guest consumed send `id`.
    Ack(u64),
    /// Snapshot at this stop under `name`.
    Snap {
        name: String,
        retype: bool,
    },
    /// The workload has ended (agent images, M126): at the next CONT the engine takes the workload
    /// fingerprint and switches the disk faults off, as at a shell image's workload marker.
    DiskOff,
    /// A power cut at this stop (agent images, M126): the run goes on in the recovery VM.
    Cut,
    /// `TIME <host tokens>`: the run's timing from this stop on (the state search's branch point,
    /// docs/design/state-search.md): tick jitter and forced preemption points, the points drawn
    /// among the next `pctk` exits. Empty tokens switch the timing off. Answer `OK time at=<n>`.
    Time(String),
    Cont,
    End(Option<String>),
}

impl HostCmd {
    /// The line as the engines read it (without the newline).
    pub fn line(&self) -> String {
        match self {
            HostCmd::Send { at, bytes } => format!("SEND at={at} {}", escape(bytes)),
            HostCmd::Ack(id) => format!("ACK {id}"),
            HostCmd::Snap { name, retype } => {
                format!("SNAP {name}{}", if *retype { " retype" } else { "" })
            }
            HostCmd::DiskOff => "DISKOFF".into(),
            HostCmd::Cut => "CUT".into(),
            HostCmd::Time(t) if t.is_empty() => "TIME".into(),
            HostCmd::Time(t) => format!("TIME {t}"),
            HostCmd::Cont => "CONT".into(),
            HostCmd::End(None) => "END".into(),
            HostCmd::End(Some(v)) => format!("END {v}"),
        }
    }

    /// Parse a host line in the form [`HostCmd::line`] prints (used to replay transcripts; the
    /// hvx program's own reader, `hydo-hvx/src/text.rs`, also takes the C engine's looser forms).
    pub fn parse(line: &str) -> Option<HostCmd> {
        match line {
            "CONT" => return Some(HostCmd::Cont),
            "DISKOFF" => return Some(HostCmd::DiskOff),
            "CUT" => return Some(HostCmd::Cut),
            "TIME" => return Some(HostCmd::Time(String::new())),
            "END" => return Some(HostCmd::End(None)),
            _ => {}
        }
        if let Some(v) = line.strip_prefix("END ") {
            return Some(HostCmd::End(Some(v.to_string())));
        }
        if let Some(v) = line.strip_prefix("TIME ") {
            return Some(HostCmd::Time(v.to_string()));
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

/// A request to a serving engine.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// One schedule's host part from the snapshot S1 the host took in the boot (and, after a
    /// power cut, its recovery VM from the host's S0), under lock-step control (M118, M121): the
    /// engine stops and waits for [`HostCmd`]s. The guest part must be just `run`, and nothing is
    /// typed.
    Ctl {
        tag: String,
        schedule: Schedule,
    },
    /// A controlled run from a snapshot an earlier controlled run of this worker took.
    Resume {
        tag: String,
        snapshot: String,
    },
    Quit,
}

impl Request {
    pub fn line(&self) -> String {
        match self {
            Request::Ctl { tag, schedule } => format!("CTL {tag} {schedule}"),
            Request::Resume { tag, snapshot } => format!("RESUME {tag} {snapshot}"),
            Request::Quit => "QUIT".into(),
        }
    }
}

/// What the host sends: a request (between runs) or a command (at a stop).
#[derive(Clone, Debug, PartialEq)]
pub enum HostMsg {
    Request(Request),
    Cmd(HostCmd),
}

impl HostMsg {
    pub fn line(&self) -> String {
        match self {
            HostMsg::Request(r) => r.line(),
            HostMsg::Cmd(c) => c.line(),
        }
    }
}

// ------------------------------------------------------------------------------------ answers

/// The most SENDs one run holds, on every engine. It guards against a host that sends without
/// end; a run of a slow workload sends some thousands (queue polls, checks and clock reads). It
/// was 1,024 before open item 83, the size of the C engines' array.
pub const MAX_SENDS: usize = 65536;

/// The refusal of a SEND past [`MAX_SENDS`].
pub fn too_many_sends(max: usize) -> String {
    format!(
        "send more than {max} sends in one run (the limit against a host controller that sends \
         without end)"
    )
}

/// The answer to a SEND.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendAnswer {
    /// Typed at this stop (its `at` was not after the stop's icount, and its bytes fit in the
    /// receive queue).
    Typed { id: u64, at: u64 },
    /// Queued: due later, or due now and waiting for room in the receive queue (the Mac and KVM
    /// engines). A later [`EngineMsg::Sent`] says when it was typed.
    Queued { id: u64, at: u64 },
}

impl SendAnswer {
    pub fn id(&self) -> u64 {
        match *self {
            SendAnswer::Typed { id, .. } | SendAnswer::Queued { id, .. } => id,
        }
    }
}

/// The answer to DISKOFF: the stop's time, and whether the schedule's host part makes this the
/// power cut (`cf=`: the disk lost power at the k-th flush, so the run goes on in the recovery VM
/// at the next CONT).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskOff {
    pub at: u64,
    pub cut: bool,
}

/// The engine's answer to a command at a stop (every command but CONT and END has one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Send(SendAnswer),
    Ack(u64),
    /// A snapshot taken at `at`; `retype` lists the unacknowledged sends a RESUME types again.
    Snap {
        name: String,
        at: u64,
        retype: Vec<u64>,
    },
    DiskOff(DiskOff),
    Cut {
        at: u64,
    },
    /// `OK time at=<n>`: the timing of `TIME` is in force from the stop at `at`.
    Time {
        at: u64,
    },
    /// `ERR <reason>`: the command was refused (the reason starts with the command's word, for
    /// example `snap s unacked=1,2`).
    Refused(String),
}

impl Answer {
    pub fn line(&self) -> String {
        match self {
            Answer::Send(SendAnswer::Typed { id, at }) => format!("OK send id={id} sent={at}"),
            Answer::Send(SendAnswer::Queued { id, at }) => format!("OK send id={id} queued={at}"),
            Answer::Ack(id) => format!("OK ack {id}"),
            Answer::Snap { name, at, retype } => {
                format!("OK snap {name} at={at} retype={}", retype_text(retype))
            }
            Answer::DiskOff(d) => format!("OK diskoff at={} cut={}", d.at, d.cut as u8),
            Answer::Cut { at } => format!("OK cut at={at}"),
            Answer::Time { at } => format!("OK time at={at}"),
            Answer::Refused(r) => format!("ERR {r}"),
        }
    }

    /// The answer in `OK <rest>` / `ERR <rest>` form.
    pub fn parse(line: &str) -> Option<Answer> {
        if let Some(r) = line.strip_prefix("ERR ") {
            return Some(Answer::Refused(r.to_string()));
        }
        let r = line.strip_prefix("OK ")?;
        let mut w = r.split(' ');
        let kind = w.next()?;
        let kv = |w: Option<&str>, k: &str| -> Option<u64> { w?.strip_prefix(k)?.parse().ok() };
        let a = match kind {
            "send" => {
                let id = kv(w.next(), "id=")?;
                let t = w.next()?;
                if let Some(v) = t.strip_prefix("sent=") {
                    Answer::Send(SendAnswer::Typed {
                        id,
                        at: v.parse().ok()?,
                    })
                } else {
                    Answer::Send(SendAnswer::Queued {
                        id,
                        at: t.strip_prefix("queued=")?.parse().ok()?,
                    })
                }
            }
            "ack" => Answer::Ack(w.next()?.parse().ok()?),
            "snap" => {
                let name = w.next()?.to_string();
                let at = kv(w.next(), "at=")?;
                let v = w.next()?.strip_prefix("retype=")?;
                let retype = if v == "-" {
                    Vec::new()
                } else {
                    v.split(',')
                        .map(|x| x.parse().ok())
                        .collect::<Option<_>>()?
                };
                Answer::Snap { name, at, retype }
            }
            "diskoff" => {
                let at = kv(w.next(), "at=")?;
                let cut = match w.next()? {
                    "cut=1" => true,
                    "cut=0" => false,
                    _ => return None,
                };
                Answer::DiskOff(DiskOff { at, cut })
            }
            "cut" => Answer::Cut {
                at: kv(w.next(), "at=")?,
            },
            "time" => Answer::Time {
                at: kv(w.next(), "at=")?,
            },
            _ => return None,
        };
        if w.next().is_some() {
            return None;
        }
        Some(a)
    }
}

/// The ids a SNAP answer lists: `-`, or the ids joined by `,`; a list that would not fit the C
/// engine's 512-byte buffer is cut there and ends with `,...` (as hvx_explore.c printed it).
pub fn retype_text(ids: &[u64]) -> String {
    if ids.is_empty() {
        return "-".into();
    }
    let mut s = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    if s.len() >= 512 {
        s.truncate(512 - 5);
        s.push_str(",...");
    }
    s
}

// ------------------------------------------------------------------------------------ results

/// One RESULT line.
#[derive(Clone, Debug, PartialEq)]
pub struct RunResult {
    pub tag: String,
    /// The final fingerprint (after the recovery boot when there was one).
    pub fp: String,
    /// The workload's fingerprint (at the end of the first boot's workload).
    pub wfp: String,
    /// Host seconds the run took (the engine's own measure).
    pub secs: f64,
    /// The text of `secs=` exactly as printed (reports print the text).
    pub secs_text: String,
    /// `key=value` facts between `secs=` and `verdict=`, in order: `tjk=<a>-<b> tjn=<n>` (tick
    /// jitter), `xtn=` (extra ticks), `qn=`/`qbn=` (vCPU turns), `pctf=`/`pctw=` (the Mac's
    /// forced preemption points), `stops=`/`typed=`/`inh=` (a controlled run), `ctl=`.
    pub extras: Vec<(String, String)>,
    /// Everything after `verdict=` (the guest's `HYDO-ORACLE: ...` line, or an engine ERROR/TIMEOUT).
    pub verdict: String,
    /// The whole line, trimmed.
    pub line: String,
}

impl RunResult {
    /// The result an engine reports: `extras` is the engine's text between `secs=` and
    /// `verdict=` (empty, or ` <key>=<value>...` with a leading space). The value is the one
    /// [`RunResult::parse`] reads from the line the engine prints for it, so a result is the same
    /// whether it came over a channel or as text. None: the parts do not make a RESULT line.
    pub fn new(
        tag: &str,
        fp: &str,
        wfp: &str,
        secs_text: &str,
        extras: &str,
        verdict: &str,
    ) -> Option<RunResult> {
        RunResult::parse(&format!(
            "RESULT {tag} fp={fp} wfp={wfp} secs={secs_text}{extras} verdict={verdict}"
        ))
    }

    /// Parse a RESULT line the way the CLIs and explore.py do (the line is trimmed first; the
    /// extras are `\S+?=\S+` tokens; the verdict is the rest after the first `verdict=` token).
    pub fn parse(line: &str) -> Option<RunResult> {
        let line = line.trim();
        let rest = line.strip_prefix("RESULT ")?;
        let (tag, rest) = split_word(rest)?;
        let (fp, rest) = split_word(rest)?;
        let fp = fp.strip_prefix("fp=")?;
        let (wfp, rest) = split_word(rest)?;
        let wfp = wfp.strip_prefix("wfp=")?;
        let (secs, mut rest) = split_word(rest)?;
        let secs_text = secs.strip_prefix("secs=")?;
        if tag.is_empty() || fp.is_empty() || wfp.is_empty() || secs_text.is_empty() {
            return None;
        }
        let mut extras = Vec::new();
        loop {
            if let Some(v) = rest.strip_prefix("verdict=") {
                return Some(RunResult {
                    tag: tag.to_string(),
                    fp: fp.to_string(),
                    wfp: wfp.to_string(),
                    secs: secs_text.parse().unwrap_or(0.0),
                    secs_text: secs_text.to_string(),
                    extras,
                    verdict: v.to_string(),
                    line: line.to_string(),
                });
            }
            let (w, r) = split_word(rest)?;
            let (k, v) = w.split_once('=')?;
            if k.is_empty() || v.is_empty() {
                return None;
            }
            extras.push((k.to_string(), v.to_string()));
            rest = r;
        }
    }

    pub fn extra(&self, key: &str) -> Option<&str> {
        self.extras
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// explore.py's verdict_kind: "ok", "violation" or "error".
    pub fn kind(&self) -> &'static str {
        verdict_kind(&self.verdict)
    }

    /// True when the run had a recovery boot (the engine saved `<tag>.recovery.log`).
    pub fn recovery(&self, outdir: &std::path::Path) -> bool {
        outdir.join(format!("{}.recovery.log", self.tag)).exists()
    }
}

/// explore.py's verdict_kind: an OK oracle line, else a VIOLATION, else an error.
pub fn verdict_kind(v: &str) -> &'static str {
    if v.contains("HYDO-ORACLE: OK") {
        "ok"
    } else if v.contains("VIOLATION") {
        "violation"
    } else {
        "error"
    }
}

/// `<word> <rest>` with exactly one space between (the engines print single spaces).
fn split_word(s: &str) -> Option<(&str, &str)> {
    let i = s.find(' ')?;
    Some((&s[..i], &s[i + 1..]))
}

/// The READY line of serve mode: the worker booted to S1.
#[derive(Clone, Debug, PartialEq)]
pub struct Ready {
    pub boot_secs: f64,
    /// The line as printed.
    pub line: String,
}

impl Ready {
    /// READY after `secs` seconds (printed with one decimal, as both engines do).
    pub fn new(secs: f64) -> Ready {
        let line = format!("READY boot_secs={secs:.1}");
        Ready::parse(&line).expect("a READY line")
    }

    pub fn parse(line: &str) -> Option<Ready> {
        let v = line
            .trim()
            .strip_prefix("READY")?
            .split("boot_secs=")
            .nth(1)?;
        Some(Ready {
            boot_secs: v.split_whitespace().next()?.parse().ok()?,
            line: line.trim_end_matches(['\n', '\r']).to_string(),
        })
    }
}

// ------------------------------------------------------------------------------------ engine side

/// One report of an engine.
#[derive(Clone, Debug, PartialEq)]
pub enum EngineMsg {
    Ready(Ready),
    Event {
        tag: String,
        event: Event,
    },
    Sent {
        tag: String,
        sent: Sent,
    },
    Drop {
        tag: String,
        id: u64,
    },
    Stop {
        tag: String,
        stop: Stop,
    },
    Answer(Answer),
    Result(Box<RunResult>),
    /// A line of an engine process that is none of the above (text only; the line as read).
    Other(String),
}

impl EngineMsg {
    /// The line as the engines print it (without the newline).
    pub fn line(&self) -> String {
        match self {
            EngineMsg::Ready(r) => r.line.clone(),
            EngineMsg::Event { tag, event } => {
                let mut l = format!("EVENT {tag} at={} n={} ", event.at, event.n);
                escape_into(&event.text, &mut l);
                l
            }
            EngineMsg::Sent { tag, sent } => format!(
                "SENT {tag} id={} at={}{}",
                sent.id,
                sent.at,
                if sent.retype { " retype=1" } else { "" }
            ),
            EngineMsg::Drop { tag, id } => format!("DROP {tag} id={id}"),
            EngineMsg::Stop { tag, stop } => format!(
                "STOP {tag} at={} n={} phase={} why={}",
                stop.at,
                stop.n,
                stop.phase.name(),
                stop.why.name()
            ),
            EngineMsg::Answer(a) => a.line(),
            EngineMsg::Result(r) => r.line.clone(),
            EngineMsg::Other(l) => l.trim_end_matches(['\n', '\r']).to_string(),
        }
    }

    /// Parse one engine line (a trailing newline is cut; a trailing `\r` is not part of the
    /// protocol). A line that is none of the protocol's is [`EngineMsg::Other`].
    pub fn parse(line: &str) -> EngineMsg {
        EngineMsg::parse_protocol(line.trim_end_matches('\n'))
            .unwrap_or_else(|| EngineMsg::Other(line.to_string()))
    }

    fn parse_protocol(line: &str) -> Option<EngineMsg> {
        if line.starts_with("OK ") || line.starts_with("ERR ") {
            return Answer::parse(line).map(EngineMsg::Answer);
        }
        if line.starts_with("RESULT ") {
            return RunResult::parse(line).map(|r| EngineMsg::Result(Box::new(r)));
        }
        if line.starts_with("READY") {
            return Ready::parse(line).map(EngineMsg::Ready);
        }
        if let Some(r) = line.strip_prefix("EVENT ") {
            // EVENT <tag> at=<n> n=<k> <text>: the text may be empty and may contain spaces
            let (tag, r) = r.split_once(' ')?;
            let (at, r) = r.split_once(' ')?;
            let (n, text) = r.split_once(' ')?;
            return Some(EngineMsg::Event {
                tag: tag.to_string(),
                event: Event {
                    at: at.strip_prefix("at=")?.parse().ok()?,
                    n: n.strip_prefix("n=")?.parse().ok()?,
                    text: unescape(text)?,
                },
            });
        }
        let mut w = line.split(' ');
        let kind = w.next()?;
        let tag = w.next()?.to_string();
        let kv: Vec<(&str, &str)> = w.map(|x| x.split_once('=')).collect::<Option<_>>()?;
        let get = |k: &str| kv.iter().find(|(a, _)| *a == k).map(|(_, v)| *v);
        let num = |k: &str| get(k).and_then(|v| v.parse::<u64>().ok());
        match kind {
            "SENT" => Some(EngineMsg::Sent {
                tag,
                sent: Sent {
                    id: num("id")?,
                    at: num("at")?,
                    retype: get("retype") == Some("1"),
                },
            }),
            "DROP" => Some(EngineMsg::Drop {
                tag,
                id: num("id")?,
            }),
            "STOP" => Some(EngineMsg::Stop {
                tag,
                stop: Stop {
                    at: num("at")?,
                    n: num("n")?,
                    phase: Phase::parse(get("phase")?)?,
                    why: StopWhy::parse(get("why")?)?,
                },
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn engine_lines_round_trip() {
        for l in [
            "READY boot_secs=41.7",
            "EVENT a at=7 n=2 KILLED redis",
            "EVENT a at=7 n=3 ",
            "EVENT b at=1 n=1 x\\x01\\\\y",
            "STOP a at=9 n=4 phase=check why=marker",
            "STOP boot at=0 n=1 phase=boot why=start",
            "STOP r at=5 n=2 phase=recovery why=hostcall",
            "SENT b id=1 at=969127 retype=1",
            "SENT b id=2 at=9",
            "DROP a id=5",
            "OK send id=1 queued=300",
            "OK send id=2 sent=301",
            "OK ack 1",
            "OK snap s at=301 retype=-",
            "OK snap S1 at=5 retype=1,2",
            "OK diskoff at=7 cut=1",
            "OK cut at=8",
            "ERR snap s unacked=1",
            "ERR unknown command FOO",
            "RESULT one fp=c513f10aee733aae wfp=1111222233334444 secs=12.3 tjk=3-40 tjn=5 verdict=HYDO-ORACLE: OK acked=20",
            "RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request",
            "hvx_explore: S1",
        ] {
            let m = EngineMsg::parse(l);
            assert_eq!(m.line(), l, "{m:?}");
        }
        assert!(matches!(
            EngineMsg::parse("STOP a at=9 n=4 phase=later why=marker"),
            EngineMsg::Other(_)
        ));
        assert!(matches!(
            EngineMsg::parse("OK snap s at=1 retype=1,..."),
            EngineMsg::Other(_)
        ));
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
            HostCmd::DiskOff,
            HostCmd::Cut,
            HostCmd::Time(String::new()),
            HostCmd::Time("pct=5 pctn=3 pctk=100".into()),
            HostCmd::Cont,
            HostCmd::End(None),
            HostCmd::End(Some("HYDO-ORACLE: OK".into())),
        ] {
            assert_eq!(HostCmd::parse(&c.line()), Some(c.clone()), "{}", c.line());
        }
        let s = Schedule::parse("disk=none rb=16 | run K=redis@5+2").unwrap();
        assert_eq!(
            Request::Ctl {
                tag: "a".into(),
                schedule: s
            }
            .line(),
            "CTL a disk=none rb=16 | run K=redis@5+2"
        );
    }

    #[test]
    fn results() {
        let r = RunResult::parse(
            "RESULT one fp=c513f10aee733aae wfp=1111222233334444 secs=12.3 tjk=3-40 tjn=5 verdict=HYDO-ORACLE: OK acked=20\n",
        )
        .unwrap();
        assert_eq!(r.tag, "one");
        assert_eq!(r.secs, 12.3);
        assert_eq!(r.extra("tjn"), Some("5"));
        assert_eq!(r.verdict, "HYDO-ORACLE: OK acked=20");
        assert_eq!(r.kind(), "ok");
        assert_eq!(
            RunResult::new(
                "one",
                "c513f10aee733aae",
                "1111222233334444",
                "12.3",
                " tjk=3-40 tjn=5",
                "HYDO-ORACLE: OK acked=20"
            ),
            Some(r)
        );
        let e = RunResult::parse("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request").unwrap();
        assert_eq!(e.kind(), "error");
        assert!(RunResult::parse("RESULT x fp=1 wfp=2 verdict=OK").is_none());
        let v = RunResult::parse("RESULT t fp=1 wfp=2 secs=1.0 verdict=").unwrap();
        assert_eq!(v.verdict, "");
        assert_eq!(Ready::new(41.66).line, "READY boot_secs=41.7");
        assert_eq!(
            Ready::parse("READY boot_secs=41.7\n").unwrap().boot_secs,
            41.7
        );
        let ids: Vec<u64> = (1..400).collect();
        let t = retype_text(&ids);
        assert!(t.len() == 511 && t.ends_with(",..."));
    }
}
