//! The in-process hvx engine (M132; the default since M137): every [`Hvx`] method runs the Rust
//! engine (`hydo-hvx`, the port of the deleted `hvx_explore.c`) on a thread of this process,
//! which opens `/dev/hvx` itself, instead of starting the engine program. The engine thread
//! speaks the same protocol over in-process pipes, so every caller (the explorer, `hydo
//! run`/`repro`, the controller, the lock-step client) works unchanged and gets the same lines.
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
use crate::process::{Piped, Proc};
use crate::{BootSpec, EngineError, EngineKind, OneRun};
use hydo_schedule::Schedule;
use std::io::{BufReader, Read, Write};
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

/// The engine program's arguments for `mode`, as `Hvx::argv` builds them for hvx_explore.
fn args(spec: &BootSpec, flags: &[&str], mode: &str, last: Option<&Schedule>) -> Vec<String> {
    let mut a = vec!["hydo-hvx".to_string()];
    a.extend(spec.engine_args(EngineKind::Hvx));
    a.extend(flags.iter().map(|s| s.to_string()));
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
    if let Some(s) = last {
        a.push(s.to_string());
    }
    a
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

#[cfg(target_os = "linux")]
fn spawn(
    h: &Hvx,
    argv: Vec<String>,
    input: Box<dyn std::io::BufRead + Send>,
    out: Box<dyn Write + Send>,
    diag: Box<dyn Write + Send>,
) -> Result<std::thread::JoinHandle<i32>, EngineError> {
    use hydo_hvx::engine::{main_with, Io, Options};
    let get = lookup(h);
    let o = Options::from_lookup(&get);
    let lock = shared_lock(&get);
    std::thread::Builder::new()
        .name("hvx-engine".into())
        .spawn(move || {
            let rc = main_with(&argv, o, Io { out, input, diag });
            drop(lock); // the shared module lock, held while this engine ran
            rc
        })
        .map_err(EngineError::Io)
}

#[cfg(not(target_os = "linux"))]
fn spawn(
    h: &Hvx,
    argv: Vec<String>,
    input: Box<dyn std::io::BufRead + Send>,
    out: Box<dyn Write + Send>,
    diag: Box<dyn Write + Send>,
) -> Result<std::thread::JoinHandle<i32>, EngineError> {
    let _ = (lookup(h), argv, input, out, diag);
    Err(EngineError::Unsupported(
        "the in-process hvx engine needs Linux and /dev/hvx".into(),
    ))
}

/// Start the engine on a thread with piped stdin and stdout and stderr appended to `stderr_log`.
fn start_piped(h: &Hvx, argv: Vec<String>, stderr_log: &Path) -> Result<Piped, EngineError> {
    let err = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(stderr_log)?;
    let (in_r, in_w) = std::io::pipe()?;
    let (out_r, out_w) = std::io::pipe()?;
    let j = spawn(
        h,
        argv,
        Box::new(BufReader::new(in_r)),
        Box::new(std::io::LineWriter::new(out_w)),
        Box::new(err),
    )?;
    let out: Box<dyn Read + Send> = Box::new(out_r);
    Ok(Piped {
        proc: Proc::Thread(Some(j)),
        stdin: Box::new(in_w),
        stdout: BufReader::new(out),
    })
}

/// `one`: stdout and stderr into one pipe, as `run_one` collects a process's output.
pub(crate) fn one(h: &Hvx, spec: &BootSpec, schedule: &Schedule) -> Result<OneRun, EngineError> {
    let t0 = Instant::now();
    let (mut r, w) = std::io::pipe()?;
    let w2 = w.try_clone()?;
    let (in_r, in_w) = std::io::pipe()?;
    drop(in_w); // `one` reads no stdin
    let j = spawn(
        h,
        args(spec, &[], "one", Some(schedule)),
        Box::new(BufReader::new(in_r)),
        Box::new(std::io::LineWriter::new(w)),
        Box::new(w2),
    )?;
    let mut bytes = Vec::new();
    r.read_to_end(&mut bytes)?;
    let code = j.join().ok();
    let output = String::from_utf8_lossy(&bytes).into_owned();
    Ok(OneRun {
        result: output.lines().find_map(crate::protocol::RunResult::parse),
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
    let p = start_piped(h, args(spec, &[], "serve", None), stderr_log)?;
    Ok(Box::new(crate::process::ProcessWorker::from_piped(
        p, true,
    )?))
}

/// `--control one` (M118).
pub(crate) fn one_lockstep(
    h: &Hvx,
    spec: &BootSpec,
    schedule: &Schedule,
    stderr_log: &Path,
) -> Result<crate::ControlledRun, EngineError> {
    let p = start_piped(
        h,
        args(spec, &["--control"], "one", Some(schedule)),
        stderr_log,
    )?;
    Ok(crate::ControlledRun::from_piped(p))
}

/// `--control boot serve` (M121).
pub(crate) fn start_controlled(
    h: &Hvx,
    spec: &BootSpec,
    boot: &Schedule,
    stderr_log: &Path,
) -> Result<crate::ControlledBoot, EngineError> {
    let p = start_piped(
        h,
        args(spec, &["--control", "boot"], "serve", Some(boot)),
        stderr_log,
    )?;
    Ok(crate::ControlledBoot::from_piped(p, false))
}

/// `--control boot one` (M121).
pub(crate) fn one_controlled(
    h: &Hvx,
    spec: &BootSpec,
    schedule: &Schedule,
    stderr_log: &Path,
) -> Result<crate::ControlledRun, EngineError> {
    let p = start_piped(
        h,
        args(spec, &["--control", "boot"], "one", Some(schedule)),
        stderr_log,
    )?;
    Ok(crate::ControlledRun::from_piped(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_and_environment() {
        let spec = BootSpec {
            image: "/b/x-disk.img".into(),
            ram_mib: 2048,
            vcpus: 2,
            cpu: crate::CpuLevel::V3,
            kernel: "/b/bzImage".into(),
            initrd: Some("/b/initramfs.cpio.gz".into()),
            outdir: "/o".into(),
            snapshots: crate::SnapshotStore::Files,
        };
        let s = Schedule::parse("disk=none | run").unwrap();
        assert_eq!(
            args(&spec, &["--control", "boot"], "one", Some(&s)).join(" "),
            "hydo-hvx --ram-mib 2048 --vcpus 2 --cpu v3 --control boot one /b/bzImage /b/initramfs.cpio.gz /b/x-disk.img /o disk=none | run"
        );
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
