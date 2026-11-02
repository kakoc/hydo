//! The in-process Mac engine (M143): with `HYDO_HVM=inproc` every [`Hvm`] method runs the Rust
//! engine (`hydo-hvm`, the port of `hv/hvm/explore/hvmx`) on a thread of this process. As for hvx
//! (M142), the two sides exchange typed values over channels ([`crate::link::ChanLink`] here,
//! `hydo_hvx::port::ChanPort` in the engine) and no text in between; the engine is configured
//! with a typed `hydo_hvm::serve::Start`, so the schedule is never printed and read back.
//!
//! Two limits of Hypervisor.framework decide where it can be used: a process has at most one VM
//! (a second in-process engine while one runs ends with an error naming the limit; a pool of
//! several workers needs the program), and the process itself must be signed with the hypervisor
//! entitlement (`codesign --force --entitlements hv/hvm/hv.entitlements -s - <program>`).
//! The engine thread's options are the configuration the program read at its start
//! ([`Hvm::config`]), then [`Hvm::env`], then `HVM_DISK` (the image's data disk, as the program
//! gets it).

use crate::hvm::{disk_for, Hvm};
use crate::process::Proc;
use crate::protocol::EngineMsg;
use crate::{BootSpec, EngineError, OneRun};
use hydo_schedule::Schedule;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

/// The engine's options: the configuration, the [`Hvm::env`] pairs, then `HVM_DISK`.
fn options(h: &Hvm, spec: &BootSpec) -> hydo_config::hvm::Options {
    let mut e = h.engine_env();
    if let Some(d) = disk_for(&spec.image) {
        e = e.with([(hydo_config::registry::HVM_DISK, d.display().to_string())]);
    }
    hydo_config::hvm::Options::from_env(&e)
}

/// What the engine boots and how (the program's modes).
#[derive(Clone, Copy)]
enum How {
    One,
    Serve,
    ControlOne,
    ControlBootServe,
    ControlBootOne,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn spawn(
    h: &Hvm,
    spec: &BootSpec,
    how: How,
    schedule: Option<&Schedule>,
    diag: Box<dyn Write + Send>,
    echo: Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>>,
) -> Result<crate::process::Started, EngineError> {
    use hydo_hvm::serve::{run, Mode, Start};
    use hydo_hvx::port::ChanPort;
    let o = options(h, spec);
    let s = schedule.cloned();
    let mode = match how {
        How::One | How::ControlOne | How::ControlBootOne => Mode::One(s.expect("a schedule")),
        How::Serve => Mode::Serve { boot: None },
        How::ControlBootServe => Mode::Serve { boot: s },
    };
    let start = Start {
        mode,
        control_one: matches!(how, How::ControlOne),
        control_boot: matches!(how, How::ControlBootServe | How::ControlBootOne),
        kernel: spec.kernel.clone(),
        initrd: spec.image.clone(),
        outdir: spec.outdir.clone(),
        // only a value other than the default is passed, as on the program's command line
        ram_mib: (spec.ram_mib != 1024).then_some(spec.ram_mib as u64),
    };
    let (htx, hrx) = std::sync::mpsc::channel::<crate::protocol::HostMsg>();
    let (etx, erx) = std::sync::mpsc::channel::<EngineMsg>();
    let mut port = ChanPort::new(hrx, etx);
    if let Some(e) = echo {
        port = port.with_echo(e);
    }
    let j = std::thread::Builder::new()
        .name("hvm-engine".into())
        .spawn(move || run(start, &o, Box::new(port), diag, None))
        .map_err(EngineError::Io)?;
    Ok(crate::process::Started {
        proc: Proc::Thread(Some(j)),
        link: Box::new(crate::link::ChanLink::new(htx, erx)),
    })
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn spawn(
    h: &Hvm,
    spec: &BootSpec,
    how: How,
    schedule: Option<&Schedule>,
    diag: Box<dyn Write + Send>,
    echo: Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>>,
) -> Result<crate::process::Started, EngineError> {
    let _ = (options(h, spec), how, schedule, diag, echo);
    Err(EngineError::Unsupported(
        "the in-process Mac engine needs macOS on Apple Silicon".into(),
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

/// `one`: the RESULT as a value; the output is the report lines and diagnostics in the order
/// they were written, as a program's stdout and stderr in one pipe.
pub(crate) fn one(h: &Hvm, spec: &BootSpec, schedule: &Schedule) -> Result<OneRun, EngineError> {
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
    Ok(OneRun {
        result,
        output: String::from_utf8_lossy(&bytes).into_owned(),
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
    let s = spawn(h, spec, How::Serve, None, diag_file(stderr_log)?, None)?;
    Ok(Box::new(crate::process::ProcessWorker::from_started(
        s, false,
    )?))
}

/// `--control one` (M119).
pub(crate) fn one_lockstep(
    h: &Hvm,
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
    h: &Hvm,
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
    h: &Hvm,
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
