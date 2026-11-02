//! The hvx engine (x86_64 Linux, VT-x, `/dev/hvx`): the Rust engine `hydo-hvx` (M132; the C
//! `hvx_explore` it replaced was deleted in M137), run in this process or as a program.
//!
//! Where it runs ([`Hvx::in_process`]): on a thread of this process ([`crate::inproc`]) when this
//! process can open the device (root, or a user in the udev rule's group), otherwise as the
//! program `hydo-hvx` under the shared-lock wrapper with sudo. `HYDO_ENGINE=inproc` or
//! `HYDO_ENGINE=process` forces one of the two; `HYDO_SUDO=1` keeps the program with sudo (the
//! group `hydo_config::engine::Engine`, read from [`Hvx::config`] with the overrides). The
//! values, the transcript lines and every result are the same either way (M142: in this process the
//! calls are Rust values over channels, [`crate::link::ChanLink`]; to the program they are its lines).
//!
//! The program's command lines (the same as the C engine's had):
//!
//! ```text
//! <hvx-run> sudo env [<extra env>...] <hydo-hvx> [--ram-mib N] [--vcpus N] [--cpu v3]
//!     one   <bzImage> <initramfs> <image> <outdir> "<schedule>"
//!     serve <bzImage> <initramfs> <image> <outdir>          (then RUN/BRANCH/QUIT on stdin)
//!     --control one   ... "<schedule>"                       (lock-step control from S1, M118)
//!     --control boot serve ... ["<boot schedule>"]           (an agent image, M121)
//!     --control boot one   ... "<schedule>"
//! ```
//!
//! `hvx-run` takes the shared module lock (`flock -s $HYDO_LOCK`) and drops the `sudo` when the
//! user can open the device; without a wrapper the program is started directly. In this process
//! the engine thread takes the same shared lock itself ([`crate::inproc`]).

use crate::process::{run_one, ControlledBoot, ControlledRun, ProcessWorker};
use crate::{BootSpec, Engine, EngineError, EngineKind, OneRun, Worker};
use hydo_schedule::Schedule;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A fresh-boot engine process under lock-step control ([`Hvx::one_lockstep`]).
pub type ControlledOne = ControlledRun;

/// How to start the hvx engine.
#[derive(Clone, Debug)]
pub struct Hvx {
    /// The engine program (`hydo-hvx`; see [`program`]), used when the engine does not run in
    /// this process.
    pub engine: PathBuf,
    /// The shared-lock wrapper (`libexec/hvx-run`, or the box's `~/bin/hvx-run`); None: run the
    /// engine directly (as root, or as a user who can open the device).
    pub hvx_run: Option<PathBuf>,
    /// Extra `KEY=VALUE` words put after `sudo env` (explore.py's EXTRA_ENV); an engine in this
    /// process reads them on top of `env`.
    pub extra_env: Vec<String>,
    /// Environment for the wrapper process itself (HYDO_LOCK, HVX_DEV, PATH ...); an engine in
    /// this process reads the configuration variables among them on top of `config`.
    pub env: Vec<(String, String)>,
    /// The configuration the program read at its start (`hydo_config::Env::process`, M144):
    /// where the engine runs and, for an engine in this process, its options.
    pub config: hydo_config::Env,
}

impl Hvx {
    /// The release layout: `<root>/bin/hydo-hvx` and `<root>/libexec/hvx-run`.
    pub fn for_release(root: &Path, config: hydo_config::Env) -> Hvx {
        Hvx {
            engine: root.join("bin").join("hydo-hvx"),
            hvx_run: Some(root.join("libexec").join("hvx-run")),
            extra_env: Vec::new(),
            env: Vec::new(),
            config,
        }
    }

    /// The argv for `mode` (one or serve), without the schedule.
    pub fn argv(&self, spec: &BootSpec, mode: &str) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        if let Some(w) = &self.hvx_run {
            a.push(w.display().to_string());
            a.push("sudo".into());
            a.push("env".into());
            a.extend(self.extra_env.iter().cloned());
        }
        a.push(self.engine.display().to_string());
        a.extend(spec.engine_args(EngineKind::Hvx));
        a.push(mode.into());
        a.push(spec.kernel.display().to_string());
        a.push(
            spec.initrd
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        a.push(spec.image.display().to_string());
        a.push(spec.outdir.display().to_string());
        a
    }

    fn command(&self, argv: &[String]) -> Command {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }

    /// One schedule from a fresh boot under lock-step control (`hvx_explore --control one`): the
    /// fresh-boot counterpart of [`Worker::lockstep`], to prove that a controlled run from a fresh
    /// boot equals the same run restored from S1. The engine's stderr goes to `stderr_log`.
    pub fn one_lockstep(
        &self,
        spec: &BootSpec,
        schedule: &Schedule,
        stderr_log: &Path,
    ) -> Result<ControlledOne, EngineError> {
        if crate::inproc::chosen(self) {
            return crate::inproc::one_lockstep(self, spec, schedule, stderr_log);
        }
        self.check_built()?;
        let argv = self.control_argv(spec, "one", &["--control"], Some(schedule));
        ControlledRun::start(self.command(&argv), stderr_log)
    }

