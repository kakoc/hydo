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
//! device to. A user who cannot open it gets the program `hydo-hvx` under `hvx-run sudo`.
//!
//! Each engine thread holds the module's shared lock while it runs, as `hvx-run` holds it for a
//! program: `flock(LOCK_SH)` on `$HYDO_LOCK`, default `.hvx-module.lock` in the home directory
//! (the invoking user's under sudo: `$SUDO_USER`'s), so a module reload waits for it. The [`Hvx`]
//! fields map as follows: `engine` and `hvx_run` are not used, `extra_env` (`KEY=VALUE` words)
//! and `env` are the engine thread's environment on top of this process's, and the snapshot
//! store is always memory.

use crate::hvx::Hvx;
use crate::process::Proc;
use crate::protocol::EngineMsg;
use crate::{BootSpec, EngineError, OneRun};
use hydo_schedule::Schedule;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

/// Whether `h` runs in this process (see the module comment).
pub fn chosen(h: &Hvx) -> bool {
    let get = lookup(h);
    match get("HYDO_ENGINE").as_deref() {
        Some("inproc") => true,
        Some("process") | Some("c") => false,
        _ => {
            cfg!(target_os = "linux")
                && get("HYDO_SUDO").as_deref() != Some("1")
                && device_usable(&get("HVX_DEV").unwrap_or_else(|| "/dev/hvx".into()))
        }
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

/// The module lock file: `$HYDO_LOCK`, else `.hvx-module.lock` in `$SUDO_USER`'s home when this
/// runs as root under sudo, else in `$HOME`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn lock_path(get: &dyn Fn(&str) -> Option<String>) -> Option<std::path::PathBuf> {
    if let Some(l) = get("HYDO_LOCK").filter(|s| !s.is_empty()) {
        return Some(l.into());
    }
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
fn shared_lock(get: &dyn Fn(&str) -> Option<String>) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let p = lock_path(get)?;
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

/// The environment the engine thread sees: this process's, then `env`, then `extra_env`.
fn lookup(h: &Hvx) -> impl Fn(&str) -> Option<String> + Send + 'static {
    let mut over: Vec<(String, String)> = h.env.clone();
    for w in &h.extra_env {
        if let Some((k, v)) = w.split_once('=') {
            over.push((k.to_string(), v.to_string()));
        }
    }
    move |k: &str| {
        over.iter()
            .rev()
            .find(|(a, _)| a == k)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(k).ok())
    }
}

/// What the engine boots: `mode` with the spec's paths and options (`--control` is
/// `control_one`, `--control boot` is `control_boot`).
#[cfg(target_os = "linux")]
fn start_of(
    spec: &BootSpec,
    mode: hydo_hvx::engine::Mode,
    control_one: bool,
    control_boot: bool,
) -> hydo_hvx::engine::Start {
    hydo_hvx::engine::Start {
        mode,
        control_one,
        control_boot,
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
    One,
    Serve,
    ControlOne,
    ControlBootServe,
    ControlBootOne,
}

/// Start the engine on a thread: its port is the far end of the returned link; `diag` takes its
/// diagnostics; `echo`: also its reports' lines, in order with the diagnostics.
#[cfg(target_os = "linux")]
fn spawn(
    h: &Hvx,
    spec: &BootSpec,
    how: How,
    schedule: Option<&Schedule>,
    diag: Box<dyn Write + Send>,
    echo: Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>>,
) -> Result<crate::process::Started, EngineError> {
    use hydo_hvx::engine::{run, Mode, Options};
    use hydo_hvx::port::ChanPort;
    let get = lookup(h);
    let o = Options::from_lookup(&get);
    let lock = shared_lock(&get);
    let s = schedule.cloned();
    let one = |s: Option<Schedule>| Mode::One(s.expect("a schedule"));
    let start = match how {
        How::One => start_of(spec, one(s), false, false),
        How::Serve => start_of(spec, Mode::Serve { boot: None }, false, false),
        How::ControlOne => start_of(spec, one(s), true, false),
        How::ControlBootServe => start_of(spec, Mode::Serve { boot: s }, false, true),
        How::ControlBootOne => start_of(spec, one(s), false, true),
    };
    let (htx, hrx) = std::sync::mpsc::channel::<crate::protocol::HostMsg>();
    let (etx, erx) = std::sync::mpsc::channel::<EngineMsg>();
    let mut port = ChanPort::new(hrx, etx);
    if let Some(e) = echo {
        port = port.with_echo(e);
    }
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
    echo: Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>>,
) -> Result<crate::process::Started, EngineError> {
    let _ = (lookup(h), spec, how, schedule, diag, echo);
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

/// A writer into a shared buffer.
struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `one`: the RESULT as a value; the output is the engine's report lines and diagnostics in the
/// order they were written, as a program's stdout and stderr in one pipe.
pub(crate) fn one(h: &Hvx, spec: &BootSpec, schedule: &Schedule) -> Result<OneRun, EngineError> {
    let t0 = Instant::now();
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let started = spawn(
        h,
        spec,
        How::One,
        Some(schedule),
        Box::new(Shared(buf.clone())),
        Some(buf.clone()),
    )?;
    let crate::process::Started { proc, mut link } = started;
    // `one` reads no requests; its reports end when the engine thread ends
    let mut result = None;
    while let Ok(m) = link.recv() {
        if let (None, EngineMsg::Result(r)) = (&result, m) {
            result = Some(*r);
        }
    }
    let code = match proc {
        Proc::Thread(Some(j)) => j.join().ok(),
        _ => None,
    };
    let bytes = std::mem::take(&mut *buf.lock().unwrap_or_else(|p| p.into_inner()));
    let output = String::from_utf8_lossy(&bytes).into_owned();
    Ok(OneRun {
        result,
        output,
        exit_code: code,
        wall: t0.elapsed(),
    })
}

/// `serve`: a worker.
pub(crate) fn serve(
    h: &Hvx,
    spec: &BootSpec,
    stderr_log: &Path,
) -> Result<Box<dyn crate::Worker>, EngineError> {
    let s = spawn(h, spec, How::Serve, None, diag_file(stderr_log)?, None)?;
    Ok(Box::new(crate::process::ProcessWorker::from_started(
        s, true,
    )?))
}

/// `--control one` (M118).
pub(crate) fn one_lockstep(
    h: &Hvx,
    spec: &BootSpec,
    schedule: &Schedule,
    stderr_log: &Path,
) -> Result<crate::ControlledRun, EngineError> {
    let s = spawn(
        h,
        spec,
        How::ControlOne,
        Some(schedule),
        diag_file(stderr_log)?,
        None,
    )?;
    Ok(crate::ControlledRun::from_started(s))
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
        None,
    )?;
    Ok(crate::ControlledBoot::from_started(s, false))
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
        None,
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
            snap_dir: "/s".into(),
            extra_env: vec!["HVX_TURN_MARGIN=160".into()],
            env: vec![("HVX_DEV".into(), "/dev/hvxside".into())],
        };
        let get = lookup(&h);
        assert_eq!(get("HVX_TURN_MARGIN").as_deref(), Some("160"));
        assert_eq!(get("HVX_DEV").as_deref(), Some("/dev/hvxside"));
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
            lock_path(&lookup(&l)).as_deref(),
            Some(std::path::Path::new("/tmp/x.lock"))
        );
    }
}
