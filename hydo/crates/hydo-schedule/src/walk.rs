//! `I=`: a walk through a web app's pages that hydo makes itself (Milestone 258,
//! docs/design/ui-exploration.md).
//!
//! `I=<seed>[,n<actions>][,x<step>.<k>]...[,rec]`: the walker of `browser.explore` chooses each
//! action of the walk from this seed. Workload step `s` runs `actions` actions (the manifest's
//! `explore.actions` without `n`). Each action draws from its own stream, `(seed, s, k)`, so an
//! action left out with `x<s>.<k>` changes no other action's draw: the shrinker cuts a walk by
//! lowering `n` and by leaving actions out. `rec` records a video and a Playwright trace of the
//! last step; recording costs guest time, so a run with `rec` is a run of its own, with its own
//! fingerprint.

use crate::num::{dec, err, hex};
use crate::ParseError;
use std::collections::BTreeSet;
use std::fmt;

/// The most actions per step a walk takes.
pub const MAX_ACTIONS: u64 = 200;

/// `I=<seed>[,n<actions>][,x<step>.<k>]...[,rec]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Walk {
    pub seed: u64,
    /// Actions per workload step; None: the manifest's `explore.actions`.
    pub actions: Option<u64>,
    /// The actions left out: (step, index of the action in its step).
    pub skip: BTreeSet<(u64, u64)>,
    /// Record a video and a Playwright trace of the last step.
    pub rec: bool,
}

impl Walk {
    pub(crate) fn parse(v: &str, what: &str) -> Result<Walk, ParseError> {
        let form = "I=<seed hex>[,n<actions>][,x<step>.<k>]...[,rec]";
        let mut it = v.split(',');
        let seed = hex(it.next().unwrap_or(""), what)
            .map_err(|e| err(format!("{} ({form})", e.message)))?;
        let mut w = Walk {
            seed,
            ..Walk::default()
        };
        for f in it {
            if f == "rec" {
                if w.rec {
                    return Err(err(format!("{what}: rec twice")));
                }
                w.rec = true;
            } else if let Some(n) = f.strip_prefix('n') {
                if w.actions.is_some() {
                    return Err(err(format!("{what}: n twice")));
                }
                let n = dec(n, what)?;
                if n == 0 || n > MAX_ACTIONS {
                    return Err(err(format!(
                        "{what}: n<actions> is 1 to {MAX_ACTIONS} actions per step"
                    )));
                }
                w.actions = Some(n);
            } else if let Some(x) = f.strip_prefix('x') {
                let (s, k) = x
                    .split_once('.')
                    .ok_or_else(|| err(format!("{what}: x<step>.<k> ({form})")))?;
                if !w.skip.insert((dec(s, what)?, dec(k, what)?)) {
                    return Err(err(format!("{what}: {f} twice")));
                }
            } else {
                return Err(err(format!("{what}: field {f:?} ({form})")));
            }
        }
        Ok(w)
    }

    /// Whether action `k` of step `s` is taken.
    pub fn takes(&self, s: u64, k: u64) -> bool {
        !self.skip.contains(&(s, k))
    }
}

impl fmt::Display for Walk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}", self.seed)?;
        if let Some(n) = self.actions {
            write!(f, ",n{n}")?;
        }
        for (s, k) in &self.skip {
            write!(f, ",x{s}.{k}")?;
        }
        if self.rec {
            f.write_str(",rec")?;
        }
        Ok(())
    }
}
