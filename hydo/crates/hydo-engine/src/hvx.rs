//! The hvx engine: `hv/hvx/hvx_explore` (x86_64 Linux, VT-x, `/dev/hvx`).
//!
//! Command lines, as the Linux CLI and explore.py build them:
//!
//! ```text
//! <hvx-run> sudo env HVX_SNAP_DIR=<dir> [<extra env>...] <hvx_explore> [--ram-mib N] [--vcpus N] [--cpu v3]
//!     one   <bzImage> <initramfs> <image> <outdir> "<schedule>"
//!     serve <bzImage> <initramfs> <image> <outdir>          (then RUN/BRANCH/QUIT on stdin)
//! ```
//!
//! `hvx-run` takes the shared module lock (`flock -s $HYDO_LOCK`) and drops the `sudo` when the
//! user can open the device; without a wrapper the engine is started directly.

use crate::process::{run_one, ProcessWorker};
use crate::{BootSpec, Engine, EngineError, EngineKind, OneRun, Worker};
use hydo_schedule::Schedule;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How to start hvx_explore.
#[derive(Clone, Debug)]
pub struct Hvx {
    /// The engine binary (`hv/hvx/hvx_explore`).
    pub engine: PathBuf,
    /// The shared-lock wrapper (`libexec/hvx-run`, or the box's `~/bin/hvx-run`); None: run the
    /// engine directly (as root, or as a user who can open the device).
    pub hvx_run: Option<PathBuf>,
    /// `HVX_SNAP_DIR`: where the engine keeps its snapshot files (no effect on any result).
    pub snap_dir: String,
    /// Extra `KEY=VALUE` words put after `HVX_SNAP_DIR=` (explore.py's EXTRA_ENV).
    pub extra_env: Vec<String>,
    /// Environment for the wrapper process itself (HYDO_LOCK, HVX_DEV, PATH ...).
    pub env: Vec<(String, String)>,
}

impl Hvx {
    /// The release layout: `<root>/hv/hvx/hvx_explore` and `<root>/libexec/hvx-run`.
    pub fn for_release(root: &Path) -> Hvx {
        Hvx {
            engine: root.join("hv").join("hvx").join("hvx_explore"),
            hvx_run: Some(root.join("libexec").join("hvx-run")),
            snap_dir: "/var/tmp/hvx-snap".into(),
            extra_env: Vec::new(),
            env: Vec::new(),
        }
    }

    /// The argv for `mode` (one or serve), without the schedule.
    pub fn argv(&self, spec: &BootSpec, mode: &str) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        if let Some(w) = &self.hvx_run {
            a.push(w.display().to_string());
            a.push("sudo".into());
            a.push("env".into());
            a.push(format!("HVX_SNAP_DIR={}", self.snap_dir));
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
        if self.hvx_run.is_none() {
            c.env("HVX_SNAP_DIR", &self.snap_dir);
        }
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }

    fn check_built(&self) -> Result<(), EngineError> {
        if !is_executable(&self.engine) {
            return Err(EngineError::NotBuilt(self.engine.clone()));
        }
        Ok(())
    }
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
        self.check_built()?;
        let mut argv = self.argv(spec, "one");
        argv.push(schedule.to_string());
        run_one(self.command(&argv))
    }

    fn start(&self, spec: &BootSpec, stderr_log: &Path) -> Result<Box<dyn Worker>, EngineError> {
        self.check_built()?;
        let argv = self.argv(spec, "serve");
        Ok(Box::new(ProcessWorker::start(self.command(&argv), stderr_log, true)?))
    }

    fn repro_line(&self, spec: &BootSpec, schedule: &Schedule) -> String {
        let mut argv = self.argv(spec, "one");
        argv.push(format!("\"{schedule}\""));
        argv.join(" ")
    }
}
