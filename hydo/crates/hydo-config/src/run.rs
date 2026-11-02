//! Where the explorer and the benchmark find things, the pool, the benchmark, the host
//! controller and the `hydo` command.

use crate::registry::*;
use crate::Env;
use std::path::PathBuf;

/// The paths group (hydo-explore, hydo-bench; the `hydo` command sets the release's).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Paths {
    /// `HYDO_ROOT`: the folder with `hydo/` and `hv/` for the programs `hydo-explore` and
    /// `hydo-bench`; None (unset or empty): found from the program's own path.
    pub root: Option<PathBuf>,
    /// `HYDO_BUILD`: the images, `bzImage` and `initramfs-kafka.cpio.gz`. Default (unset or
    /// empty) `~/hydo/build`.
    pub build: PathBuf,
    /// `HYDO_HVX_RUN`: the shared-lock wrapper the engine program is started through. Default
    /// (unset or empty) `~/bin/hvx-run`.
    pub hvx_run: PathBuf,
}

impl Paths {
    pub fn from_env(e: &Env) -> Paths {
        let home = e.home();
        Paths {
            root: e.path_nonempty(HYDO_ROOT),
            build: e
                .path_nonempty(HYDO_BUILD)
                .unwrap_or_else(|| home.join("hydo").join("build")),
            hvx_run: e
                .path_nonempty(HYDO_HVX_RUN)
                .unwrap_or_else(|| home.join("bin").join("hvx-run")),
        }
    }
}

/// The explorer's worker pool (hydo-explore's pool, which hydo-bench uses too).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pool {
    /// `HYDO_QUIET_FILE`: while this file exists, start no new VM (wait, checking every 30 s,
    /// at most 2 hours). Default: none.
    pub quiet_file: Option<PathBuf>,
    /// `HYDO_EXPLORE_CACHE`: the result cache a deterministic exploration resumes from.
    /// Default: none.
    pub cache: Option<PathBuf>,
}

impl Pool {
    pub fn from_env(e: &Env) -> Pool {
        Pool {
            quiet_file: e.path(HYDO_QUIET_FILE),
            cache: e.path(HYDO_EXPLORE_CACHE),
        }
    }
}

/// The host controller of agent images (hydo-control).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Control {
    /// Not `HVM_SPIN_REWIND=0`: a SPIN of an agent image on hvm runs again in the rewind mode
    /// (M130). Default on.
    pub spin_rewind: bool,
}

impl Default for Control {
    fn default() -> Control {
        Control::from_env(&Env::default())
    }
}

impl Control {
    pub fn from_env(e: &Env) -> Control {
        Control {
            spin_rewind: !e.is(HVM_SPIN_REWIND, "0"),
        }
    }
}

/// The platform the `hydo` command acts as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Mac,
}

/// The `hydo` command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cli {
    /// `HYDO_CLI_PLATFORM=linux|mac`: act as that platform's command; None: this binary's.
    pub platform: Option<Platform>,
}

impl Cli {
    pub fn from_env(e: &Env) -> Cli {
        Cli {
            platform: match e.text(HYDO_CLI_PLATFORM).as_deref() {
                Some("linux") => Some(Platform::Linux),
                Some("mac") => Some(Platform::Mac),
                _ => None,
            },
        }
    }
}
