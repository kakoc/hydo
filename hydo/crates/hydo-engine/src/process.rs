//! A started engine and the sessions and workers over it: a fresh-boot controlled run
//! ([`ControlledRun`]), a controlled boot ([`ControlledBoot`]) and the `serve` worker it becomes,
//! each over a [`Link`] (an engine program's stdin and stdout, or an engine thread of this
//! process, [`crate::inproc`]).

use crate::link::{Link, TextLink};
use crate::lockstep::{Lockstep, LockstepStart};
use crate::protocol::{EngineMsg, HostMsg, Request};
use crate::{ControlledWorker, EngineError};
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

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

/// A `serve` engine after its controlled boot: READY, then one controlled run per CTL/RESUME.
pub(crate) struct ProcessWorker {
    proc: Proc,
    link: Box<dyn Link>,
    boot_secs: f64,
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

fn check_tag(tag: &str) -> Result<(), EngineError> {
    if tag.is_empty() || tag.contains(char::is_whitespace) || tag.len() > 63 {
        return Err(EngineError::Protocol(format!(
            "bad tag {tag:?} (one word, at most 63 bytes)"
        )));
    }
    Ok(())
}

impl ControlledWorker for ProcessWorker {
    fn boot_secs(&self) -> f64 {
        self.boot_secs
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

/// An engine whose run is under lock-step control from its first instruction: `--control boot
/// one` (an agent image, M121; [`crate::ControlledEngine::one_controlled`]).
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
}

impl ControlledBoot {
    pub(crate) fn start(cmd: Command, stderr_log: &Path) -> Result<ControlledBoot, EngineError> {
        Ok(ControlledBoot::from_started(spawn_piped(cmd, stderr_log)?))
    }

    pub(crate) fn from_started(s: Started) -> ControlledBoot {
        ControlledBoot {
            proc: s.proc,
            link: s.link,
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
    pub fn ready(self) -> Result<Box<dyn ControlledWorker>, EngineError> {
        let ControlledBoot { mut proc, link } = self;
        let (boot_secs, link) = read_ready(&mut proc, link)?;
        Ok(Box::new(ProcessWorker {
            proc,
            link,
            boot_secs,
        }))
    }
}
