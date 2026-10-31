//! One interface to hydo's run engines.
//!
//! An engine boots a guest image, takes the two snapshot points every image defines, runs one
//! schedule at a time and answers with a verdict and fingerprints. Two engines exist today, both
//! separate programs that this crate drives over their text protocol, unchanged:
//!
//! - [`hvx::Hvx`]: `hv/hvx/hvx_explore` on x86_64 Linux with the hvx kernel module (`/dev/hvx`),
//!   started through the shared-lock wrapper (`hvx-run`, sudo only when needed).
//! - [`hvm::Hvm`]: `hv/hvm/explore/hvmx` on an Apple Silicon Mac (Hypervisor.framework).
//!
//! Both speak the same protocol (see [`protocol`]): `serve` prints `READY boot_secs=<s>` after
//! booting to S1, then answers each `RUN <tag> <schedule>` line with one `RESULT` line (hvx also
//! answers `BRANCH`), and `one` runs a single schedule from a fresh boot and prints its `RESULT`.
//!
//! hvm also runs lock-step control sessions (`CTRL`, M119): the host types every byte at an exact
//! virtual time and the engine waits at each stop ([`control`], [`Worker::lock_step`],
//! [`Engine::start_fresh`]). Nothing in `run` or `branch` changes for them.
//!
//! The snapshot points ([`SnapshotPoint`]) are the same for every image: S0 (root mounted,
//! nothing started: the start of every recovery boot) and S1 (services up and provisioned: the
//! start of every schedule). The recovery boot after a power cut (a guest `C=` token or a host
//! `cf=` token) is part of one run: the engine carries the flushed data disk into a VM restored
//! from S0, and the run's result is that boot's verdict ([`RunResult::recovery`] tells).
//!
//! # The intended next step: an in-process engine
//!
//! The trait is shaped so that a third implementation can replace the subprocess without changing
//! any caller: the Rust explorer driving `/dev/hvx` directly through its ioctls (`hv/hvx/hvx_abi.h`)
//! inside the `hydo` process. Such an engine boots once, keeps S0 and S1 as in-memory snapshots
//! (registers, device state, the data disk's volatile write cache and guest RAM), and serves each
//! [`Worker::run`] by forking a copy-on-write child VM from S1 instead of restoring from files;
//! a power cut forks the recovery VM from S0 with the flushed disk. [`Engine::start`] then
//! returns a worker that owns those snapshots, [`Worker::run`] keeps its signature, and
//! [`Engine::one`] stays the fresh-boot path that proves a snapshot run equals a boot run. The
//! fingerprints must stay the C engine's (`fnv` over console, snapshot, RAM and disk) so every
//! recorded baseline still holds.
//!
//! # Where the time of a run goes, and how the trait leaves room for it (docs/PERF-RUN-COST.md)
//!
//! A stack run of about 20 s is 41% guest, 22% fingerprint hashing, 21% snapshot files on disk and
//! 15% VM create/fill/teardown; Python is negligible. The trait keeps each of those inside the
//! engine, so a faster implementation changes no caller:
//!
//! - **The worker owns its base snapshot.** [`Engine::start`] returns a [`Worker`] that holds S0
//!   and S1 for its whole life; where they live is the engine's choice, asked for with
//!   [`BootSpec::snapshots`]: [`SnapshotStore::Files`] (today's default, unchanged) or
//!   [`SnapshotStore::Memory`] (hvx: the existing `HVX_SNAP_MALLOC=1`; an in-process engine:
//!   plain memory). Callers never see snapshot files.
//! - **Reset to a snapshot is a worker operation.** [`Worker::reset`] puts the worker's VM back at
//!   S0 or S1. The process engines do it inside every RUN (the default method says so); an
//!   in-process engine may do it by copying back only the pages and disk blocks the last run
//!   dirtied (0.4% to 11% of RAM, under 1.5% of the data disk), and may share the read-only root
//!   between VMs instead of copying it.
//! - **Fingerprinting is engine-side.** A run's `fp`/`wfp` come back in [`RunResult`]; callers
//!   only compare them. [`Engine::fingerprint_scheme`] names the function; a faster exact
//!   implementation (for example one that skips all-zero pages while computing the same value)
//!   lives in the engine and keeps the name, so every recorded baseline still compares.
//! - **The pool size comes from the machine.** [`Engine::max_workers`] answers how many workers
//!   this machine can run for a spec: min(hardware threads, free memory / memory per worker),
//!   not a fixed number ([`machine_workers`]).

