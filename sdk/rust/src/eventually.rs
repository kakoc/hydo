//! Eventually: bounded liveness. "After an anchor point, the condition becomes true within a
//! deadline." hvx makes guest time deterministic, so a deadline measured on the guest's
//! monotonic clock (`Instant`, `CLOCK_MONOTONIC` -- the clock Go's `time.Since` reads) gives
//! the same verdict and the same elapsed time on every run of a schedule.
//!
//! One evaluation record per episode, with `elapsed_ms` and `within_ms` in the details:
//! `ok:true` when the condition became true by the deadline, `ok:false` when the deadline
//! passed first. As for the other types, each outcome is written at most once per process.

use crate::assertion::{Assertion, Loc, Type};
use crate::json::Details;
use std::borrow::Cow;
use std::future::Future;
use std::time::{Duration, Instant};

/// A declared eventually property.
pub struct Liveness {
    pub(crate) a: Assertion,
    within: Duration,
}

impl std::fmt::Debug for Liveness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Liveness").field("a", &self.a).field("within", &self.within).finish()
    }
}

impl Liveness {
    /// A `static` eventually property (in the catalog once registered; see
    /// [`declare!`](crate::declare)).
    pub const fn new(id: &'static str, msg: &'static str, within: Duration) -> Self {
        Liveness { a: Assertion::raw(Type::Eventually, Cow::Borrowed(id), Cow::Borrowed(msg), None, None), within }
    }
    #[doc(hidden)]
    pub const fn __new(id: &'static str, msg: &'static str, within: Duration, file: &'static str, line: u32, col: u32, wire: bool) -> Self {
        Liveness { a: Assertion::raw(Type::Eventually, Cow::Borrowed(id), Cow::Borrowed(msg), Some(Loc { file, line, col, wire }), None), within }
    }
    #[doc(hidden)]
    pub fn __assertion(&'static self) -> &'static Assertion {
        &self.a
    }
    pub fn within(&self) -> Duration {
        self.within
    }

    /// Starts an episode now (the anchor).
    pub fn begin(&'static self) -> Watch {
        Watch { l: self, start: Instant::now(), done: false }
    }

    /// Starts an episode now, polls `cond` every `poll` until it is true or the deadline has
    /// passed, records the outcome and returns it. Blocks the thread (use
    /// [`await_async`](Self::await_async) in async code). The last poll happens at or after
    /// the deadline, so a condition that becomes true exactly at the deadline still counts.
    pub fn await_cond(&'static self, mut cond: impl FnMut() -> bool, poll: Duration, details: impl Into<Details>) -> bool {
        let d = details.into();
        let mut w = self.begin();
        loop {
            if cond() {
                return w.done(d);
            }
            if w.start.elapsed() > self.within {
                w.expire(d);
                return false;
            }
            std::thread::sleep(poll);
        }
    }

    /// [`await_cond`](Self::await_cond) for async code, independent of the runtime: `cond`
    /// returns a future of the check, `sleep` returns a future that sleeps for the given
    /// time (with tokio: `|d| tokio::time::sleep(d)`).
    pub async fn await_async<C, CF, S, SF>(&'static self, mut cond: C, poll: Duration, mut sleep: S, details: impl Into<Details>) -> bool
    where
        C: FnMut() -> CF,
        CF: Future<Output = bool>,
        S: FnMut(Duration) -> SF,
        SF: Future<Output = ()>,
    {
        let d = details.into();
        let mut w = self.begin();
        loop {
            if cond().await {
                return w.done(d);
            }
            if w.start.elapsed() > self.within {
                w.expire(d);
                return false;
            }
            sleep(poll).await;
        }
    }
}

/// One episode of a [`Liveness`] property, started at its anchor.
#[derive(Debug)]
pub struct Watch {
    l: &'static Liveness,
    start: Instant,
    done: bool,
}

impl Watch {
    fn finish(&mut self, ok: bool, details: Details) -> bool {
        if self.done {
            return ok;
        }
        self.done = true;
        let mut d = details;
        d.insert("elapsed_ms", self.start.elapsed().as_millis() as i64);
        d.insert("within_ms", self.l.within.as_millis() as i64);
        self.l.a.record(ok, d);
        ok
    }

    /// Records that the condition became true now: ok if within the deadline, else a
    /// violation (true, but too late). Returns whether it was in time.
    pub fn done(&mut self, details: impl Into<Details>) -> bool {
        let ok = self.start.elapsed() <= self.l.within;
        self.finish(ok, details.into())
    }

    /// Records that the deadline passed with the condition still false (a violation).
    pub fn expire(&mut self, details: impl Into<Details>) {
        self.finish(false, details.into());
    }

    /// Time since the anchor.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Declares an eventually property at run time (registered now, in the catalog in this order).
pub fn eventually(id: impl Into<Cow<'static, str>>, msg: impl Into<Cow<'static, str>>, within: Duration) -> &'static Liveness {
    let l: &'static Liveness = Box::leak(Box::new(Liveness { a: Assertion::raw(Type::Eventually, id.into(), msg.into(), None, None), within }));
    crate::assertion::register(&l.a);
    l
}
