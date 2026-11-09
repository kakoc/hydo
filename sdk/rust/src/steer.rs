//! Steered values: a number that says how close a run came to failing, for `hydo explore
//! --steer <id>` (hydo's docs/design/steered-search.md).
//!
//! The program reports the number wherever it knows it, as often as it likes. The SDK writes a
//! `HYDO-STEER` record the first time and then only when the number is better than every
//! number this id reported before in the process (smaller for [`minimize`], larger for
//! [`maximize`]), at most [`MAX_RECORDS`] records per id, so the console stays small. The
//! explorer takes the best value of each run.

use crate::json::write_str;
use crate::sink;
use std::borrow::Cow;
use std::sync::Mutex;

/// The most records one id writes in one process.
pub const MAX_RECORDS: u32 = 64;

/// Which way is closer to failing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Goal {
    Min,
    Max,
}

#[derive(Debug, Default)]
struct State {
    best: Option<i64>,
    written: u32,
}

/// A declared steered value. Create it with [`minimize`] or [`maximize`], or as a `static` with
/// [`Steer::minimize`] and [`Steer::maximize`].
#[derive(Debug)]
pub struct Steer {
    id: Cow<'static, str>,
    msg: Cow<'static, str>,
    goal: Goal,
    state: Mutex<State>,
}

impl Steer {
    /// A value that is closer to failing when it is smaller (`static` form).
    pub const fn minimize(id: &'static str, msg: &'static str) -> Self {
        Steer { id: Cow::Borrowed(id), msg: Cow::Borrowed(msg), goal: Goal::Min, state: Mutex::new(State { best: None, written: 0 }) }
    }
    /// A value that is closer to failing when it is larger (`static` form).
    pub const fn maximize(id: &'static str, msg: &'static str) -> Self {
        Steer { id: Cow::Borrowed(id), msg: Cow::Borrowed(msg), goal: Goal::Max, state: Mutex::new(State { best: None, written: 0 }) }
    }

    /// The value's id, as its records carry it.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Reports the value now. The SDK writes a record for the first value and for one better
    /// than every earlier value of this id, within [`MAX_RECORDS`].
    pub fn value(&self, v: i64) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let better = match (st.best, self.goal) {
            (None, _) => true,
            (Some(b), Goal::Min) => v < b,
            (Some(b), Goal::Max) => v > b,
        };
        if !better || st.written >= MAX_RECORDS {
            return;
        }
        st.best = Some(v);
        st.written += 1;
        if !sink::active() {
            return;
        }
        let mut s = String::from("HYDO-STEER {\"id\":");
        write_str(&mut s, &self.id);
        s.push_str(if self.goal == Goal::Min { ",\"goal\":\"min\",\"v\":" } else { ",\"goal\":\"max\",\"v\":" });
        s.push_str(&v.to_string());
        s.push_str(",\"msg\":");
        write_str(&mut s, &self.msg);
        s.push_str("}\n");
        sink::write_line(s.as_bytes());
    }
}

/// Declares, at run time, a value that is closer to failing when it is smaller (a queue that
/// must not be empty, a distance to a deadline).
pub fn minimize(id: impl Into<Cow<'static, str>>, msg: impl Into<Cow<'static, str>>) -> &'static Steer {
    Box::leak(Box::new(Steer { id: id.into(), msg: msg.into(), goal: Goal::Min, state: Mutex::new(State::default()) }))
}

/// Declares, at run time, a value that is closer to failing when it is larger (a lag, a count
/// of retries).
pub fn maximize(id: impl Into<Cow<'static, str>>, msg: impl Into<Cow<'static, str>>) -> &'static Steer {
    Box::leak(Box::new(Steer { id: id.into(), msg: msg.into(), goal: Goal::Max, state: Mutex::new(State::default()) }))
}