pub mod control;
pub mod hvm;
pub mod hvx;
pub mod image;
mod process;
pub mod protocol;

use hydo_schedule::Schedule;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

pub use protocol::{BranchOutcome, BranchRequest, Ready, RunResult};

/// Which engine a spec is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    /// `hv/hvx/hvx_explore` (x86_64 Linux, VT-x, the hvx kernel module).
    Hvx,
    /// `hv/hvm/explore/hvmx` (Apple Silicon, Hypervisor.framework).
    Hvm,
}

impl EngineKind {
    /// The engine of the platform this binary was built for: hvm on macOS, hvx elsewhere.
    pub fn native() -> EngineKind {
        if cfg!(target_os = "macos") {
            EngineKind::Hvm
        } else {
            EngineKind::Hvx
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            EngineKind::Hvx => "hvx",
            EngineKind::Hvm => "hvm",
        }
    }
}

/// The guest CPU level (hydo.yaml `guest.cpu`): x86-64-v2 (the default) or v3 (AVX/AVX2; hvx only).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CpuLevel {
    #[default]
    V2,
    V3,
}

impl CpuLevel {
    pub fn parse(s: &str) -> Option<CpuLevel> {
        match s {
            "v2" => Some(CpuLevel::V2),
            "v3" => Some(CpuLevel::V3),
            _ => None,
        }
    }
}

/// The two snapshot points every hydo image defines (the init prints the marker when it gets there).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotPoint {
    /// Root mounted, data disk not mounted, nothing started: every recovery boot starts here.
    S0,
    /// Services up and provisioned, synced and idle: every schedule starts here.
    S1,
}

impl SnapshotPoint {
    /// The console marker the guest prints when it reaches the point.
    pub fn marker(self) -> &'static str {
        match self {
            SnapshotPoint::S0 => "hydo-explore-ready-S0",
            SnapshotPoint::S1 => "hydo-explore-ready-S1",
        }
    }
}

/// What to boot. The defaults are what both engines assume when an option is not passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootSpec {
    /// hvx: the image's disk (`*-disk.img`, root + data partitions); hvm: the arm64 initramfs
    /// (`*-arm64.cpio.gz`), whose data disk `<same name>-disk.img` is attached when it exists.
    pub image: PathBuf,
    /// Guest RAM in MiB (hydo.yaml `guest.ram_mib`; hvx 256..3072, hvm 256..4096; default 1024).
    pub ram_mib: u32,
    /// vCPUs (hvx only; default 1).
    pub vcpus: u32,
    /// Guest CPU level (hvx only).
    pub cpu: CpuLevel,
    /// The guest kernel: hvx's bzImage, or hvm's arm64 `Image` (an image built for the second
    /// kernel records it; the caller resolves that with `hvm::kernel_for`).
    pub kernel: PathBuf,
    /// hvx only: the switch_root initramfs booted with the bzImage.
    pub initrd: Option<PathBuf>,
    /// Where the engine writes the consoles (`<tag>.log`, `<tag>.recovery.log`).
    pub outdir: PathBuf,
    /// Where a worker keeps its S0/S1 snapshots (no effect on any result).
    pub snapshots: SnapshotStore,
}

/// Where a worker keeps its base snapshots. The choice never changes a result or a fingerprint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SnapshotStore {
    /// Snapshot files (hvx: `$HVX_SNAP_DIR`, hvm: `$HVM_SNAP_DIR`): today's default.
    #[default]
    Files,
    /// In memory: hvx passes `HVX_SNAP_MALLOC=1` (about 5x faster worker boots and ~21% less per
    /// run on a spinning disk, docs/PERF-RUN-COST.md); hvm keeps its files (no such switch yet); an
    /// in-process engine keeps them in its own memory.
    Memory,
}

