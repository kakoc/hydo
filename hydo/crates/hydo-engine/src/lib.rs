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
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::NotBuilt(p) => write!(f, "{} is not built", p.display()),
            EngineError::Io(e) => write!(f, "{e}"),
            EngineError::NotReady(l) => write!(f, "the engine did not start: {l:?}"),
            EngineError::Protocol(l) => write!(f, "unexpected engine output: {l:?}"),
            EngineError::Unsupported(w) => write!(f, "{w}"),
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

    /// The shell line that reproduces one schedule by hand (report.md's repro line).
    fn repro_line(&self, spec: &BootSpec, schedule: &Schedule) -> String;
}

/// A booted engine that runs schedules from S1, one at a time, each in a fresh VM.
pub trait Worker {
    /// How long the boot to S1 took (the READY line).
    fn boot_secs(&self) -> f64;

    /// Run one schedule from S1 (and, after a power cut, its recovery boot from S0).
    fn run(&mut self, tag: &str, schedule: &Schedule) -> Result<RunResult, EngineError>;

    /// Snapshot mid-run and run timing variants from there (hvx's BRANCH; hvm answers Unsupported).
    fn branch(&mut self, req: &BranchRequest) -> Result<BranchOutcome, EngineError>;

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
