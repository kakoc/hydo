//! [`Env`]: the values of hydo's configuration variables, read once.

use crate::registry::{self, VARIABLES};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// The configuration variables' values ([`crate::registry::Kind::User`] and
/// [`crate::registry::Kind::Developer`] only) and the home directory, taken once by
/// [`Env::process`] at a program's start and passed down. The typed groups of
/// [`crate::Config`] are read from it; the hvx engine reads its options from it with the
/// overrides of one engine ([`Env::with`], [`Env::with_words`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    vals: BTreeMap<&'static str, OsString>,
    home: Option<OsString>,
}

fn registered(name: &str) -> Option<&'static str> {
    VARIABLES
        .iter()
        .find(|v| v.name == name && v.kind.is_host_config())
        .map(|v| v.name)
}

impl Env {
    /// This process's environment: the one place hydo reads its variables from it. Call it at
    /// a program's start (a `main`) or in a test, never inside a library.
    pub fn process() -> Env {
        let mut e = Env::default();
        for v in VARIABLES.iter().filter(|v| v.kind.is_host_config()) {
            if let Some(x) = std::env::var_os(v.name) {
                e.vals.insert(v.name, x);
            }
        }
        e.home = std::env::var_os("HOME");
        e
    }

    /// From `(name, value)` pairs (tests; a name that is not a configuration variable is left
    /// out). No home directory.
    pub fn from_pairs<K: AsRef<str>, V: AsRef<OsStr>>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Env {
        Env::default().with(pairs)
    }

    /// This environment with `pairs` on top (later pairs win; names that are not configuration
    /// variables are left out, as the engine reads only those).
    pub fn with<K: AsRef<str>, V: AsRef<OsStr>>(
        &self,
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Env {
        let mut e = self.clone();
        for (k, v) in pairs {
            if let Some(n) = registered(k.as_ref()) {
                e.vals.insert(n, v.as_ref().to_os_string());
            }
        }
        e
    }

    /// This environment with `KEY=VALUE` words on top (a word without `=` is left out).
    pub fn with_words(&self, words: &[String]) -> Env {
        self.with(words.iter().filter_map(|w| w.split_once('=')))
    }

    /// The home directory (`$HOME`) the defaults are under.
    pub fn home(&self) -> PathBuf {
        self.home.clone().map(PathBuf::from).unwrap_or_default()
    }

    /// With this home directory (tests).
    pub fn with_home(mut self, home: impl Into<OsString>) -> Env {
        self.home = Some(home.into());
        self
    }

    fn check(name: &str) {
        debug_assert!(
            registry::find(name).is_some_and(|v| v.kind.is_host_config()),
            "{name} is not a configuration variable of hydo_config::registry"
        );
    }

    /// The raw value (any bytes).
    pub fn os(&self, name: &str) -> Option<&OsStr> {
        Env::check(name);
        self.vals.get(name).map(OsString::as_os_str)
    }

    /// The value as text (None when unset or not UTF-8, as `std::env::var`).
    pub fn text(&self, name: &str) -> Option<String> {
        self.os(name)?.to_str().map(str::to_string)
    }

    /// The value as text when it is not empty.
    pub fn nonempty(&self, name: &str) -> Option<String> {
        self.text(name).filter(|v| !v.is_empty())
    }

    /// The value as a path (any bytes), empty included.
    pub fn path(&self, name: &str) -> Option<PathBuf> {
        self.os(name).map(PathBuf::from)
    }

    /// The value as a path when it is not empty.
    pub fn path_nonempty(&self, name: &str) -> Option<PathBuf> {
        self.os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
    }

    /// Set, whatever the value (a C program's `getenv(...) != NULL`).
    pub fn is_set(&self, name: &str) -> bool {
        self.os(name).is_some()
    }

    /// Set and not `0` (default off).
    pub fn set_not_0(&self, name: &str) -> bool {
        self.text(name).is_some_and(|v| v != "0")
    }

    /// Not `0` (default on).
    pub fn not_0(&self, name: &str) -> bool {
        self.text(name).as_deref() != Some("0")
    }

    /// Exactly `want`.
    pub fn is(&self, name: &str, want: &str) -> bool {
        self.text(name).as_deref() == Some(want)
    }

    /// Every value, as `(name, value)` pairs in name order.
    pub fn pairs(&self) -> impl Iterator<Item = (&'static str, &OsStr)> + '_ {
        self.vals.iter().map(|(k, v)| (*k, v.as_os_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{HVX_DEV, HYDO_HVX_HOSTCALL, HYDO_HVX_REUSE};

    #[test]
    fn overrides_and_flags() {
        let e = Env::from_pairs([("HVX_DEV", "/dev/a"), ("PATH", "/bin")]);
        assert_eq!(e.text(HVX_DEV).as_deref(), Some("/dev/a"));
        assert_eq!(e.pairs().count(), 1, "PATH is not a configuration variable");
        let e = e.with_words(&["HVX_DEV=/dev/b".into(), "junk".into()]);
        assert_eq!(e.text(HVX_DEV).as_deref(), Some("/dev/b"));
        assert!(e.not_0(HYDO_HVX_HOSTCALL));
        assert!(!e.set_not_0(HYDO_HVX_REUSE));
        let e = e.with([(HYDO_HVX_HOSTCALL, "0"), (HYDO_HVX_REUSE, "1")]);
        assert!(!e.not_0(HYDO_HVX_HOSTCALL));
        assert!(e.set_not_0(HYDO_HVX_REUSE));
    }
}
