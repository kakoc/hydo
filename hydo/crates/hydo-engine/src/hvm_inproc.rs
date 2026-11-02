//! The in-process Mac engine (M143): with `HYDO_HVM=inproc` every [`Hvm`] method runs the Rust
//! engine (`hydo-hvm`, the port of `hv/hvm/explore/hvmx`) on a thread of this process over
//! in-process pipes, so every caller gets the same lines as from the program.
//!
//! Two limits of Hypervisor.framework decide where it can be used: a process has at most one VM
//! (a second in-process engine while one runs ends with an error naming the limit; a pool of
//! several workers needs the program), and the process itself must be signed with the hypervisor
//! entitlement (`codesign --force --entitlements hv/hvm/hv.entitlements -s - <program>`).
//! The engine thread's environment is this process's, then [`Hvm::env`], then `HVM_DISK` (the
//! image's data disk, as the program gets it).

use crate::hvm::{disk_for, Hvm};
use crate::process::{Piped, Proc};
use crate::{BootSpec, EngineError, EngineKind, OneRun};
use hydo_schedule::Schedule;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::time::Instant;

fn args(spec: &BootSpec, flags: &[&str], mode: &str, last: Option<&Schedule>) -> Vec<String> {
    let mut a = vec!["hydo-hvm".to_string()];
    a.extend(spec.engine_args(EngineKind::Hvm));
    a.extend(flags.iter().map(|s| s.to_string()));
    a.push(mode.into());
    a.push(spec.kernel.display().to_string());
    a.push(spec.image.display().to_string());
    a.push(spec.outdir.display().to_string());
    if let Some(s) = last {
        a.push(s.to_string());
    }
    a
}

/// The environment the engine thread sees.
fn lookup(h: &Hvm, spec: &BootSpec) -> impl Fn(&str) -> Option<String> + Send + 'static {
    let mut over: Vec<(String, String)> = h.env.clone();
    if let Some(d) = disk_for(&spec.image) {
        over.push(("HVM_DISK".into(), d.display().to_string()));
    }
    move |k: &str| {
        over.iter()
            .rev()
            .find(|(a, _)| a == k)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(k).ok())
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn spawn(
    h: &Hvm,
    spec: &BootSpec,
    argv: Vec<String>,
    input: Box<dyn std::io::BufRead + Send>,
    out: Box<dyn Write + Send>,
    diag: Box<dyn Write + Send>,
) -> Result<std::thread::JoinHandle<i32>, EngineError> {
    use hydo_hvm::serve::{main_with, Io};
    let get = lookup(h, spec);
    std::thread::Builder::new()
        .name("hvm-engine".into())
        .spawn(move || {
            main_with(
                &argv,
                &get,
                Io {
                    out,
                    input,
                    diag,
                    echo: None,
                },
            )
        })
        .map_err(EngineError::Io)
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn spawn(
    h: &Hvm,
    spec: &BootSpec,
    argv: Vec<String>,
    input: Box<dyn std::io::BufRead + Send>,
    out: Box<dyn Write + Send>,
    diag: Box<dyn Write + Send>,
) -> Result<std::thread::JoinHandle<i32>, EngineError> {
    let _ = (lookup(h, spec), argv, input, out, diag);
    Err(EngineError::Unsupported(
        "the in-process Mac engine needs macOS on Apple Silicon".into(),
    ))
}

fn start_piped(
    h: &Hvm,
    spec: &BootSpec,
    argv: Vec<String>,
    stderr_log: &Path,
) -> Result<Piped, EngineError> {
    let err = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(stderr_log)?;
    let (in_r, in_w) = std::io::pipe()?;
    let (out_r, out_w) = std::io::pipe()?;
    let j = spawn(
        h,
        spec,
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
pub(crate) fn one(h: &Hvm, spec: &BootSpec, schedule: &Schedule) -> Result<OneRun, EngineError> {
    let t0 = Instant::now();
    let (mut r, w) = std::io::pipe()?;
    let w2 = w.try_clone()?;
    let (in_r, in_w) = std::io::pipe()?;
    drop(in_w);
    let j = spawn(
        h,
        spec,
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

/// `serve`: a worker (the only VM of this process while it lives).
pub(crate) fn serve(
    h: &Hvm,
    spec: &BootSpec,
    stderr_log: &Path,
) -> Result<Box<dyn crate::Worker>, EngineError> {
    let p = start_piped(h, spec, args(spec, &[], "serve", None), stderr_log)?;
    Ok(Box::new(crate::process::ProcessWorker::from_piped(
        p, false,
    )?))
}

/// `--control one` (M119).
pub(crate) fn one_lockstep(
    h: &Hvm,
    spec: &BootSpec,
    schedule: &Schedule,
    stderr_log: &Path,
) -> Result<crate::ControlledRun, EngineError> {
    let p = start_piped(
        h,
        spec,
        args(spec, &["--control"], "one", Some(schedule)),
        stderr_log,
    )?;
    Ok(crate::ControlledRun::from_piped(p))
}

/// `--control boot serve` (M121).
pub(crate) fn start_controlled(
    h: &Hvm,
    spec: &BootSpec,
    boot: &Schedule,
    stderr_log: &Path,
) -> Result<crate::ControlledBoot, EngineError> {
    let p = start_piped(
        h,
        spec,
        args(spec, &["--control", "boot"], "serve", Some(boot)),
        stderr_log,
    )?;
    Ok(crate::ControlledBoot::from_piped(p, false))
}

/// `--control boot one` (M121).
pub(crate) fn one_controlled(
    h: &Hvm,
    spec: &BootSpec,
    schedule: &Schedule,
    stderr_log: &Path,
) -> Result<crate::ControlledRun, EngineError> {
    let p = start_piped(
        h,
        spec,
        args(spec, &["--control", "boot"], "one", Some(schedule)),
        stderr_log,
    )?;
    Ok(crate::ControlledRun::from_piped(p))
}
