//! A started engine and the workers over it: a fresh-boot `one` run of a program, and a `serve`
//! worker over a [`Link`] (an engine program's stdin and stdout, or an engine thread of this
//! process, [`crate::inproc`]).

use crate::link::{Link, TextLink};
use crate::lockstep::{Lockstep, LockstepStart};
use crate::protocol::{BranchOutcome, BranchRequest, EngineMsg, HostMsg, Request, RunResult};
use crate::{EngineError, OneRun, Worker};
use hydo_schedule::Schedule;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

/// What runs the engine: a child process, or a thread of this process (its exit status is the
/// thread's return value).
pub(crate) enum Proc {
    Child(Child),
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Thread(Option<std::thread::JoinHandle<i32>>),
}

impl Proc {
    fn kill(&mut self) {
        // a thread cannot be killed; it ends when its link closes (the caller drops it)
        if let Proc::Child(c) = self {
            let _ = c.kill();
        }
    }

    fn wait(&mut self) -> std::io::Result<Option<i32>> {
        match self {
            Proc::Child(c) => Ok(c.wait()?.code()),
            Proc::Thread(h) => Ok(h.take().and_then(|h| h.join().ok())),
        }
    }
}

/// A started engine: what runs it and the host's end of its conversation.
pub(crate) struct Started {
    pub(crate) proc: Proc,
    pub(crate) link: Box<dyn Link>,
}

/// Run `cmd` to completion with stdout and stderr written into ONE pipe (so the text keeps the
/// order in which the engine wrote it, as Python's `stderr=STDOUT` does) and parse its RESULT.
pub(crate) fn run_one(mut cmd: Command) -> Result<OneRun, EngineError> {
    let t0 = Instant::now();
    let (mut reader, writer) = std::io::pipe()?;
    cmd.stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    let mut child = cmd.spawn()?;
    drop(cmd); // the parent's copies of the write end must be closed, or the read never ends
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    let status = child.wait()?;
    let output = String::from_utf8_lossy(&bytes).into_owned();
    let result = output.lines().find_map(RunResult::parse);
    Ok(OneRun {
        result,
        output,
        exit_code: status.code(),
        wall: t0.elapsed(),
    })
}

/// A `serve` engine: READY, then one RESULT per RUN.
pub(crate) struct ProcessWorker {
    proc: Proc,
    link: Box<dyn Link>,
    boot_secs: f64,
    branch: bool,
}

/// Start `cmd` with stdin/stdout piped (the text link) and stderr appended to `stderr_log`.
fn spawn_piped(mut cmd: Command, stderr_log: &Path) -> Result<Started, EngineError> {
    let err = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(stderr_log)?;
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(err);
    let mut child = cmd.spawn()?;
    let stdin: Box<dyn Write + Send> = Box::new(child.stdin.take().expect("piped stdin"));
    let out: Box<dyn Read + Send> = Box::new(child.stdout.take().expect("piped stdout"));
    Ok(Started {
        proc: Proc::Child(child),
        link: Box::new(TextLink::new(BufReader::new(out), stdin)),
    })
}

/// Read the READY of a serving engine (or stop it and say what it printed instead).
fn read_ready(proc: &mut Proc, link: Box<dyn Link>) -> Result<(f64, Box<dyn Link>), EngineError> {
    let mut link = link;
    let got = link.recv();
    if let Ok(EngineMsg::Ready(r)) = got {
        return Ok((r.boot_secs, link));
    }
    let line = match (&got, link.last_text()) {
        (Ok(_), Some(t)) => t.to_string(),
        (Ok(m), None) => m.line(),
        (Err(_), _) => String::new(),
    };
    proc.kill();
    drop(link);
    let _ = proc.wait();
    Err(EngineError::NotReady(line))
}

impl ProcessWorker {
    /// Start `cmd` with stdin/stdout piped and stderr appended to `stderr_log`, and wait for READY.
    pub(crate) fn start(
        cmd: Command,
        stderr_log: &Path,
        branch: bool,
    ) -> Result<ProcessWorker, EngineError> {
        ProcessWorker::from_started(spawn_piped(cmd, stderr_log)?, branch)
    }

    /// A serve engine already started (a process or a thread): wait for READY.
    pub(crate) fn from_started(s: Started, branch: bool) -> Result<ProcessWorker, EngineError> {
        let Started { mut proc, link } = s;
        let (boot_secs, link) = read_ready(&mut proc, link)?;
        Ok(ProcessWorker {
            proc,
            link,
            boot_secs,
            branch,
        })
    }

    /// The text of a report for an error: the line an engine program printed, or the value's line.
    fn text(&self, m: &EngineMsg) -> String {
        self.link
            .last_text()
            .map(str::to_string)
            .unwrap_or_else(|| m.line())
    }
}

fn check_tag(tag: &str) -> Result<(), EngineError> {
    if tag.is_empty() || tag.contains(char::is_whitespace) || tag.len() > 63 {
        return Err(EngineError::Protocol(format!(
            "bad tag {tag:?} (one word, at most 63 bytes)"
        )));
    }
    Ok(())
}

impl Worker for ProcessWorker {
    fn boot_secs(&self) -> f64 {
        self.boot_secs
    }

