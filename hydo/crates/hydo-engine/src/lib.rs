//! One interface to hydo's run engines.
//!
//! An engine boots a guest image, takes the two snapshot points every image defines, runs one
//! schedule at a time and answers with a verdict and fingerprints. Two engines exist today, both
//! driven with the same typed calls (`hydo-engine-proto`; [`link::Link`] carries them as values to
//! an engine thread of this process, or as the protocol's lines to an engine program):
//!
//! - [`hvx::Hvx`]: the Rust engine `hydo-hvx` (`hydo/crates/hydo-hvx`) on x86_64 Linux with the
//!   hvx kernel module (`/dev/hvx`): on a thread of this process when it can open the device,
//!   otherwise as the program `hydo-hvx` under the shared-lock wrapper (`hvx-run`, with sudo).
//!   It replaced the C `hv/hvx/hvx_explore` (M132; deleted in M137).
//! - [`hvm::Hvm`]: `hv/hvm/explore/hvmx` on an Apple Silicon Mac (Hypervisor.framework), a
//!   program.
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
//! # The in-process engine (M132; the default since M137)
//!
//! [`hvx::Hvx`] runs the Rust engine (`hydo/crates/hydo-hvx`, the port of the deleted
//! `hvx_explore.c`) on a thread of this process, driving `/dev/hvx` through its ioctls, whenever
//! this process can open the device (`HYDO_ENGINE=inproc|process` forces a choice); see
//! [`inproc`]. It keeps S0 and S1 in memory as their non-zero pages, fills each fresh VM with the
//! non-zero pages it needs only, makes the next fresh VM on a helper thread while a run runs,
//! tears old VMs down on another, and hashes with the exact zero-page shortcut; every line,
//! console and fingerprint is the C engine's. The same engine is also the program `hydo-hvx`
//! (the C engine's command line; [`hvx::program`] finds it, `HYDO_HVX_EXPLORE` names another). Copy-on-write forks of a VM
//! and a VM kept between runs need module support (see docs/HISTORY.md, Milestone 132).
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

pub mod hvm;
pub mod hvm_inproc;
pub mod hvx;
pub mod image;
pub mod inproc;
pub mod link;
pub mod lockstep;
mod process;
pub mod protocol;

use hydo_schedule::Schedule;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

pub use link::{ChanLink, Link, TextLink};
pub use lockstep::{DiskOff, Driver, Lockstep, LockstepStart};
pub use process::{ControlledBoot, ControlledRun};
pub use protocol::{BranchOutcome, BranchRequest, EngineMsg, HostMsg, Ready, Request, RunResult};

/// Which engine a spec is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    /// The Rust hvx engine (`hydo-hvx`; x86_64 Linux, VT-x, the hvx kernel module).
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

    /// The snapshot name the host gives the point in an agent image's controlled boot (M121:
    /// there is no marker; the host says `SNAP S0` and `SNAP S1` at stops it chooses).
    pub fn name(self) -> &'static str {
        match self {
            SnapshotPoint::S0 => "S0",
            SnapshotPoint::S1 => "S1",
        }
    }
}

/// Who runs an image's guest side (`/etc/hydo/control`, [`image::control`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestControl {
    /// The shell init (`hydo-init.sh`): every image without `/etc/hydo/control`. The engines
    /// boot it to the S0/S1 markers by themselves.
    Shell,
    /// The guest agent `hydo-agent`, speaking agent protocol `proto` (`hydo-build --control
    /// agent`, M121): boot it with [`Engine::start_controlled`] / [`Engine::one_controlled`].
    Agent { proto: u64 },
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

/// A run engine. Implementations: [`hvx::Hvx`] (a process, or with `HYDO_ENGINE=inproc` the Rust
/// engine in this process, [`inproc`]) and [`hvm::Hvm`].
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

    /// Boot an agent image (one whose [`image::control`] is [`GuestControl::Agent`]) under
    /// lock-step control from its first instruction (M121): the returned boot's session reports
    /// stops in [`lockstep::Phase::Boot`], the caller takes the snapshots `S0` and `S1` where it
    /// decides (`SNAP S0`, `SNAP S1`; [`SnapshotPoint::name`]) and ends the boot with END; then
    /// [`ControlledBoot::ready`] gives the worker. Its [`Worker::lockstep`] runs
    /// ([`LockstepStart::Run`]) restore the caller's S1 and type nothing (the guest part is `run`);
    /// [`Worker::run`] and [`Worker::branch`] are refused for such an image. `boot`'s `rb=` is
    /// the boot's cadence (its other tokens are not used).
    fn start_controlled(
        &self,
        spec: &BootSpec,
        boot: &Schedule,
        stderr_log: &std::path::Path,
    ) -> Result<ControlledBoot, EngineError> {
        let _ = (spec, boot, stderr_log);
        Err(EngineError::Unsupported(
            "booting an agent image under control is not supported by this engine".into(),
        ))
    }

    /// The fresh-boot counterpart of [`Engine::start_controlled`] for one schedule: the boot runs
    /// under control as there, and the caller's `SNAP S1` in the boot phase arms `schedule`'s host
    /// part and starts the workload phase at that same stop, so the run equals a worker's CTL run
    /// of `schedule` from that S1 (stop numbers, input hash and fingerprint).
    fn one_controlled(
        &self,
        spec: &BootSpec,
        schedule: &Schedule,
        stderr_log: &std::path::Path,
    ) -> Result<ControlledRun, EngineError> {
        let _ = (spec, schedule, stderr_log);
        Err(EngineError::Unsupported(
            "booting an agent image under control is not supported by this engine".into(),
        ))
    }

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

    /// Start a run under lock-step control (hvx's CTL/RESUME, [`lockstep`]): the returned session
    /// reports every stop and waits for the caller's commands until the run's RESULT. No existing
    /// caller uses it; engines without it answer Unsupported.
    /// `log`: also write the transcript (`< ` host lines, `> ` engine lines), request included.
    fn lockstep<'a>(
        &'a mut self,
        start: &LockstepStart,
        log: Option<&'a mut dyn std::io::Write>,
    ) -> Result<Lockstep<'a>, EngineError> {
        let _ = (start, log);
        Err(EngineError::Unsupported(
            "lock-step control is not supported by this engine".into(),
        ))
    }

    /// One schedule from S1 under a host controller (M126; the design's `run_controlled`): the
    /// engine's CTL request, then `driver` answers every stop until the run's RESULT (the power
    /// cut's recovery VM included). `log`: also write the engine transcript.
    fn run_controlled<'a>(
        &'a mut self,
        tag: &str,
        schedule: &Schedule,
        driver: &mut dyn lockstep::Driver,
        log: Option<&'a mut dyn std::io::Write>,
    ) -> Result<RunResult, EngineError> {
        let start = LockstepStart::Run {
            tag: tag.to_string(),
            schedule: schedule.clone(),
        };
        let mut ls = self.lockstep(&start, log)?;
        driver.drive(&mut ls)
    }

    /// The same from a snapshot an earlier controlled run of this worker took (`RESUME`).
    fn resume<'a>(
        &'a mut self,
        tag: &str,
        snapshot: &str,
        driver: &mut dyn lockstep::Driver,
        log: Option<&'a mut dyn std::io::Write>,
    ) -> Result<RunResult, EngineError> {
        let start = LockstepStart::Resume {
            tag: tag.to_string(),
            snapshot: snapshot.to_string(),
        };
        let mut ls = self.lockstep(&start, log)?;
        driver.drive(&mut ls)
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
