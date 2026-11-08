#![doc = include_str!("../README.md")]
//!
//! ## Notes for hydo's developers
//!
//! One interface to hydo's run engines.
//!
//! Two layers. A [`ControlledEngine`] boots an agent image (every image `hydo build` makes since
//! Milestone 136) under lock-step control from its first instruction and runs a schedule's host
//! part from a snapshot the host took, stopping at every numbered stop for the host's commands
//! ([`lockstep`]); the host controller (`hydo-control`'s `AgentEngine`) drives it and is the
//! [`Engine`] every caller uses (the explorer, `hydo run`/`repro`, hydo-bench): it boots an image,
//! takes the two snapshot points, runs one schedule at a time and answers with a verdict and
//! fingerprints. The shell-init images' plain path (an engine that booted to the init's markers
//! by itself and typed a schedule's guest part, `RUN`, and its timing branches, `BRANCH`) was
//! deleted in Milestone 145; such an image is refused by [`image::control`].
//!
//! Three controlled engines exist today, all driven with the same typed calls
//! (`hydo-engine-proto`; [`link::Link`] carries them as values to an engine thread of this
//! process, or as the protocol's lines to an engine program):
//!
//! - [`hvx::Hvx`]: the Rust engine `hydo-hvx` (`hydo/crates/hydo-hvx`) on x86_64 Linux with the
//!   hvx kernel module (`/dev/hvx`): on a thread of this process when it can open the device,
//!   otherwise as the program `hydo-hvx` under the shared-lock wrapper (`hvx-run`, with sudo).
//!   It replaced the C `hv/hvx/hvx_explore` (M132; deleted in M137).
//! - [`hvm::Hvm`]: the Rust engine `hydo-hvm` on an Apple Silicon Mac (Hypervisor.framework), a
//!   program, or in this process with `HYDO_HVM=inproc`. It replaced the C `hvmx` (M143; deleted
//!   in M147).
//! - [`kvm::Kvm`]: the Rust engine `hydo-kvm` on x86_64 Linux with stock KVM (`/dev/kvm`, no
//!   kernel module): the Mac engine's exit-count clock and protocol with x86_64 devices
//!   (docs/design/kvm-engine.md); in this process by default, or the program `hydo-kvm`.
//!
//! All speak the same protocol (see [`protocol`]): `--control boot serve` stops from the first
//! instruction until the host has taken S0 and S1 and ended the boot, prints `READY
//! boot_secs=<s>`, then answers each `CTL`/`RESUME` with a controlled run ending in one `RESULT`
//! line; `--control boot one` runs a single schedule from a fresh boot the same way.
//!
//! The snapshot points ([`SnapshotPoint`]) are the same for every image: S0 (root mounted,
//! nothing started: the start of every recovery boot) and S1 (services up and provisioned: the
//! start of every schedule); the host takes them where its controller decides. The recovery boot after a power cut (a guest `C=` token or a host
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
//!   [`BootSpec::snapshots`]: [`SnapshotStore::Files`] or [`SnapshotStore::Memory`]. Since the
//!   Rust hvx engine (M137) every hvx engine keeps them in memory whatever is asked, and hvm keeps
//!   its files; the field no longer changes anything but the explorer's printed pool line.
//!   Callers never see snapshot files.
//! - **Reset to a snapshot is a worker operation.** `Worker::reset` puts the worker's VM back at
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
pub mod kvm;
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
pub use protocol::{EngineMsg, HostMsg, Ready, Request, RunResult};

/// Which engine a spec is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    /// The Rust hvx engine (`hydo-hvx`; x86_64 Linux, VT-x, the hvx kernel module).
    Hvx,
    /// The Rust Mac engine `hydo-hvm` (Apple Silicon, Hypervisor.framework).
    Hvm,
    /// The KVM engine `hydo-kvm` (x86_64 Linux with `/dev/kvm`, no kernel module; chosen with
    /// `--backend kvm` or `HYDO_BACKEND=kvm`, never the native default).
    Kvm,
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
            EngineKind::Kvm => "kvm",
        }
    }
}

/// The guest CPU level (hydo.yaml `guest.cpu`): x86-64-v2 (the default) or v3 (AVX/AVX2; hvx and KVM).
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
    /// The snapshot name the host gives the point in an agent image's controlled boot (M121:
    /// there is no marker; the host says `SNAP S0` and `SNAP S1` at stops it chooses).
    pub fn name(self) -> &'static str {
        match self {
            SnapshotPoint::S0 => "S0",
            SnapshotPoint::S1 => "S1",
        }
    }
}

