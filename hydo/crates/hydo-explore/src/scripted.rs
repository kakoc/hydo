//! A scripted engine for the explorer's own tests (`tests/fake_engine.rs`, M145): a program whose
//! every answer is a pure function of the schedule text stands in for the host controller and a
//! real engine, so the explorer's whole flow (generators, re-runs, shrinking, sweeps, reports) is
//! checked without a VM. It is chosen with `HYDO_SCRIPTED_ENGINE=<program>`
//! (`hydo_config::run::Pool::scripted_engine`) and is not an engine protocol: real runs always go
//! through `hydo-control`'s `AgentEngine` over a controlled engine.
//!
//! The program's command lines and lines:
//!
//! ```text
//! <program> serve <consoles dir>             prints READY boot_secs=<s>, then for every line
//!                                            `<tag> <schedule>` one RESULT line; ends at EOF
//! <program> one <consoles dir> "<schedule>"  prints one RESULT line (tag `one`)
//! ```
//!
//! It writes the consoles a run leaves (`<consoles dir>/<tag>.log`, ...) itself.

use hydo_engine::{BootSpec, Engine, EngineError, EngineKind, OneRun, Ready, RunResult, Worker};
use hydo_schedule::Schedule;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

/// The scripted engine: `program`, as the backend `kind`.
#[derive(Clone, Debug)]
pub struct ScriptedEngine {
    pub program: PathBuf,
    pub kind: EngineKind,
}

impl Engine for ScriptedEngine {
    fn kind(&self) -> EngineKind {
        self.kind
    }

    fn one(&self, spec: &BootSpec, schedule: &Schedule) -> Result<OneRun, EngineError> {
        let t0 = Instant::now();
        let o = Command::new(&self.program)
            .arg("one")
            .arg(&spec.outdir)
            .arg(schedule.to_string())
            .stderr(Stdio::inherit())
            .output()?;
        let output = String::from_utf8_lossy(&o.stdout).into_owned();
        Ok(OneRun {
            result: output.lines().find_map(RunResult::parse),
            output,
            exit_code: o.status.code(),
            wall: t0.elapsed(),
        })
    }

    fn start(&self, spec: &BootSpec, stderr_log: &Path) -> Result<Box<dyn Worker>, EngineError> {
        let err = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(stderr_log)?;
        let mut child = Command::new(&self.program)
            .arg("serve")
            .arg(&spec.outdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(err)
            .spawn()?;
        let input = child.stdin.take().expect("piped stdin");
        let mut out = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut line = String::new();
        out.read_line(&mut line)?;
        let Some(r) = Ready::parse(&line) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngineError::NotReady(line));
        };
        Ok(Box::new(ScriptedWorker {
            child,
            input: Some(input),
            out,
            boot_secs: r.boot_secs,
        }))
    }

    fn repro_line(&self, spec: &BootSpec, schedule: &Schedule) -> String {
        format!("hydo run --image {} \"{schedule}\"", spec.image.display())
    }
}

struct ScriptedWorker {
    child: Child,
    input: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    boot_secs: f64,
}

impl Worker for ScriptedWorker {
    fn boot_secs(&self) -> f64 {
        self.boot_secs
    }

    fn run(&mut self, tag: &str, schedule: &Schedule) -> Result<RunResult, EngineError> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| EngineError::Protocol("the engine exited".into()))?;
        writeln!(input, "{tag} {schedule}")?;
        input.flush()?;
        let mut line = String::new();
        if self.out.read_line(&mut line)? == 0 {
            return Err(EngineError::Protocol("the engine exited".into()));
        }
        RunResult::parse(&line).ok_or(EngineError::Protocol(line))
    }

    fn close(mut self: Box<Self>) -> Result<(), EngineError> {
        drop(self.input.take());
        self.child.wait()?;
        Ok(())
    }
}
