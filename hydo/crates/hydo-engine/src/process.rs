//! Driving an engine program: a fresh-boot `one` run, and a `serve` worker over stdin/stdout.

use crate::protocol::{BranchOutcome, BranchRequest, Ready, RunResult};
use crate::{EngineError, OneRun, Worker};
use hydo_schedule::Schedule;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

/// Run `cmd` to completion with stdout and stderr written into ONE pipe (so the text keeps the
/// order in which the engine wrote it, as Python's `stderr=STDOUT` does) and parse its RESULT.
pub(crate) fn run_one(mut cmd: Command) -> Result<OneRun, EngineError> {
    let t0 = Instant::now();
    let (mut reader, writer) = std::io::pipe()?;
    cmd.stdin(Stdio::null()).stdout(writer.try_clone()?).stderr(writer);
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
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    boot_secs: f64,
    branch: bool,
}

impl ProcessWorker {
    /// Start `cmd` with stdin/stdout piped and stderr appended to `stderr_log`, and wait for READY.
    pub(crate) fn start(mut cmd: Command, stderr_log: &Path, branch: bool) -> Result<ProcessWorker, EngineError> {
        let err = std::fs::OpenOptions::new().create(true).append(true).open(stderr_log)?;
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(err);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut line = String::new();
        stdout.read_line(&mut line)?;
        let Some(ready) = Ready::parse(&line) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngineError::NotReady(line));
        };
        Ok(ProcessWorker {
            child,
            stdin,
            stdout,
            boot_secs: ready.boot_secs,
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
            return Err(EngineError::Protocol(format!("bad tag {tag:?} (one word, at most 63 bytes)")));
        }
        self.send(&format!("RUN {tag} {schedule}"))?;
        let line = self.read_line()?;
        RunResult::parse(&line).ok_or(EngineError::Protocol(line))
    }

    fn branch(&mut self, req: &BranchRequest) -> Result<BranchOutcome, EngineError> {
        if !self.branch {
            return Err(EngineError::Unsupported("BRANCH is not supported on hvm".into()));
        }
        self.send(&req.line())?;
        let mut out = BranchOutcome::default();
        loop {
            let line = self.read_line()?;
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("BRANCHEND") {
                let rest = rest.trim();
                out.end_note = rest.split_once(' ').map(|(_, n)| n.to_string()).unwrap_or_default();
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

    fn close(self: Box<Self>) -> Result<(), EngineError> {
        let mut this = *self;
        let _ = this.send("QUIT");
        drop(this.stdin);
        this.child.wait()?;
        Ok(())
    }
}