/// What to boot. The defaults are what both engines assume when an option is not passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootSpec {
    /// hvx: the image's disk (`*-disk.img`, root + data partitions); hvm: the arm64 initramfs
    /// (`*-arm64.cpio.gz`), whose data disk `<same name>-disk.img` is attached when it exists.
    pub image: PathBuf,
    /// Guest RAM in MiB (hydo.yaml `guest.ram_mib`; 256..8192 on every engine; default 1024).
    pub ram_mib: u32,
    /// vCPUs (hvx only; default 1).
    pub vcpus: u32,
    /// Guest CPU level (hvx and KVM).
    pub cpu: CpuLevel,
    /// hydo.yaml `guest.cpu_cache` (M203): the guest's CPUID describes a fixed cache hierarchy
    /// (hvx and KVM; default false: the cache leaves are zero, as before).
    pub cpu_cache: bool,
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
    /// Snapshot files (the deleted C engine's `HVX_SNAP_DIR`; hvm: `HVM_SNAP_DIR`).
    #[default]
    Files,
    /// In memory (the deleted C engine's `HVX_SNAP_MALLOC=1`). hydo-hvx always keeps its
    /// snapshots in memory, so for hvx the two choices are the same since M137.
    Memory,
}

impl BootSpec {
    /// The engine options in front of the mode, as explore.py passes them: `--ram-mib N` only for a
    /// size other than 1024, `--vcpus N` only above 1 (hvx), `--cpu v3` only for v3 (hvx, KVM),
    /// `--cpu-cache on` only with `guest.cpu_cache` (hvx, KVM).
    pub fn engine_args(&self, kind: EngineKind) -> Vec<String> {
        let mut a = Vec::new();
        if self.ram_mib != 1024 {
            a.push("--ram-mib".to_string());
            a.push(self.ram_mib.to_string());
        }
        if kind == EngineKind::Hvx && self.vcpus > 1 {
            a.push("--vcpus".to_string());
            a.push(self.vcpus.to_string());
        }
        if self.cpu == CpuLevel::V3 && matches!(kind, EngineKind::Hvx | EngineKind::Kvm) {
            a.push("--cpu".to_string());
            a.push("v3".to_string());
        }
        if self.cpu_cache && matches!(kind, EngineKind::Hvx | EngineKind::Kvm) {
            a.push("--cpu-cache".to_string());
            a.push("on".to_string());
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
    /// The engine cannot do this (for example a schedule token one engine does not have).
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

/// What every caller runs schedules with: boot an image to its snapshot points, run one schedule
/// from a fresh boot or from S1 in a [`Worker`], and answer with a verdict and fingerprints. The
/// implementation is the host controller over a [`ControlledEngine`] (`hydo-control`'s
/// `AgentEngine`); tests implement it with scripted results.
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

/// A run engine: it boots an agent image (one [`image::control`] accepts) under lock-step control
/// and runs schedules' host parts under it. Implementations: [`hvx::Hvx`] (the Rust engine in this
/// process, [`inproc`], or the program `hydo-hvx`) and [`hvm::Hvm`].
pub trait ControlledEngine {
    fn kind(&self) -> EngineKind;

    /// Boot an agent image under lock-step control from its first instruction (M121): the
    /// returned boot's session reports stops in [`lockstep::Phase::Boot`], the caller takes the
    /// snapshots `S0` and `S1` where it decides (`SNAP S0`, `SNAP S1`; [`SnapshotPoint::name`])
    /// and ends the boot with END; then [`ControlledBoot::ready`] gives the worker, whose
    /// [`ControlledWorker::lockstep`] runs ([`LockstepStart::Run`]) restore the caller's S1 and
    /// type nothing (the guest part is `run`). `boot`'s `rb=` is the boot's cadence (its other
    /// tokens are not used).
    fn start_controlled(
        &self,
        spec: &BootSpec,
        boot: &Schedule,
        stderr_log: &std::path::Path,
    ) -> Result<ControlledBoot, EngineError>;

    /// The fresh-boot counterpart of [`ControlledEngine::start_controlled`] for one schedule: the
    /// boot runs under control as there, and the caller's `SNAP S1` in the boot phase arms
    /// `schedule`'s host part and starts the workload phase at that same stop, so the run equals a
    /// worker's CTL run of `schedule` from that S1 (stop numbers, input hash and fingerprint).
    fn one_controlled(
        &self,
        spec: &BootSpec,
        schedule: &Schedule,
        stderr_log: &std::path::Path,
    ) -> Result<ControlledRun, EngineError>;

    /// The name of the function behind `fp`/`wfp` ([`Engine::fingerprint_scheme`]).
    fn fingerprint_scheme(&self) -> &'static str {
        "fnv1a64-v1"
    }

    /// How many workers this machine can run at once for `spec` ([`Engine::max_workers`]).
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

/// Whether one guest of `ram_mib` fits into the memory this machine has available now (Milestone
/// 179: a guest may have up to 8 GiB, and every engine allocates all of it when the VM is made).
/// Err with the sentence to print when it does not; Ok when it fits or the memory is unknown.
pub fn guest_fits(ram_mib: u64) -> Result<(), String> {
    match available_mib() {
        Some(av) if av < ram_mib + 256 => Err(format!(
            "one guest of {ram_mib} MiB (guest.ram_mib) needs about {} MiB of this machine's memory, \
             and it has {av} MiB available now: free memory, or build the image with a smaller \
             guest.ram_mib",
            ram_mib + 256
        )),
        _ => Ok(()),
    }
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

/// The host's CPU: its vendor (`GenuineIntel`, `AuthenticAMD`, `Apple`) and model name. KVM
/// fingerprints depend on the vendor (docs/design/kvm-engine.md), so every result records it
/// (docs/design/ci.md section 4.1). Read from `/proc/cpuinfo` on Linux and from
/// `machdep.cpu.brand_string` on macOS; None when it cannot be read.
pub fn host_cpu() -> Option<(String, String)> {
    if let Ok(t) = std::fs::read_to_string("/proc/cpuinfo") {
        return host_cpu_of_cpuinfo(&t);
    }
    let out = std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()?;
    let model = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if model.is_empty() {
        return None;
    }
    // Apple Silicon names itself "Apple M2 Pro"; an Intel Mac names its vendor in the model
    let vendor = if model.starts_with("Apple") {
        "Apple"
    } else if model.contains("Intel") {
        "GenuineIntel"
    } else {
        "unknown"
    };
    Some((vendor.into(), model))
}

/// The vendor and the first model name of a `/proc/cpuinfo` text (x86_64: `vendor_id` and `model
/// name`; arm64 Linux has neither and gives `CPU implementer`).
pub fn host_cpu_of_cpuinfo(text: &str) -> Option<(String, String)> {
    let field = |k: &str| {
        text.lines()
            .find(|l| l.split(':').next().is_some_and(|n| n.trim() == k))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let vendor = field("vendor_id").or_else(|| {
        field("CPU implementer").map(|i| match i.as_str() {
            "0x41" => "ARM".to_string(),
            "0x61" => "Apple".to_string(),
            _ => format!("implementer {i}"),
        })
    })?;
    let model = field("model name").unwrap_or_default();
    Some((vendor, model))
}

/// A booted image that runs schedules from S1, one at a time, each in a fresh VM ([`Engine::start`]).
pub trait Worker {
    /// How long the boot to S1 took (the READY line).
    fn boot_secs(&self) -> f64;

    /// Run one schedule from S1 (and, after a power cut, its recovery boot from S0). The engine
    /// resets its VM to S1 first and computes the fingerprints itself.
    fn run(&mut self, tag: &str, schedule: &Schedule) -> Result<RunResult, EngineError>;

    /// Tear the worker down (QUIT) and wait for the engine to exit.
    fn close(self: Box<Self>) -> Result<(), EngineError>;
}

/// A [`ControlledEngine`]'s booted engine ([`ControlledBoot::ready`]): it runs controlled runs
/// from the snapshots the host took, one at a time, each in a fresh VM.
pub trait ControlledWorker {
    /// How long the boot to S1 took (the READY line).
    fn boot_secs(&self) -> f64;

    /// Start a run under lock-step control (`CTL`/`RESUME`, [`lockstep`]): the returned session
    /// reports every stop and waits for the caller's commands until the run's RESULT.
    /// `log`: also write the transcript (`< ` host lines, `> ` engine lines), request included.
    fn lockstep<'a>(
        &'a mut self,
        start: &LockstepStart,
        log: Option<&'a mut dyn std::io::Write>,
    ) -> Result<Lockstep<'a>, EngineError>;

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

/// The engine for this platform under a release root, with the configuration the program read.
pub fn native(root: &std::path::Path, config: hydo_config::Env) -> Box<dyn ControlledEngine> {
    match EngineKind::native() {
        EngineKind::Hvx => Box::new(hvx::Hvx::for_release(root, config)),
        EngineKind::Hvm => Box::new(hvm::Hvm::for_release(root, config)),
        EngineKind::Kvm => Box::new(kvm::Kvm::for_release(root, config)),
    }
}

#[cfg(test)]
mod host_cpu_tests {
    use super::host_cpu_of_cpuinfo;

    #[test]
    fn the_vendor_and_model_come_from_cpuinfo() {
        let amd = "processor\t: 0\nvendor_id\t: AuthenticAMD\ncpu family\t: 25\nmodel name\t: AMD EPYC 9V74 80-Core Processor\n\nprocessor\t: 1\nvendor_id\t: AuthenticAMD\n";
        assert_eq!(
            host_cpu_of_cpuinfo(amd),
            Some((
                "AuthenticAMD".into(),
                "AMD EPYC 9V74 80-Core Processor".into()
            ))
        );
        let arm = "processor\t: 0\nBogoMIPS\t: 48.00\nCPU implementer\t: 0x41\n";
        assert_eq!(host_cpu_of_cpuinfo(arm), Some(("ARM".into(), "".into())));
        assert_eq!(host_cpu_of_cpuinfo("nothing here\n"), None);
    }
}
