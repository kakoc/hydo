//! The hvm engine (Apple Silicon, Hypervisor.framework): since M143 the Rust engine `hydo-hvm`
//! (`hydo/crates/hydo-hvm`, the port of `hv/hvm/explore/hvmx`), with the same command lines and
//! protocol; `HYDO_HVM=c` runs the C `hvmx` for one milestone, `HYDO_HVM=inproc` runs the Rust
//! engine on a thread of this process ([`crate::hvm_inproc`]; one VM per process, and this
//! process must carry the hypervisor entitlement). See [`program`].
//!
//! Command lines, as the Mac CLI and `explore.py --backend hvm` (hvm_backend.py) build them:
//!
//! ```text
//! [env HVM_DISK=<data disk>] <hvmx> [--ram-mib N] one   <Image> <initramfs> <outdir> "<schedule>"
//! [env HVM_DISK=<data disk>] <hvmx> [--ram-mib N] serve <Image> <initramfs> <outdir>
//! [env HVM_DISK=<data disk>] <hvmx> [--ram-mib N] --control one <Image> <initramfs> <outdir> "<schedule>"
//! [env HVM_DISK=<data disk>] <hvmx> [--ram-mib N] --control boot serve|one <Image> <initramfs> <outdir> ["<schedule>"]
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
use crate::process::{run_one, ControlledBoot, ControlledRun, ProcessWorker};
use crate::{BootSpec, Engine, EngineError, EngineKind, OneRun, Worker};
use hydo_schedule::Schedule;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A fresh-boot hvmx under lock-step control ([`Hvm::one_lockstep`]).
pub type ControlledOne = ControlledRun;

/// How to start hvmx.
#[derive(Clone, Debug)]
pub struct Hvm {
    /// The engine program ([`program`]: `hydo-hvm`, or the C `hv/hvm/explore/hvmx`), signed with
    /// the hypervisor entitlement (a `hydo-hvm` that is not gets signed before it runs).
    pub hvmx: PathBuf,
    /// Environment for the engine process (on top of this process's); an engine in this process
    /// reads the configuration variables among them on top of `config`.
    pub env: Vec<(String, String)>,
    /// The configuration the program read at its start (`hydo_config::Env::process`, M144):
    /// where the engine runs and, for an engine in this process, its options.
    pub config: hydo_config::Env,
}

impl Hvm {
    /// The release (and repository) layout: the engine [`program`] of `root`.
    pub fn for_release(root: &Path, config: hydo_config::Env) -> Hvm {
        Hvm {
            hvmx: program(root, &hydo_config::hvm::Engine::from_env(&config)),
            env: Vec::new(),
            config,
        }
    }

    /// The configuration an engine in this process sees: `config`, then `env` on top.
    pub(crate) fn engine_env(&self) -> hydo_config::Env {
        self.config
            .with(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
    }

    /// Whether the engine runs in this process (`HYDO_HVM=inproc`).
    pub fn in_process(&self) -> bool {
        hydo_config::hvm::Engine::from_env(&self.engine_env()).choice
            == hydo_config::hvm::Choice::InProcess
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
            c.env(hydo_config::registry::HVM_DISK, d);
        }
        c
    }

