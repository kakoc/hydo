//! The hvm engine: `hv/hvm/explore/hvmx` (Apple Silicon, Hypervisor.framework).
//!
//! Command lines, as the Mac CLI and `explore.py --backend hvm` (hvm_backend.py) build them:
//!
//! ```text
//! [env HVM_DISK=<data disk>] <hvmx> [--ram-mib N] one   <Image> <initramfs> <outdir> "<schedule>"
//! [env HVM_DISK=<data disk>] <hvmx> [--ram-mib N] serve <Image> <initramfs> <outdir>
//! [env HVM_DISK=<data disk>] <hvmx> [--ram-mib N] --control one <Image> <initramfs> <outdir> "<schedule>"
//! ```
//!
//! The data disk is `<image without .cpio.gz>-disk.img` next to the initramfs, attached only when
//! it exists. An image built for the second guest kernel names it in `/etc/hydo/kernel`
//! (`<file name> <sha256>`); [`kernel_for`] resolves and checks it.
//!
//! Since M119 hvmx speaks hvx_explore's lock-step protocol ([`crate::lockstep`]): `CTL`/`RESUME`
//! through [`Worker::lockstep`] on a serve worker, and `--control one` through
//! [`Hvm::one_lockstep`]. The clock is the exit count instead of hvx's icount.

use crate::image::{embedded_file, sha256_file, ImageError};
use crate::lockstep::Lockstep;
use crate::process::{run_one, ProcessWorker};
use crate::{BootSpec, Engine, EngineError, EngineKind, OneRun, Worker};
use hydo_schedule::Schedule;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// How to start hvmx.
#[derive(Clone, Debug)]
pub struct Hvm {
    /// The engine binary (`hv/hvm/explore/hvmx`, signed with the hypervisor entitlement).
    pub hvmx: PathBuf,
    /// Environment for the engine process.
    pub env: Vec<(String, String)>,
}

impl Hvm {
    /// The release (and repository) layout: `<root>/hv/hvm/explore/hvmx`.
    pub fn for_release(root: &Path) -> Hvm {
        Hvm {
            hvmx: root.join("hv").join("hvm").join("explore").join("hvmx"),
            env: Vec::new(),
        }
    }

    pub fn argv(&self, spec: &BootSpec, mode: &str) -> Vec<String> {
        let mut a = vec![self.hvmx.display().to_string()];
        a.extend(spec.engine_args(EngineKind::Hvm));
        a.push(mode.into());
        a.push(spec.kernel.display().to_string());
        a.push(spec.image.display().to_string());
        a.push(spec.outdir.display().to_string());
        a
    }

    fn command(&self, spec: &BootSpec, argv: &[String]) -> Command {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        for (k, v) in &self.env {
            c.env(k, v);
        }
        if let Some(d) = disk_for(&spec.image) {
            c.env("HVM_DISK", d);
        }
        c
    }

    fn check_built(&self) -> Result<(), EngineError> {
        if !crate::hvx::is_executable(&self.hvmx) {
            return Err(EngineError::NotBuilt(self.hvmx.clone()));
        }
        Ok(())
    }

    /// One schedule from a fresh boot under lock-step control (`hvmx --control one`): the
    /// fresh-boot counterpart of [`Worker::lockstep`], to prove that a controlled run from a fresh
    /// boot equals the same run restored from S1 (as `hvx::Hvx::one_lockstep` on hvx). The
    /// engine's stderr goes to `stderr_log`.
    pub fn one_lockstep(
        &self,
        spec: &BootSpec,
        schedule: &Schedule,
        stderr_log: &Path,
    ) -> Result<ControlledOne, EngineError> {
        self.check_built()?;
        let mut argv = self.argv(spec, "one");
        let mode = argv.len() - 4; // ... one <Image> <initramfs> <outdir>
        argv.insert(mode, "--control".into());
        argv.push(schedule.to_string());
        let err = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(stderr_log)?;
        let mut cmd = self.command(spec, &argv);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(err);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        Ok(ControlledOne {
            child,
            stdin,
            stdout,
        })
    }
}

