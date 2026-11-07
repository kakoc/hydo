//! The inputs of an image build (hydo-build). The `hydo` command fills the release's paths in
//! where a variable is not set ([`Input::set_default`]); hydo-build reads every value from here.

use crate::registry::*;
use crate::Env;
use std::path::PathBuf;

/// One input path with the variable that sets it (named in hydo-build's messages).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    /// The variable's name.
    pub var: &'static str,
    /// Its value, as set (an empty value included); None: unset.
    pub value: Option<String>,
}

impl Input {
    fn read(e: &Env, var: &'static str) -> Input {
        Input {
            var,
            value: e.text(var),
        }
    }

    /// The value when it is not empty.
    pub fn nonempty(&self) -> Option<&str> {
        self.value.as_deref().filter(|v| !v.is_empty())
    }

    /// The value when it is not empty, else `default`.
    pub fn or(&self, default: &str) -> String {
        self.nonempty().unwrap_or(default).to_string()
    }

    /// Set `v` when the variable is unset (Python's `os.environ.setdefault`).
    pub fn set_default(&mut self, v: impl Into<String>) {
        if self.value.is_none() {
            self.value = Some(v.into());
        }
    }
}

/// The build group. Every [`Input`] is a path; unset or empty means hydo-build's default (the
/// pinned `/nix/store` path or the file in the build directory), except where a field says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    /// `HYDO_ONBOARD`: the product's `onboard/` directory (an empty value is a value). Default:
    /// the release's, else the source tree's.
    pub onboard: Option<PathBuf>,
    /// `HYDO_BUSYBOX`: the x86_64 busybox; on arm64 the host busybox that lists the applets.
    pub busybox: Input,
    /// `HYDO_BUSYBOX_ARM64`.
    pub busybox_arm64: Input,
    /// `HYDO_SEEDRNG`.
    pub seedrng: Input,
    /// `HYDO_SEEDRNG_ARM64` (staged only when set and not empty).
    pub seedrng_arm64: Input,
    /// `HYDO_MKE2FS`.
    pub mke2fs: Input,
    /// `HYDO_FAKETIME`.
    pub faketime: Input,
    /// `HYDO_IPROUTE2`.
    pub iproute2: Input,
    /// `HYDO_IPTABLES`.
    pub iptables: Input,
    /// `HYDO_MODROOT`.
    pub modroot: Input,
    /// `HYDO_FCBIN`.
    pub fcbin: Input,
    /// `HYDO_FCBIN_ARM64`.
    pub fcbin_arm64: Input,
    /// `HYDO_WORLDBIN`.
    pub worldbin: Input,
    /// `HYDO_WORLDBIN_ARM64`.
    pub worldbin_arm64: Input,
    /// `HYDO_READ_BIN`.
    pub read_bin: Input,
    /// `HYDO_READ_BIN_ARM64`.
    pub read_bin_arm64: Input,
    /// `HYDO_SQL_BIN`.
    pub sql_bin: Input,
    /// `HYDO_SQL_BIN_ARM64`.
    pub sql_bin_arm64: Input,
    /// `HYDO_AGENT_BIN`.
    pub agent_bin: Input,
    /// `HYDO_AGENT_BIN_ARM64`.
    pub agent_bin_arm64: Input,
    /// `HYDO_ARM64_STORE`: a directory holding `nix/store/...` (unset or empty: the host's).
    pub arm64_store: Input,
    /// `HYDO_TC_ARM64`: the arm64 `tc` (set but empty: an image without network faults).
    pub tc_arm64: Input,
    /// `HYDO_DEBUGFS` (default: next to mke2fs).
    pub debugfs: Input,
    /// `HYDO_AGENT_UNPINNED=1`: stage an agent that is not the pinned one. Default off.
    pub agent_unpinned: bool,
    /// `HYDO_KEEP_OWNERS` set and not empty: keep the build user's owners (Linux release).
    /// Default off.
    pub keep_owners: bool,
    /// `HYDO_VERSION_CHECK=warn`: build a plan whose `hydo_version:` this hydo does not
    /// satisfy, with a warning. Default off (refuse).
    pub version_check_warn: bool,
}

impl Default for Build {
    fn default() -> Build {
        Build::from_env(&Env::default())
    }
}

impl Build {
    pub fn from_env(e: &Env) -> Build {
        Build {
            onboard: e.path(HYDO_ONBOARD),
            busybox: Input::read(e, HYDO_BUSYBOX),
            busybox_arm64: Input::read(e, HYDO_BUSYBOX_ARM64),
            seedrng: Input::read(e, HYDO_SEEDRNG),
            seedrng_arm64: Input::read(e, HYDO_SEEDRNG_ARM64),
            mke2fs: Input::read(e, HYDO_MKE2FS),
            faketime: Input::read(e, HYDO_FAKETIME),
            iproute2: Input::read(e, HYDO_IPROUTE2),
            iptables: Input::read(e, HYDO_IPTABLES),
            modroot: Input::read(e, HYDO_MODROOT),
            fcbin: Input::read(e, HYDO_FCBIN),
            fcbin_arm64: Input::read(e, HYDO_FCBIN_ARM64),
            worldbin: Input::read(e, HYDO_WORLDBIN),
            worldbin_arm64: Input::read(e, HYDO_WORLDBIN_ARM64),
            read_bin: Input::read(e, HYDO_READ_BIN),
            read_bin_arm64: Input::read(e, HYDO_READ_BIN_ARM64),
            sql_bin: Input::read(e, HYDO_SQL_BIN),
            sql_bin_arm64: Input::read(e, HYDO_SQL_BIN_ARM64),
            agent_bin: Input::read(e, HYDO_AGENT_BIN),
            agent_bin_arm64: Input::read(e, HYDO_AGENT_BIN_ARM64),
            arm64_store: Input::read(e, HYDO_ARM64_STORE),
            tc_arm64: Input::read(e, HYDO_TC_ARM64),
            debugfs: Input::read(e, HYDO_DEBUGFS),
            agent_unpinned: e.is(HYDO_AGENT_UNPINNED, "1"),
            keep_owners: e.os(HYDO_KEEP_OWNERS).is_some_and(|v| !v.is_empty()),
            version_check_warn: e.is(HYDO_VERSION_CHECK, "warn"),
        }
    }
}