    fn check_built(&self) -> Result<(), EngineError> {
        if !crate::hvx::is_executable(&self.hvmx) {
            return Err(EngineError::NotBuilt(self.hvmx.clone()));
        }
        if self.hvmx.file_name().is_some_and(|n| n == "hydo-hvm") {
            ensure_signed(&self.hvmx).map_err(EngineError::Io)?;
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
        if self.in_process() {
            return crate::hvm_inproc::one_lockstep(self, spec, schedule, stderr_log);
        }
        self.check_built()?;
        let argv = self.control_argv(spec, "one", &["--control"], Some(schedule));
        ControlledRun::start(self.command(spec, &argv), stderr_log)
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
        let at = argv.len() - 4; // ... <mode> <Image> <initramfs> <outdir>
        for (i, f) in flags.iter().enumerate() {
            argv.insert(at + i, f.to_string());
        }
        if let Some(s) = last {
            argv.push(s.to_string());
        }
        argv
    }
}

/// The Mac engine program of a tree or a release at `root`: `$HYDO_HVM_ENGINE` when set; with
/// `HYDO_HVM=c` the C engine `hv/hvm/explore/hvmx` (kept for one milestone after M143); else the
/// Rust engine, a tree's `target/release/hydo-hvm` or a release's `bin/hydo-hvm`; else (a release
/// built before M143) the C engine.
pub fn program(root: &Path, e: &hydo_config::hvm::Engine) -> PathBuf {
    if let Some(p) = &e.program {
        return p.clone();
    }
    let c = root.join("hv").join("hvm").join("explore").join("hvmx");
    if e.choice == hydo_config::hvm::Choice::C {
        return c;
    }
    for p in [
        root.join("target").join("release").join("hydo-hvm"),
        root.join("bin").join("hydo-hvm"),
    ] {
        if crate::hvx::is_executable(&p) {
            return p;
        }
    }
    c
}

/// The hypervisor entitlement (`hv/hvm/hv.entitlements`).
pub const ENTITLEMENTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>
"#;

/// Sign `prog` ad hoc with the hypervisor entitlement unless it already has it (cargo builds the
/// program unsigned; Hypervisor.framework refuses a process without it). Checked once per file
/// version in this process.
pub fn ensure_signed(prog: &Path) -> std::io::Result<()> {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static DONE: Mutex<Option<HashSet<(PathBuf, std::time::SystemTime)>>> = Mutex::new(None);
    let mtime = std::fs::metadata(prog)?.modified()?;
    let key = (prog.to_path_buf(), mtime);
    let mut done = DONE.lock().unwrap_or_else(|e| e.into_inner());
    if done.get_or_insert_with(HashSet::new).contains(&key) {
        return Ok(());
    }
    let has = Command::new("codesign")
        .args(["-d", "--entitlements", "-"])
        .arg(prog)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout).contains("com.apple.security.hypervisor")
                || String::from_utf8_lossy(&o.stderr).contains("com.apple.security.hypervisor")
        })
        .unwrap_or(false);
    if !has {
        let plist =
            std::env::temp_dir().join(format!("hydo-hv-{}.entitlements", std::process::id()));
        std::fs::write(&plist, ENTITLEMENTS)?;
        let st = Command::new("codesign")
            .args(["--force", "--entitlements"])
            .arg(&plist)
            .args(["-s", "-"])
            .arg(prog)
            .output();
        let _ = std::fs::remove_file(&plist);
        let st = st?;
        if !st.status.success() {
            return Err(std::io::Error::other(format!(
                "codesign {}: {}",
                prog.display(),
                String::from_utf8_lossy(&st.stderr).trim()
            )));
        }
    }
    let mtime = std::fs::metadata(prog)?.modified()?;
    done.get_or_insert_with(HashSet::new)
        .insert((prog.to_path_buf(), mtime));
    Ok(())
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
        if self.in_process() {
            return crate::hvm_inproc::one(self, spec, schedule);
        }
        self.check_built()?;
        let mut argv = self.argv(spec, "one");
        argv.push(schedule.to_string());
        run_one(self.command(spec, &argv))
    }

    fn start(&self, spec: &BootSpec, stderr_log: &Path) -> Result<Box<dyn Worker>, EngineError> {
        if self.in_process() {
            return crate::hvm_inproc::serve(self, spec, stderr_log);
        }
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

    fn start_controlled(
        &self,
        spec: &BootSpec,
        boot: &Schedule,
        stderr_log: &Path,
    ) -> Result<ControlledBoot, EngineError> {
        if self.in_process() {
            return crate::hvm_inproc::start_controlled(self, spec, boot, stderr_log);
        }
        self.check_built()?;
        let argv = self.control_argv(spec, "serve", &["--control", "boot"], Some(boot));
        ControlledBoot::start(self.command(spec, &argv), stderr_log, false)
    }

    fn one_controlled(
        &self,
        spec: &BootSpec,
        schedule: &Schedule,
        stderr_log: &Path,
    ) -> Result<ControlledRun, EngineError> {
        if self.in_process() {
            return crate::hvm_inproc::one_controlled(self, spec, schedule, stderr_log);
        }
        self.check_built()?;
        let argv = self.control_argv(spec, "one", &["--control", "boot"], Some(schedule));
        ControlledRun::start(self.command(spec, &argv), stderr_log)
    }
}
