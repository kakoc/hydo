//! The KVM engine (hydo-kvm, docs/design/kvm-engine.md): which engine runs and where
//! ([`Engine`], read by hydo-engine and the `hydo` command) and the engine's own options
//! ([`Options`]). Numbers are kept as text and read by the engine at the point of use, as the
//! other engines' options are.

use crate::registry::*;
use crate::Env;
use std::path::PathBuf;

/// Where the KVM engine runs (`HYDO_KVM`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Choice {
    /// On a thread of this process (the default: KVM allows several VMs in one process).
    #[default]
    InProcess,
    /// `process`: the program `hydo-kvm`, one process per worker.
    Program,
}

/// Which x86_64 engine the `hydo` command uses on Linux (`HYDO_BACKEND`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    /// The hvx kernel module (`/dev/hvx`), the default.
    #[default]
    Hvx,
    /// Stock Linux KVM (`/dev/kvm`).
    Kvm,
}

impl Backend {
    pub fn parse(s: &str) -> Option<Backend> {
        match s {
            "hvx" => Some(Backend::Hvx),
            "kvm" => Some(Backend::Kvm),
            _ => None,
        }
    }
}

/// The KVM engine group (hydo-engine, hydo-cli).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Engine {
    /// `HYDO_KVM`: `process`, else in this process.
    pub choice: Choice,
    /// `HYDO_KVM_ENGINE`: the engine program; None (unset or empty): the tree's or release's.
    pub program: Option<PathBuf>,
    /// `HYDO_KVM_KERNEL`: the guest kernel; None: `bzImage-kvm` found by its sha256.
    pub kernel: Option<PathBuf>,
    /// `HYDO_KVM_DEV`: the device (default `/dev/kvm`).
    pub dev: Option<PathBuf>,
    /// `HYDO_BACKEND`: `kvm` makes the `hydo` command use this engine; None: unset (hvx).
    pub backend: Option<String>,
}

/// The device KVM is opened at by default.
pub const DEFAULT_DEV: &str = "/dev/kvm";

impl Engine {
    pub fn from_env(e: &Env) -> Engine {
        Engine {
            choice: match e.text(HYDO_KVM).as_deref() {
                Some("process") => Choice::Program,
                _ => Choice::InProcess,
            },
            program: e.path_nonempty(HYDO_KVM_ENGINE),
            kernel: e.path_nonempty(HYDO_KVM_KERNEL),
            dev: e.path_nonempty(HYDO_KVM_DEV),
            backend: e.nonempty(HYDO_BACKEND),
        }
    }

    /// The device path.
    pub fn dev(&self) -> PathBuf {
        self.dev
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DEV))
    }
}

/// The KVM engine's own options (hydo-kvm).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `HYDO_KVM_DEV`: the device (default `/dev/kvm`).
    pub dev: Option<String>,
    /// `HYDO_KVM_STEP`: nanoseconds of guest time per VM exit (default 2500).
    pub step: Option<String>,
    /// `HYDO_KVM_RUN_EXITS`: the exit budget per phase (default 50,000,000).
    pub run_exits: Option<String>,
    /// `HYDO_KVM_SPIN_SECS`: host seconds without an exit before a SPIN (default 10).
    pub spin_secs: Option<String>,
    /// `HYDO_KVM_SPIN_MS`: host milliseconds without an exit before the spin observer looks
    /// (default 5).
    pub spin_ms: Option<String>,
    /// `HYDO_KVM_TRACE`: one line per exit into this file.
    pub trace: Option<String>,
    /// `HYDO_KVM_ECHO`, set: the console copied to stdout (the program).
    pub echo: bool,
    /// `HYDO_KVM_DIAG`, set: per-component hashes of a run's end state.
    pub diag: bool,
    /// `HYDO_KVM_DIAG_RAM`: with HYDO_KVM_DIAG, the first this many MiB of guest RAM written next
    /// to each fingerprint (`<tag>.<phase>.ram`).
    pub diag_ram: Option<String>,
    /// `HYDO_KVM_TRACE_UNKNOWN`, set: log MSRs and ports the engine's model does not know.
    pub trace_unknown: bool,
    /// `HYDO_KVM_SPIN_STATE`, set: the spin observer's full-state cycles (a prototype).
    pub spin_state: bool,
    /// `HYDO_KVM_NO_FP`, set: no fingerprints (they read as 0), to measure what they cost.
    pub no_fp: bool,
}

impl Options {
    pub fn from_env(e: &Env) -> Options {
        let t = |k: &str| e.text(k);
        Options {
            dev: t(HYDO_KVM_DEV),
            step: t(HYDO_KVM_STEP),
            run_exits: t(HYDO_KVM_RUN_EXITS),
            spin_secs: t(HYDO_KVM_SPIN_SECS),
            spin_ms: t(HYDO_KVM_SPIN_MS),
            trace: t(HYDO_KVM_TRACE),
            echo: e.is_set(HYDO_KVM_ECHO),
            diag: e.is_set(HYDO_KVM_DIAG),
            diag_ram: t(HYDO_KVM_DIAG_RAM),
            trace_unknown: e.is_set(HYDO_KVM_TRACE_UNKNOWN),
            spin_state: e.is_set(HYDO_KVM_SPIN_STATE),
            no_fp: e.is_set(HYDO_KVM_NO_FP),
        }
    }
}