impl BootSpec {
    /// The engine options in front of the mode, as explore.py passes them: `--ram-mib N` only for a
    /// size other than 1024, `--vcpus N` only above 1 (hvx), `--cpu v3` only for v3 (hvx).
    pub fn engine_args(&self, kind: EngineKind) -> Vec<String> {
        let mut a = Vec::new();
        if self.ram_mib != 1024 {
            a.push("--ram-mib".to_string());
            a.push(self.ram_mib.to_string());
        }
        if kind == EngineKind::Hvx {
            if self.vcpus > 1 {
                a.push("--vcpus".to_string());
                a.push(self.vcpus.to_string());
            }
            if self.cpu == CpuLevel::V3 {
                a.push("--cpu".to_string());
                a.push("v3".to_string());
            }
        }
        a
    }
}

/// The output of a fresh-boot run (`one`).
#[derive(Clone, Debug)]
pub struct OneRun {
    /// The parsed RESULT line, or None when the engine printed none (a crash or a refused schedule).
    pub result: Option<RunResult>,
    /// Everything the engine printed, stdout and stderr interleaved as they were written.
    pub output: String,
    /// The engine's exit status (None when it was killed by a signal).
    pub exit_code: Option<i32>,
    /// Wall-clock time of the whole process, boot included.
    pub wall: Duration,
}

/// Why an engine could not be used (not a failed run: a run's failure is its verdict).
#[derive(Debug)]
pub enum EngineError {
    /// The engine program is missing or not executable.
    NotBuilt(PathBuf),
    /// Starting or talking to the engine process failed.
    Io(std::io::Error),
    /// The engine did not reach READY (its first output line is given).
    NotReady(String),
    /// The engine answered something that is not its protocol.
    Protocol(String),
    /// The engine cannot do this (for example a BRANCH on hvm).
    Unsupported(String),
    /// The engine refused a control command or session (its `ERR` text, or the verdict of the
    /// RESULT line that answered a `CTRL`); the session state is unchanged.
    Refused(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::NotBuilt(p) => write!(f, "{} is not built", p.display()),
            EngineError::Io(e) => write!(f, "{e}"),
            EngineError::NotReady(l) => write!(f, "the engine did not start: {l:?}"),
            EngineError::Protocol(l) => write!(f, "unexpected engine output: {l:?}"),
            EngineError::Unsupported(w) => write!(f, "{w}"),
            EngineError::Refused(w) => write!(f, "the engine refused: {w}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        EngineError::Io(e)
    }
}

/// A run engine. Implementations: [`hvx::Hvx`], [`hvm::Hvm`]; next, an in-process engine (see the
/// crate documentation).
pub trait Engine {
    fn kind(&self) -> EngineKind;

    /// One schedule from a fresh boot, with no snapshot restore before the workload. Its
    /// fingerprint must equal the one a [`Worker`] gives for the same schedule.
    fn one(&self, spec: &BootSpec, schedule: &Schedule) -> Result<OneRun, EngineError>;

    /// Create a worker: boot the image, take S0 and S1, and return when the engine is READY.
    /// `stderr_log`: where the engine's diagnostics go (explore.py: `worker-<n>.err`).
    fn start(
        &self,
        spec: &BootSpec,
        stderr_log: &std::path::Path,
    ) -> Result<Box<dyn Worker>, EngineError>;

    /// A worker whose FIRST request (a RUN or a control session from S1) runs on the boot's own
    /// state at S1 without a snapshot restore, as [`Engine::one`] does; later requests restore as
    /// in [`Engine::start`]. It proves that a control session from a fresh boot equals the same
    /// session from the restored S1 (M119; hvmx's `fresh` mode). Unsupported by default.
    fn start_fresh(
        &self,
        spec: &BootSpec,
        stderr_log: &std::path::Path,
    ) -> Result<Box<dyn Worker>, EngineError> {
        let _ = (spec, stderr_log);
        Err(EngineError::Unsupported(format!(
            "{} has no fresh-boot worker",
            self.kind().name()
        )))
    }

