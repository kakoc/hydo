//! Which hvx engine runs and with what (hydo-engine; the `hydo` command's run environment).

use crate::registry::{HVX_DEV, HYDO_ENGINE, HYDO_HVX_EXPLORE, HYDO_LOCK, HYDO_SUDO};
use crate::Env;
use std::path::PathBuf;

/// Where the hvx engine runs (`HYDO_ENGINE`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Choice {
    /// In this process exactly when this is Linux, [`Engine::sudo`] is off and this process can
    /// read and write the device (any other value, or unset).
    #[default]
    Auto,
    /// `inproc`: always on a thread of this process.
    InProcess,
    /// `process` (or `c`): always the program.
    Program,
}

/// The engine group.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Engine {
    /// `HYDO_ENGINE`: where the engine runs. Default [`Choice::Auto`].
    pub choice: Choice,
    /// `HYDO_SUDO=1`: the program under sudo even when the device could be opened. Default off.
    pub sudo: bool,
    /// `HVX_DEV`: the device; None (unset or empty): `/dev/hvx`.
    pub dev: Option<String>,
    /// `HYDO_LOCK`: the module lock file; None (unset or empty): `.hvx-module.lock` in the
    /// home directory (`$SUDO_USER`'s under sudo).
    pub lock: Option<PathBuf>,
    /// `HYDO_HVX_EXPLORE`: the engine program; None (unset or empty): the tree's
    /// `target/release/hydo-hvx` when built, else the release's `bin/hydo-hvx`.
    pub program: Option<PathBuf>,
}

/// The device when none is named.
pub const DEFAULT_DEV: &str = "/dev/hvx";

impl Engine {
    pub fn from_env(e: &Env) -> Engine {
        Engine {
            choice: match e.text(HYDO_ENGINE).as_deref() {
                Some("inproc") => Choice::InProcess,
                Some("process") | Some("c") => Choice::Program,
                _ => Choice::Auto,
            },
            sudo: e.is(HYDO_SUDO, "1"),
            dev: e.nonempty(HVX_DEV),
            lock: e.path_nonempty(HYDO_LOCK),
            program: e.path_nonempty(HYDO_HVX_EXPLORE),
        }
    }

    /// The device.
    pub fn dev(&self) -> &str {
        self.dev.as_deref().unwrap_or(DEFAULT_DEV)
    }
}
