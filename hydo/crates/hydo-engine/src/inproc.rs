//! The in-process hvx engine (M132; the default since M137): every [`Hvx`] method runs the Rust
//! engine (`hydo-hvx`, the port of the deleted `hvx_explore.c`) on a thread of this process,
//! which opens `/dev/hvx` itself, instead of starting the engine program. Since M142 the host and
//! the engine thread exchange the protocol's values themselves ([`crate::link::ChanLink`] here,
//! `hydo_hvx::port::ChanPort` there; no text in between), and the engine is configured with a
//! typed `hydo_hvx::engine::Start` instead of a command line, so every caller (the explorer, `hydo
//! run`/`repro`, the controller, the lock-step client) gets the same values as from the program.
//!
//! [`chosen`] decides: `HYDO_ENGINE=inproc` always, `HYDO_ENGINE=process` (or `c`) never, and
//! otherwise exactly when this is Linux, `HYDO_SUDO` is not `1` and this process can read and
//! write the device (`HVX_DEV`, default `/dev/hvx`): as root, or as a user the udev rule gives the
//! device to. A user who cannot open it gets the program `hydo-hvx` under `hvx-run sudo`. These
//! are `hydo_config::engine::Engine`, read from the [`Hvx`]'s configuration with its overrides
//! on top ([`settings`]); nothing here reads the environment (M144).
//!
//! Each engine thread holds the module's shared lock while it runs, as `hvx-run` holds it for a
//! program: `flock(LOCK_SH)` on `$HYDO_LOCK`, default `.hvx-module.lock` in the home directory
//! (the invoking user's under sudo: `$SUDO_USER`'s), so a module reload waits for it. The [`Hvx`]
//! fields map as follows: `engine` and `hvx_run` are not used, `env` and then `extra_env`
//! (`KEY=VALUE` words) go on top of `config` for the engine thread's options, and the snapshot
//! store is always memory.

use crate::hvx::Hvx;
#[cfg(target_os = "linux")]
use crate::process::Proc;
#[cfg(target_os = "linux")]
use crate::protocol::EngineMsg;
use crate::{BootSpec, EngineError};
use hydo_config::engine::{Choice, Engine};
use hydo_schedule::Schedule;
use std::io::Write;
use std::path::Path;

/// Whether `h` runs in this process (see the module comment).
pub fn chosen(h: &Hvx) -> bool {
    let e = Engine::from_env(&settings(h));
    match e.choice {
        Choice::InProcess => true,
        Choice::Program => false,
        Choice::Auto => cfg!(target_os = "linux") && !e.sudo && device_usable(e.dev()),
    }
}

/// This process can read and write `dev` (access(2) with the real ids, as `hvx-run`'s test).
fn device_usable(dev: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(dev) else {
        return false;
    };
    // SAFETY: a valid NUL-terminated path
    unsafe { libc::access(c.as_ptr(), libc::R_OK | libc::W_OK) == 0 }
}

/// The module lock file: the configured one (`HYDO_LOCK`), else `.hvx-module.lock` in
/// `$SUDO_USER`'s home when this runs as root under sudo, else in `$HOME`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn lock_path(e: &Engine) -> Option<std::path::PathBuf> {
    if let Some(l) = &e.lock {
        return Some(l.clone());
    }
    let get = |k: &str| std::env::var(k).ok();
    // SAFETY: geteuid has no preconditions
    if unsafe { libc::geteuid() } == 0 {
        if let Some(u) = get("SUDO_USER").filter(|u| !u.is_empty() && u != "root") {
            if let Some(h) = home_of(&u) {
                return Some(h.join(".hvx-module.lock"));
            }
        }
    }
    get("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| std::path::Path::new(&h).join(".hvx-module.lock"))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn home_of(user: &str) -> Option<std::path::PathBuf> {
    let c = std::ffi::CString::new(user).ok()?;
    // SAFETY: getpwnam returns NULL or a pointer into static storage, read at once
    unsafe {
        let pw = libc::getpwnam(c.as_ptr());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let d = std::ffi::CStr::from_ptr((*pw).pw_dir);
        Some(std::path::PathBuf::from(d.to_string_lossy().into_owned()))
    }
}

/// Take the shared module lock (blocking while a reload holds it exclusively). The file is
/// created when missing, owned by the sudo user when this runs as root. None: no lock file can
/// be opened (no home directory); the engine then runs without it, as a program run without
/// the wrapper does.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn shared_lock(e: &Engine) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let get = |k: &str| std::env::var(k).ok();
    let p = lock_path(e)?;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .open(&p)
        .or_else(|_| {
            let f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&p)?;
            let ids = (
                get("SUDO_UID").and_then(|v| v.parse::<u32>().ok()),
                get("SUDO_GID").and_then(|v| v.parse::<u32>().ok()),
            );
            if let (Some(u), Some(g)) = ids {
                // SAFETY: a valid open descriptor
                unsafe { libc::fchown(f.as_raw_fd(), u, g) };
            }
            Ok::<_, std::io::Error>(f)
        })
        .ok()?;
    loop {
        // SAFETY: a valid open descriptor
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH) } == 0 {
            return Some(f);
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return None;
        }
    }
}

