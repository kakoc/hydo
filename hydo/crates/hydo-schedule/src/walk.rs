//! `I=`: a walk through a web app's pages that hydo makes itself (Milestone 258,
//! docs/design/ui-exploration.md).
//!
//! `I=<seed>[,n<actions>][,x<step>.<k>[-<last>]]...[,rec[<step>]]`: the walker of `browser.explore`
//! chooses each action of the walk from this seed. Workload step `s` runs `actions` actions (the
//! manifest's `explore.actions` without `n`). Each action draws from its own stream, `(seed, s,
//! k)`, so an action left out with `x<s>.<k>` changes no other action's draw: the shrinker cuts a
//! walk by lowering `n` and by leaving actions out. `x<s>.<k>-<last>` leaves out actions `k` to
//! `last` of step `s` (Milestone 264); a walk prints a run of four or more left-out actions of one
//! step that way, and shorter runs one by one, as Milestone 258 wrote them. `rec` records a video and a Playwright trace of the
//! last step that takes an action, and `rec<step>` of that step (Milestone 267: the step of a
//! failure); recording costs guest time, so a run with `rec` is a run of its own, with its own
//! fingerprint. A walk with `rec` repeats the actions that the same schedule without `rec` takes
//! instead of choosing them again (Milestone 270): the host runs that schedule first, or takes the
//! failing run's console, and hands the walker its actions.

use crate::num::{dec, err, hex};
use crate::ParseError;
use std::collections::BTreeSet;
use std::fmt;

/// The most actions per step a walk takes.
pub const MAX_ACTIONS: u64 = 200;

/// `I=<seed>[,n<actions>][,x<step>.<k>]...[,rec[<step>]]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Walk {
    pub seed: u64,
    /// Actions per workload step; None: the manifest's `explore.actions`.
    pub actions: Option<u64>,
    /// The actions left out: (step, index of the action in its step).
    pub skip: BTreeSet<(u64, u64)>,
    /// Record a video and a Playwright trace of one step.
    pub rec: bool,
    /// The step to record (`rec<step>`); None: the last step that takes an action.
    pub rec_step: Option<u64>,
}

impl Walk {
    pub(crate) fn parse(v: &str, what: &str) -> Result<Walk, ParseError> {
        let form = "I=<seed hex>[,n<actions>][,x<step>.<k>[-<last>]]...[,rec[<step>]]";
        let mut it = v.split(',');
        let seed = hex(it.next().unwrap_or(""), what)
            .map_err(|e| err(format!("{} ({form})", e.message)))?;
        let mut w = Walk {
            seed,
            ..Walk::default()
        };
        for f in it {
            if let Some(s) = f.strip_prefix("rec") {
                if w.rec {
                    return Err(err(format!("{what}: rec twice")));
                }
                w.rec = true;
                if !s.is_empty() {
                    w.rec_step = Some(dec(s, what)?);
                }
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
                let s = dec(s, what)?;
                let (a, b) = match k.split_once('-') {
                    Some((a, b)) => (dec(a, what)?, dec(b, what)?),
                    None => (dec(k, what)?, dec(k, what)?),
                };
                if b < a || b >= MAX_ACTIONS {
                    return Err(err(format!(
                        "{what}: {f}: x<step>.<first>-<last> with first <= last < {MAX_ACTIONS}"
                    )));
                }
                for j in a..=b {
                    if !w.skip.insert((s, j)) {
                        return Err(err(format!("{what}: {f} leaves out {s}.{j} twice")));
                    }
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
        // runs of consecutive actions of one step: four or more as a range
        let mut it = self.skip.iter().peekable();
        while let Some(&(s, a)) = it.next() {
            let mut b = a;
            while it.peek() == Some(&&(s, b + 1)) {
                b += 1;
                it.next();
            }
            if b - a >= 3 {
                write!(f, ",x{s}.{a}-{b}")?;
            } else {
                for k in a..=b {
                    write!(f, ",x{s}.{k}")?;
                }
            }
        }
        if self.rec {
            f.write_str(",rec")?;
            if let Some(s) = self.rec_step {
                write!(f, "{s}")?;
            }
        }
        Ok(())
    }
}