    /// argv for a controlled mode: `flags` go in front of the mode, `last` after the paths.
    fn control_argv(
        &self,
        spec: &BootSpec,
        mode: &str,
        flags: &[&str],
        last: Option<&Schedule>,
    ) -> Vec<String> {
        let mut argv = self.argv(spec, mode);
        let at = argv.len() - 5; // ... <mode> <kernel> <initrd> <image> <outdir>
        for (i, f) in flags.iter().enumerate() {
            argv.insert(at + i, f.to_string());
        }
        if let Some(s) = last {
            argv.push(s.to_string());
        }
        argv
    }

    /// Whether the engine runs on a thread of this process (see the module comment).
    pub fn in_process(&self) -> bool {
        crate::inproc::chosen(self)
    }

    /// The engine program is needed (it does not run in this process) and is not there.
    pub fn missing_program(&self) -> Option<&Path> {
        (!self.in_process() && !is_executable(&self.engine)).then_some(self.engine.as_path())
    }

    fn check_built(&self) -> Result<(), EngineError> {
        if !is_executable(&self.engine) {
            return Err(EngineError::NotBuilt(self.engine.clone()));
        }
        Ok(())
    }
}

/// The environment the shared-lock wrapper and the engine program get from the configuration
/// ([`Hvx::env`]): the module lock (`HYDO_LOCK`), the device (`HVX_DEV`) and `HYDO_SUDO=1`, each
/// when configured. They are what `libexec/hvx-run` and `hydo-hvx` read; a value the process's
/// environment had reaches them anyway, and one the `hydo` command chose (the side module's lock
/// file) reaches them only this way.
pub fn wrapper_env(e: &hydo_config::engine::Engine) -> Vec<(String, String)> {
    use hydo_config::registry::{HVX_DEV, HYDO_LOCK, HYDO_SUDO};
    let mut v = Vec::new();
    if let Some(l) = &e.lock {
        v.push((HYDO_LOCK.to_string(), l.display().to_string()));
    }
    if let Some(d) = &e.dev {
        v.push((HVX_DEV.to_string(), d.clone()));
    }
    if e.sudo {
        v.push((HYDO_SUDO.to_string(), "1".to_string()));
    }
    v
}

/// The engine program for a release or a tree at `root`: the configured one (`HYDO_HVX_EXPLORE`,
/// a name from the C engine; it names any program with that command line), else
/// `<root>/target/release/hydo-hvx` when a tree has built it, else `<root>/bin/hydo-hvx` (a
/// release).
pub fn program(root: &Path, engine: &hydo_config::engine::Engine) -> PathBuf {
    if let Some(p) = &engine.program {
        return p.clone();
    }
    let tree = root.join("target").join("release").join("hydo-hvx");
    if is_executable(&tree) {
        return tree;
    }
    root.join("bin").join("hydo-hvx")
}

pub(crate) fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

impl Engine for Hvx {
    fn kind(&self) -> EngineKind {
        EngineKind::Hvx
    }

    fn one(&self, spec: &BootSpec, schedule: &Schedule) -> Result<OneRun, EngineError> {
        if crate::inproc::chosen(self) {
            return crate::inproc::one(self, spec, schedule);
        }
        self.check_built()?;
        let mut argv = self.argv(spec, "one");
        argv.push(schedule.to_string());
        run_one(self.command(&argv))
    }

    fn start(&self, spec: &BootSpec, stderr_log: &Path) -> Result<Box<dyn Worker>, EngineError> {
        if crate::inproc::chosen(self) {
            return crate::inproc::serve(self, spec, stderr_log);
        }
        self.check_built()?;
        let argv = self.argv(spec, "serve");
        Ok(Box::new(ProcessWorker::start(
            self.command(&argv),
            stderr_log,
            true,
        )?))
    }

    fn repro_line(&self, spec: &BootSpec, schedule: &Schedule) -> String {
        let mut argv = self.argv(spec, "one");
        argv.push(format!("\"{schedule}\""));
        argv.join(" ")
    }

    fn start_controlled(
        &self,
        spec: &BootSpec,
        boot: &Schedule,
        stderr_log: &Path,
    ) -> Result<ControlledBoot, EngineError> {
        if crate::inproc::chosen(self) {
            return crate::inproc::start_controlled(self, spec, boot, stderr_log);
        }
        self.check_built()?;
        let argv = self.control_argv(spec, "serve", &["--control", "boot"], Some(boot));
        ControlledBoot::start(self.command(&argv), stderr_log, false)
    }

    fn one_controlled(
        &self,
        spec: &BootSpec,
        schedule: &Schedule,
        stderr_log: &Path,
    ) -> Result<ControlledRun, EngineError> {
        if crate::inproc::chosen(self) {
            return crate::inproc::one_controlled(self, spec, schedule, stderr_log);
        }
        self.check_built()?;
        let argv = self.control_argv(spec, "one", &["--control", "boot"], Some(schedule));
        ControlledRun::start(self.command(&argv), stderr_log)
    }
}
