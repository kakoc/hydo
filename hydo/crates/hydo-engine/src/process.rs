//! Driving an engine program: a fresh-boot `one` run, and a `serve` worker over stdin/stdout.

use crate::lockstep::{Lockstep, LockstepStart};
use crate::protocol::{BranchOutcome, BranchRequest, Ready, RunResult};
use crate::{EngineError, OneRun, Worker};
use hydo_schedule::Schedule;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// The engine's stdin and stdout as this side sees them: a child process's pipes, or the pipes of
/// an engine running on a thread of this process (the in-process engine, [`crate::inproc`]).
pub(crate) type Stdin = Box<dyn Write + Send>;
pub(crate) type Stdout = BufReader<Box<dyn Read + Send>>;

/// What runs the engine: a child process, or a thread of this process (its exit status is the
/// thread's return value).
pub(crate) enum Proc {
    Child(Child),
    Thread(Option<std::thread::JoinHandle<i32>>),
}

impl Proc {
    fn kill(&mut self) {
        // a thread cannot be killed; it ends when its stdin closes (the caller drops it)
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

/// An engine with piped stdin and stdout.
pub(crate) struct Piped {
    pub(crate) proc: Proc,
    pub(crate) stdin: Stdin,
    pub(crate) stdout: Stdout,
}
use std::time::Instant;

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

/// A `serve` engine process: READY, then one RESULT per RUN.
pub(crate) struct ProcessWorker {
    child: Proc,
    stdin: Stdin,
    stdout: Stdout,
    boot_secs: f64,
    branch: bool,
}

/// Start `cmd` with stdin/stdout piped and stderr appended to `stderr_log`.
fn spawn_piped(mut cmd: Command, stderr_log: &Path) -> Result<Piped, EngineError> {
    let err = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(stderr_log)?;
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(err);
    let mut child = cmd.spawn()?;
    let stdin: Stdin = Box::new(child.stdin.take().expect("piped stdin"));
    let out: Box<dyn Read + Send> = Box::new(child.stdout.take().expect("piped stdout"));
    Ok(Piped {
        proc: Proc::Child(child),
        stdin,
        stdout: BufReader::new(out),
    })
}

/// Read the READY line of a serve process (or kill it and say what it printed instead).
fn read_ready(child: &mut Proc, stdout: &mut Stdout) -> Result<f64, EngineError> {
    let mut line = String::new();
    stdout.read_line(&mut line)?;
    match Ready::parse(&line) {
        Some(ready) => Ok(ready.boot_secs),
        None => {
            child.kill();
            let _ = child.wait();
            Err(EngineError::NotReady(line))
        }
    }
}

impl ProcessWorker {
    /// Start `cmd` with stdin/stdout piped and stderr appended to `stderr_log`, and wait for READY.
    pub(crate) fn start(
        cmd: Command,
        stderr_log: &Path,
        branch: bool,
    ) -> Result<ProcessWorker, EngineError> {
        ProcessWorker::from_piped(spawn_piped(cmd, stderr_log)?, branch)
    }

    /// A serve engine already started (a process or a thread): wait for READY.
    pub(crate) fn from_piped(p: Piped, branch: bool) -> Result<ProcessWorker, EngineError> {
        let Piped {
            proc: mut child,
            stdin,
            mut stdout,
        } = p;
        let boot_secs = read_ready(&mut child, &mut stdout)?;
        Ok(ProcessWorker {
            child,
            stdin,
            stdout,
            boot_secs,
            branch,
        })
    }

    fn send(&mut self, line: &str) -> Result<(), EngineError> {
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_line(&mut self) -> Result<String, EngineError> {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line)?;
        if n == 0 {
            return Err(EngineError::Protocol("the engine exited".into()));
        }
        Ok(line)
    }
}

impl Worker for ProcessWorker {
    fn boot_secs(&self) -> f64 {
        self.boot_secs
    }

    fn run(&mut self, tag: &str, schedule: &Schedule) -> Result<RunResult, EngineError> {
        if tag.is_empty() || tag.contains(char::is_whitespace) || tag.len() > 63 {
            return Err(EngineError::Protocol(format!(
                "bad tag {tag:?} (one word, at most 63 bytes)"
            )));
        }
        self.send(&format!("RUN {tag} {schedule}"))?;
        let line = self.read_line()?;
        RunResult::parse(&line).ok_or(EngineError::Protocol(line))
    }

    fn branch(&mut self, req: &BranchRequest) -> Result<BranchOutcome, EngineError> {
        if !self.branch {
            return Err(EngineError::Unsupported(
                "BRANCH is not supported on hvm".into(),
            ));
        }
        self.send(&req.line())?;
        let mut out = BranchOutcome::default();
        loop {
            let line = self.read_line()?;
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("BRANCHEND") {
                let rest = rest.trim();
                out.end_note = rest
                    .split_once(' ')
                    .map(|(_, n)| n.to_string())
                    .unwrap_or_default();
                return Ok(out);
            }
            if let Some(rest) = l.strip_prefix("BRANCHPOINT ") {
                for w in rest.split(' ') {
                    if let Some(v) = w.strip_prefix("tjlo=") {
                        out.tjlo = v.parse().ok();
                    } else if let Some(v) = w.strip_prefix("held=") {
                        out.held = v.parse().ok();
                    }
                }
                continue;
            }
            if let Some(r) = RunResult::parse(l) {
                out.runs.push(r);
            }
        }
    }

    fn lockstep<'a>(
        &'a mut self,
        start: &LockstepStart,
        log: Option<&'a mut dyn Write>,
    ) -> Result<Lockstep<'a>, EngineError> {
        // both engines speak the protocol: hvx_explore (M118) and hvmx (M119)
        let tag = start.tag();
        if tag.is_empty() || tag.contains(char::is_whitespace) || tag.len() > 63 {
            return Err(EngineError::Protocol(format!(
                "bad tag {tag:?} (one word, at most 63 bytes)"
            )));
        }
        let mut s = Lockstep::new(&mut self.stdout, &mut self.stdin);
        if let Some(l) = log {
            s = s.with_log(l);
        }
        s.request(&start.line())?;
        Ok(s)
    }

