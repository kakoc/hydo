//! `U=`: a service switched to another version of itself in the middle of a run (Milestone 238,
//! docs/design/upgrades.md).
//!
//! `U=<target>@<at>[/<gap>][+<back>]:<version>`: before step `at`, hydo stops each service of
//! the target (SIGTERM, then SIGKILL after a grace period), switches it to the manifest's version
//! `<version>` (`services[].versions`) and starts it again on the same data, with its readiness
//! check. A target of several services (a list or a group selector) is a rolling upgrade: the
//! services are switched one after another, in the target's order, and the k-th (from 0) before
//! step `at + k*gap`; without `/<gap>` all of them before step `at`, each once the one before it
//! is ready again. With `+<back>` each service is rolled back to the version it ran before,
//! `back` steps after its own switch. A downgrade is the same token with an older version.

use crate::num::{dec, err};
use crate::target::Target;
use crate::ParseError;
use std::fmt;

/// The longest version name.
pub const VERSION_NAME_MAX: usize = 40;

/// Whether `v` is a version name: a letter or digit, then letters, digits, `.`, `_` and `-`, at
/// most [`VERSION_NAME_MAX`] bytes (`3.6.0`, `3.6.0-rc.2`, `v2`, `new`).
pub fn version_name_ok(v: &str) -> bool {
    v.len() <= VERSION_NAME_MAX
        && v.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `U=<target>@<at>[/<gap>][+<back>]:<version>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Upgrade {
    pub target: Target,
    /// The step before which the first service is switched.
    pub at: u64,
    /// Steps between two services of a rolling upgrade (0: all before step `at`).
    pub gap: u64,
    /// Steps after its own switch at which each service goes back to the version it ran before.
    pub back: Option<u64>,
    /// The version the services are switched to.
    pub version: String,
}

/// One switch of one service, as the host controller applies it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Switch {
    /// The step before which it happens.
    pub step: u64,
    pub service: String,
    /// The version it switches to; None for a rollback (the version the service ran before
    /// its upgrade by the same token).
    pub version: Option<String>,
}

impl Upgrade {
    pub(crate) fn parse(v: &str, what: &str) -> Result<Upgrade, ParseError> {
        let form = "U=<service|list|group.selector>@<at>[/<gap>][+<back>]:<version>";
        let (head, version) = v
            .rsplit_once(':')
            .ok_or_else(|| err(format!("{what}: {form}")))?;
        if !version_name_ok(version) {
            return Err(err(format!(
                "{what}: {version:?} is not a version name (a letter or digit, then letters, \
                 digits, '.', '_' and '-', at most {VERSION_NAME_MAX} bytes)"
            )));
        }
        let (t, when) = head
            .split_once('@')
            .ok_or_else(|| err(format!("{what}: {form}")))?;
        let target = Target::parse(t, what)?;
        let (when, back) = match when.split_once('+') {
            Some((w, b)) => {
                let b = dec(b, what)?;
                if b == 0 {
                    return Err(err(format!(
                        "{what}: a rollback comes at least one step after the upgrade (+1 or more)"
                    )));
                }
                (w, Some(b))
            }
            None => (when, None),
        };
        let (at, gap) = match when.split_once('/') {
            Some((a, g)) => {
                let g = dec(g, what)?;
                if g == 0 {
                    return Err(err(format!(
                        "{what}: write no /0 (the services are switched one after another \
                         before the same step without it)"
                    )));
                }
                if target.service().is_some() {
                    return Err(err(format!(
                        "{what}: /<gap> spaces the services of a list or a group; one service \
                         takes none"
                    )));
                }
                (dec(a, what)?, g)
            }
            None => (dec(when, what)?, 0),
        };
        Ok(Upgrade {
            target,
            at,
            gap,
            back,
            version: version.to_string(),
        })
    }

    /// The switches of the token for the target's services `svcs` (in the target's order):
    /// each service's upgrade, and its rollback with `+<back>`, in the order of their steps
    /// (a service's rollback before the next service's upgrade at the same step).
    pub fn switches(&self, svcs: &[String]) -> Vec<Switch> {
        let mut out = Vec::new();
        for (k, s) in svcs.iter().enumerate() {
            let step = self.at + k as u64 * self.gap;
            out.push(Switch {
                step,
                service: s.clone(),
                version: Some(self.version.clone()),
            });
            if let Some(b) = self.back {
                out.push(Switch {
                    step: step + b,
                    service: s.clone(),
                    version: None,
                });
            }
        }
        // stable: the order above among switches of one step
        out.sort_by_key(|w| w.step);
        out
    }

    /// The last step of the token's switches for a target of `n` services.
    pub fn last_step(&self, n: usize) -> u64 {
        self.at + n.saturating_sub(1) as u64 * self.gap + self.back.unwrap_or(0)
    }
}

impl fmt::Display for Upgrade {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.target, self.at)?;
        if self.gap > 0 {
            write!(f, "/{}", self.gap)?;
        }
        if let Some(b) = self.back {
            write!(f, "+{b}")?;
        }
        write!(f, ":{}", self.version)
    }
}