/// The configuration the engine thread reads: the program's, then `env`, then `extra_env`.
fn settings(h: &Hvx) -> hydo_config::Env {
    h.config
        .with(h.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .with_words(&h.extra_env)
}

/// What the engine boots: `mode` (the program's `--control boot serve|one`) with the spec's
/// paths and options.
#[cfg(target_os = "linux")]
fn start_of(spec: &BootSpec, mode: hydo_hvx::engine::Mode) -> hydo_hvx::engine::Start {
    hydo_hvx::engine::Start {
        mode,
        kernel: spec.kernel.clone(),
        initrd: spec.initrd.clone().unwrap_or_default(),
        image: spec.image.clone(),
        outdir: spec.outdir.clone(),
        // only a value other than the default is passed, as on the program's command line
        ram_mib: (spec.ram_mib != 1024).then_some(spec.ram_mib as u64),
        vcpus: (spec.vcpus > 1).then_some(spec.vcpus),
        cpu_v3: spec.cpu == crate::CpuLevel::V3,
    }
}

/// How the engine thread is asked to run (the program's modes).
#[derive(Clone, Copy)]
enum How {
    ControlBootServe,
    ControlBootOne,
}

/// Start the engine on a thread: its port is the far end of the returned link; `diag` takes its
/// diagnostics.
#[cfg(target_os = "linux")]
fn spawn(
    h: &Hvx,
    spec: &BootSpec,
    how: How,
    schedule: Option<&Schedule>,
    diag: Box<dyn Write + Send>,
) -> Result<crate::process::Started, EngineError> {
    use hydo_hvx::engine::{run, Mode, Options};
    use hydo_hvx::port::ChanPort;
    let e = settings(h);
    let o = Options::from_env(&e);
    let lock = shared_lock(&Engine::from_env(&e));
    let s = schedule.cloned();
    let start = match how {
        How::ControlBootServe => start_of(spec, Mode::Serve { boot: s }),
        How::ControlBootOne => start_of(spec, Mode::One(s.expect("a schedule"))),
    };
    let (htx, hrx) = std::sync::mpsc::channel::<crate::protocol::HostMsg>();
    let (etx, erx) = std::sync::mpsc::channel::<EngineMsg>();
    let port = ChanPort::new(hrx, etx);
    let j = std::thread::Builder::new()
        .name("hvx-engine".into())
        .spawn(move || {
            let rc = run(start, o, Box::new(port), diag);
            drop(lock); // the shared module lock, held while this engine ran
            rc
        })
        .map_err(EngineError::Io)?;
    Ok(crate::process::Started {
        proc: Proc::Thread(Some(j)),
        link: Box::new(crate::link::ChanLink::new(htx, erx)),
    })
}

#[cfg(not(target_os = "linux"))]
fn spawn(
    h: &Hvx,
    spec: &BootSpec,
    how: How,
    schedule: Option<&Schedule>,
    diag: Box<dyn Write + Send>,
) -> Result<crate::process::Started, EngineError> {
    let _ = (settings(h), spec, how, schedule, diag);
    Err(EngineError::Unsupported(
        "the in-process hvx engine needs Linux and /dev/hvx".into(),
    ))
}

/// The engine's diagnostics appended to `stderr_log`.
fn diag_file(stderr_log: &Path) -> Result<Box<dyn Write + Send>, EngineError> {
    Ok(Box::new(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(stderr_log)?,
    ))
}

/// `--control boot serve` (M121).
pub(crate) fn start_controlled(
    h: &Hvx,
    spec: &BootSpec,
    boot: &Schedule,
    stderr_log: &Path,
) -> Result<crate::ControlledBoot, EngineError> {
    let s = spawn(
        h,
        spec,
        How::ControlBootServe,
        Some(boot),
        diag_file(stderr_log)?,
    )?;
    Ok(crate::ControlledBoot::from_started(s))
}

/// `--control boot one` (M121).
pub(crate) fn one_controlled(
    h: &Hvx,
    spec: &BootSpec,
    schedule: &Schedule,
    stderr_log: &Path,
) -> Result<crate::ControlledRun, EngineError> {
    let s = spawn(
        h,
        spec,
        How::ControlBootOne,
        Some(schedule),
        diag_file(stderr_log)?,
    )?;
    Ok(crate::ControlledRun::from_started(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_and_environment() {
        let h = Hvx {
            engine: "/x".into(),
            hvx_run: None,
            extra_env: vec!["HVX_TURN_MARGIN=160".into()],
            env: vec![("HVX_DEV".into(), "/dev/hvxside".into())],
            config: hydo_config::Env::default(),
        };
        let e = settings(&h);
        assert_eq!(e.text("HVX_TURN_MARGIN").as_deref(), Some("160"));
        assert_eq!(e.text("HVX_DEV").as_deref(), Some("/dev/hvxside"));
        // a device nobody has: the program, unless forced
        assert!(!chosen(&h));
        let mut f = h.clone();
        f.env.push(("HYDO_ENGINE".into(), "inproc".into()));
        assert!(chosen(&f));
        let mut p = h.clone();
        p.env.push(("HVX_DEV".into(), "/dev/null".into()));
        p.env.push(("HYDO_ENGINE".into(), "process".into()));
        assert!(!chosen(&p));
        let mut l = h.clone();
        l.env.push(("HYDO_LOCK".into(), "/tmp/x.lock".into()));
        assert_eq!(
            lock_path(&Engine::from_env(&settings(&l))).as_deref(),
            Some(std::path::Path::new("/tmp/x.lock"))
        );
    }
}