    fn close(self: Box<Self>) -> Result<(), EngineError> {
        let mut this = *self;
        let _ = this.send("QUIT");
        drop(this.stdin);
        this.child.wait()?;
        Ok(())
    }
}

/// An engine process whose run is under lock-step control from its start: `--control one` (a
/// shell image from its S1), and since M121 `--control boot one` (an agent image from its first
/// instruction). `hvx::ControlledOne` and `hvm::ControlledOne` are this type.
pub struct ControlledRun {
    child: Proc,
    stdin: Stdin,
    stdout: Stdout,
}

impl ControlledRun {
    pub(crate) fn start(cmd: Command, stderr_log: &Path) -> Result<ControlledRun, EngineError> {
        Ok(ControlledRun::from_piped(spawn_piped(cmd, stderr_log)?))
    }

    pub(crate) fn from_piped(p: Piped) -> ControlledRun {
        ControlledRun {
            child: p.proc,
            stdin: p.stdin,
            stdout: p.stdout,
        }
    }

    /// The session (the run's tag is `one`). `log`: also write the transcript.
    pub fn session<'a>(&'a mut self, log: Option<&'a mut dyn Write>) -> Lockstep<'a> {
        let s = Lockstep::new(&mut self.stdout, &mut self.stdin);
        match log {
            Some(l) => s.with_log(l),
            None => s,
        }
    }

    /// Wait for the engine to exit (after the RESULT); its exit status.
    pub fn wait(mut self) -> Result<Option<i32>, EngineError> {
        drop(self.stdin);
        Ok(self.child.wait()?)
    }
}

/// A `serve` engine booting an agent image under lock-step control (M121, `--control boot
/// serve`; [`crate::Engine::start_controlled`]). [`ControlledBoot::session`] is the boot's
/// lock-step session (tag `boot`, stops `phase=boot`): the host takes the snapshots `S0` and
/// `S1` where it decides and ends the boot with END (RESULT `boot`). [`ControlledBoot::ready`]
/// then reads READY and returns the worker, whose CTL runs restore the host's S1.
pub struct ControlledBoot {
    child: Proc,
    stdin: Stdin,
    stdout: Stdout,
    branch: bool,
}

impl ControlledBoot {
    pub(crate) fn start(
        cmd: Command,
        stderr_log: &Path,
        branch: bool,
    ) -> Result<ControlledBoot, EngineError> {
        Ok(ControlledBoot::from_piped(
            spawn_piped(cmd, stderr_log)?,
            branch,
        ))
    }

    pub(crate) fn from_piped(p: Piped, branch: bool) -> ControlledBoot {
        ControlledBoot {
            child: p.proc,
            stdin: p.stdin,
            stdout: p.stdout,
            branch,
        }
    }

    /// The boot's session. `log`: also write the transcript.
    pub fn session<'a>(&'a mut self, log: Option<&'a mut dyn Write>) -> Lockstep<'a> {
        let s = Lockstep::new(&mut self.stdout, &mut self.stdin);
        match log {
            Some(l) => s.with_log(l),
            None => s,
        }
    }

    /// After the boot's RESULT: wait for READY and return the worker.
    pub fn ready(self) -> Result<Box<dyn Worker>, EngineError> {
        let ControlledBoot {
            mut child,
            stdin,
            mut stdout,
            branch,
        } = self;
        let boot_secs = read_ready(&mut child, &mut stdout)?;
        Ok(Box::new(ProcessWorker {
            child,
            stdin,
            stdout,
            boot_secs,
            branch,
        }))
    }
}