    fn run(&mut self, tag: &str, schedule: &Schedule) -> Result<RunResult, EngineError> {
        check_tag(tag)?;
        self.link.send(HostMsg::Request(Request::Run {
            tag: tag.to_string(),
            schedule: schedule.clone(),
        }))?;
        match self.link.recv()? {
            EngineMsg::Result(r) => Ok(*r),
            m => Err(EngineError::Protocol(self.text(&m))),
        }
    }

    fn branch(&mut self, req: &BranchRequest) -> Result<BranchOutcome, EngineError> {
        if !self.branch {
            return Err(EngineError::Unsupported(
                "BRANCH is not supported on hvm".into(),
            ));
        }
        self.link
            .send(HostMsg::Request(Request::Branch(req.clone())))?;
        let mut out = BranchOutcome::default();
        loop {
            match self.link.recv()? {
                EngineMsg::BranchEnd { note, .. } => {
                    out.end_note = note;
                    return Ok(out);
                }
                EngineMsg::BranchPoint { point, .. } => {
                    out.tjlo = point.map(|p| p.0);
                    out.held = point.map(|p| p.1);
                }
                EngineMsg::Result(r) => out.runs.push(*r),
                _ => {}
            }
        }
    }

    fn lockstep<'a>(
        &'a mut self,
        start: &LockstepStart,
        log: Option<&'a mut dyn Write>,
    ) -> Result<Lockstep<'a>, EngineError> {
        // both engines take it: hydo-hvx (M118) and hvmx (M119)
        check_tag(start.tag())?;
        let mut s = Lockstep::new(&mut *self.link);
        if let Some(l) = log {
            s = s.with_log(l);
        }
        s.request(start.request())?;
        Ok(s)
    }

    fn close(self: Box<Self>) -> Result<(), EngineError> {
        let ProcessWorker { mut proc, link, .. } = *self;
        let mut link = link;
        let _ = link.send(HostMsg::Request(Request::Quit));
        drop(link);
        proc.wait()?;
        Ok(())
    }
}

/// An engine whose run is under lock-step control from its start: `--control one` (a shell image
/// from its S1), and since M121 `--control boot one` (an agent image from its first
/// instruction). `hvx::ControlledOne` and `hvm::ControlledOne` are this type.
pub struct ControlledRun {
    proc: Proc,
    link: Box<dyn Link>,
}

impl ControlledRun {
    pub(crate) fn start(cmd: Command, stderr_log: &Path) -> Result<ControlledRun, EngineError> {
        Ok(ControlledRun::from_started(spawn_piped(cmd, stderr_log)?))
    }

    pub(crate) fn from_started(s: Started) -> ControlledRun {
        ControlledRun {
            proc: s.proc,
            link: s.link,
        }
    }

    /// The session (the run's tag is `one`). `log`: also write the transcript.
    pub fn session<'a>(&'a mut self, log: Option<&'a mut dyn Write>) -> Lockstep<'a> {
        let s = Lockstep::new(&mut *self.link);
        match log {
            Some(l) => s.with_log(l),
            None => s,
        }
    }

    /// Wait for the engine to exit (after the RESULT); its exit status.
    pub fn wait(self) -> Result<Option<i32>, EngineError> {
        let ControlledRun { mut proc, link } = self;
        drop(link);
        Ok(proc.wait()?)
    }
}

/// A `serve` engine booting an agent image under lock-step control (M121, `--control boot
/// serve`; [`crate::Engine::start_controlled`]). [`ControlledBoot::session`] is the boot's
/// lock-step session (tag `boot`, stops `phase=boot`): the host takes the snapshots `S0` and
/// `S1` where it decides and ends the boot with END (RESULT `boot`). [`ControlledBoot::ready`]
/// then reads READY and returns the worker, whose CTL runs restore the host's S1.
pub struct ControlledBoot {
    proc: Proc,
    link: Box<dyn Link>,
    branch: bool,
}

impl ControlledBoot {
    pub(crate) fn start(
        cmd: Command,
        stderr_log: &Path,
        branch: bool,
    ) -> Result<ControlledBoot, EngineError> {
        Ok(ControlledBoot::from_started(
            spawn_piped(cmd, stderr_log)?,
            branch,
        ))
    }

    pub(crate) fn from_started(s: Started, branch: bool) -> ControlledBoot {
        ControlledBoot {
            proc: s.proc,
            link: s.link,
            branch,
        }
    }

    /// The boot's session. `log`: also write the transcript.
    pub fn session<'a>(&'a mut self, log: Option<&'a mut dyn Write>) -> Lockstep<'a> {
        let s = Lockstep::new(&mut *self.link);
        match log {
            Some(l) => s.with_log(l),
            None => s,
        }
    }

    /// After the boot's RESULT: wait for READY and return the worker.
    pub fn ready(self) -> Result<Box<dyn Worker>, EngineError> {
        let ControlledBoot {
            mut proc,
            link,
            branch,
        } = self;
        let (boot_secs, link) = read_ready(&mut proc, link)?;
        Ok(Box::new(ProcessWorker {
            proc,
            link,
            boot_secs,
            branch,
        }))
    }
}