    /// The shell line that reproduces one schedule by hand (report.md's repro line).
    fn repro_line(&self, spec: &BootSpec, schedule: &Schedule) -> String;

    /// The name of the function behind `fp`/`wfp`. Two engines (or two implementations of one)
    /// with the same name give the same value for the same run, whatever they skip internally
    /// (an exact hash may skip all-zero pages). hvx and hvm both hash with 64-bit FNV-1a.
    fn fingerprint_scheme(&self) -> &'static str {
        "fnv1a64-v1"
    }

    /// How many workers this machine can run at once for `spec`: min(hardware threads, available
    /// memory / memory per worker). Callers size their pool with it instead of a fixed number.
    fn max_workers(&self, spec: &BootSpec) -> usize {
        machine_workers(spec.ram_mib as u64 * 2 + 256)
    }
}

/// min(hardware threads, available memory / `per_worker_mib`), at least 1. A worker needs its
/// guest RAM, a copy of it for an in-memory snapshot, and the engine's own memory (the default
/// estimate in [`Engine::max_workers`]: twice the guest RAM plus 256 MiB).
pub fn machine_workers(per_worker_mib: u64) -> usize {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let by_mem = available_mib()
        .map(|m| (m / per_worker_mib.max(1)) as usize)
        .unwrap_or(threads);
    threads.min(by_mem).max(1)
}

/// Memory available for new work, in MiB: `MemAvailable` on Linux; on macOS the free, inactive
/// and purgeable pages (`vm_stat`). None when it cannot be read.
pub fn available_mib() -> Option<u64> {
    if let Ok(t) = std::fs::read_to_string("/proc/meminfo") {
        let kb: u64 = t
            .lines()
            .find(|l| l.starts_with("MemAvailable:"))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        return Some(kb / 1024);
    }
    let out = std::process::Command::new("vm_stat").output().ok()?;
    let t = String::from_utf8_lossy(&out.stdout);
    let page: u64 = t
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |k: &str| -> u64 {
        t.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().trim_end_matches('.').parse().ok())
            .unwrap_or(0)
    };
    Some(
        (pages("Pages free") + pages("Pages inactive") + pages("Pages purgeable")) * page
            / (1 << 20),
    )
}

/// A booted engine that runs schedules from S1, one at a time, each in a fresh VM.
pub trait Worker {
    /// How long the boot to S1 took (the READY line).
    fn boot_secs(&self) -> f64;

    /// Run one schedule from S1 (and, after a power cut, its recovery boot from S0). The engine
    /// resets its VM to S1 first and computes the fingerprints itself.
    fn run(&mut self, tag: &str, schedule: &Schedule) -> Result<RunResult, EngineError>;

    /// Put the worker's VM back at a snapshot point it owns. The process engines (hvx_explore,
    /// hvmx) do this inside every RUN, in a fresh VM, so for them it is a no-op; an in-process
    /// engine may restore only the pages and disk blocks the last run dirtied.
    fn reset(&mut self, point: SnapshotPoint) -> Result<(), EngineError> {
        let _ = point;
        Ok(())
    }

    /// Snapshot mid-run and run timing variants from there (hvx's BRANCH; hvm answers Unsupported).
    fn branch(&mut self, req: &BranchRequest) -> Result<BranchOutcome, EngineError>;

    /// The worker's lock-step control sessions ([`control`], M119), or None when its engine has
    /// none (hvx_explore so far). No existing caller uses it: `run` and `branch` are unchanged.
    fn lock_step(&mut self) -> Option<&mut dyn control::LockStep> {
        None
    }

    /// Tear the worker down (QUIT) and wait for the engine to exit.
    fn close(self: Box<Self>) -> Result<(), EngineError>;
}

/// The engine for this platform with its default configuration under a release root.
pub fn native(root: &std::path::Path) -> Box<dyn Engine> {
    match EngineKind::native() {
        EngineKind::Hvx => Box::new(hvx::Hvx::for_release(root)),
        EngineKind::Hvm => Box::new(hvm::Hvm::for_release(root)),
    }
}