/// A fresh-boot hvmx under lock-step control ([`Hvm::one_lockstep`]).
pub struct ControlledOne {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl ControlledOne {
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
        Ok(self.child.wait()?.code())
    }
}

/// The data disk built next to an arm64 initramfs (`X-arm64.cpio.gz` -> `X-arm64-disk.img`), if it exists.
pub fn disk_for(image: &Path) -> Option<PathBuf> {
    let s = image.to_str()?;
    let d = PathBuf::from(format!("{}-disk.img", s.strip_suffix(".cpio.gz")?));
    d.exists().then_some(d)
}

/// (file name, sha256) of the kernel an image was built for (`/etc/hydo/kernel`), or None.
pub fn image_kernel(image: &Path) -> Option<(String, String)> {
    let text = embedded_file(image, "etc/hydo/kernel").ok()?;
    let mut w = text.split_whitespace();
    Some((w.next()?.to_string(), w.next()?.to_string()))
}

/// The kernel file to boot `image` with (hvm_backend.kernel_for): `kernel` when given, else the
/// image's recorded kernel next to `default`, else `default`. A recorded kernel is checked by
/// sha256, so a run never boots a kernel the image was not built for. Err: the message
/// hvm_backend raises (SystemExit text).
pub fn kernel_for(image: &Path, kernel: Option<&Path>, default: &Path) -> Result<PathBuf, String> {
    let Some((name, sha)) = image_kernel(image) else {
        return Ok(kernel.unwrap_or(default).to_path_buf());
    };
    let path = match kernel {
        Some(k) => k.to_path_buf(),
        None => default.parent().unwrap_or(Path::new("")).join(&name),
    };
    let short = &sha[..sha.len().min(16)];
    if !path.is_file() {
        return Err(format!(
            "{} was built for the guest kernel {} (sha256 {}...) and it is not at {} \
             (hv/hvm/explore/build-guest-netns.sh builds it)",
            image.display(),
            name,
            short,
            path.display()
        ));
    }
    let got = sha256_file(&path).map_err(|e| e.to_string())?;
    if got != sha {
        return Err(format!(
            "{} was built for the guest kernel {} (sha256 {}...), but {} has sha256 {}...",
            image.display(),
            name,
            short,
            path.display(),
            &got[..16]
        ));
    }
    Ok(path)
}

/// True when the image's compiled manifest names a tc binary (hvm_backend.image_has_tc).
pub fn image_has_tc(image: &Path) -> Result<bool, ImageError> {
    let text = embedded_file(image, "etc/hydo/manifest.sh")?;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("HYDO_TC=") {
            return Ok(!v.trim_matches(|c| c == '\'' || c == '"').is_empty());
        }
    }
    Ok(false)
}

impl Engine for Hvm {
    fn kind(&self) -> EngineKind {
        EngineKind::Hvm
    }

    fn one(&self, spec: &BootSpec, schedule: &Schedule) -> Result<OneRun, EngineError> {
        self.check_built()?;
        let mut argv = self.argv(spec, "one");
        argv.push(schedule.to_string());
        run_one(self.command(spec, &argv))
    }

    fn start(&self, spec: &BootSpec, stderr_log: &Path) -> Result<Box<dyn Worker>, EngineError> {
        self.check_built()?;
        let argv = self.argv(spec, "serve");
        Ok(Box::new(ProcessWorker::start(
            self.command(spec, &argv),
            stderr_log,
            false,
        )?))
    }

    fn repro_line(&self, spec: &BootSpec, schedule: &Schedule) -> String {
        let env = disk_for(&spec.image)
            .map(|d| format!("HVM_DISK={} ", d.display()))
            .unwrap_or_default();
        let mut argv = self.argv(spec, "one");
        let last = argv.len() - 1;
        argv[last] = "<outdir>".into();
        format!("{env}{} \"{schedule}\"", argv.join(" "))
    }
}
