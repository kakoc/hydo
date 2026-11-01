//! The in-process hvx engine (M132, opt-in): `HYDO_ENGINE=inproc` makes every [`Hvx`] method
//! run the Rust engine (`hydo-hvx`, the port of `hvx_explore.c`) on a thread of this process,
//! which opens `/dev/hvx` itself, instead of starting the `hvx_explore` program. The engine
//! thread speaks the same protocol over in-process pipes, so every caller (the explorer, `hydo
//! run`/`repro`, the controller, the lock-step client) works unchanged and gets the same lines.
//!
//! The process must be allowed to open the device and must hold the module's shared lock for
//! the whole time, so it runs under the wrapper as a whole: `~/bin/hvx-run sudo env
//! HYDO_ENGINE=inproc hydo-explore ...` on the bare-metal machine. The [`Hvx`] fields map as
//! follows: `engine` and `hvx_run` are not used, `extra_env` (`KEY=VALUE` words) and `env` are the
//! engine thread's environment on top of this process's, and the snapshot store is always memory.

use crate::hvx::Hvx;
use crate::process::{Piped, Proc};
use crate::{BootSpec, EngineError, EngineKind, OneRun};
use hydo_schedule::Schedule;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::time::Instant;

/// `HYDO_ENGINE=inproc`.
pub fn enabled() -> bool {
    std::env::var("HYDO_ENGINE").as_deref() == Ok("inproc")
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
    std::thread::Builder::new()
        .name("hvx-engine".into())
        .spawn(move || main_with(&argv, o, Io { out, input, diag }))
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
    }
}
