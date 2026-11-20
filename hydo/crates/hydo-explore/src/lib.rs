#![doc = include_str!("../README.md")]
//!
//! ## Notes for hydo's developers
//!
//! hydo's fault explorer (the port of hydo/explore/explore.py and the modules it imported for the
//! product: manifest_space.py, fc_adaptive.py, grpc_faults.py, hvm_backend.py, and the explorer
//! side of hydo_composer.py).
//!
//! It generates a fault schedule from every seed (a pure function of the seed, the strategy and the
//! options: splitmix64 streams), runs the schedules on a pool of engine workers (each boots the
//! guest once and starts every schedule from a snapshot; [`hydo_engine::Engine`]), keeps every run
//! whose invariant fails, re-runs each failure to confirm it reproduces bit for bit, shrinks one
//! failure per (class, signature) to a minimal failing schedule, and writes its results in the
//! results format (hydo-results: `events.jsonl` as it runs, `exploration.json` at the end;
//! [`results`]) and `report.md`, made from exploration.json ([`md`]).
//!
//! The schedule strings, finding classes, shrink candidates and shrink paths are explore.py's,
//! byte for byte (`tests/identity.rs` against explore.py's answers, `tests/fake_engine.rs` end to
//! end against explore.py's outputs on a scripted engine, both recorded in `tests/golden/` before
//! the Python was removed; docs/HISTORY.md M102, M107). What changed is how runs are
//! scheduled ([`pool`], [`run`]): no batch barriers between the first runs, the re-runs and the
//! shrinks, shrink rounds that stop at their first kept candidate, snapshots in memory when the
//! machine has the room, and the pool sized from the machine.

pub mod advice;
pub mod args;
pub mod compose;
pub mod contracts;
pub mod cover;
pub mod errlog;
pub mod fc;
pub mod gen;
pub mod md;
pub mod merge;
pub mod paths;
pub mod pool;
pub mod py;
pub mod report;
pub mod results;
pub mod rng;
pub mod run;
pub mod sched;
pub mod scripted;
pub mod space;
pub mod state;
pub mod steer;
pub mod turns;

pub use run::{main_with, Drawer, Env};

/// The exit status when the explorer cannot go on (a bad option, an engine that does not start).
/// 1 by default; `hydo ci run` sets 2, its status for a run that could not be completed, so that a
/// CI job never reads an explorer that stopped as a run that failed by the team's policy.
pub static DIE_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);

/// The exit status of an exploration that stopped before it ran any schedule (Milestone 328): it
/// refused (the guest does not fit into the machine's memory, options that do not go together,
/// an image it cannot run) or its engine did not start, so nothing was explored and no report
/// was written. It is kept apart from 0 (an exploration that ran, with or without failures), 1
/// (the explorer stopped after it had run schedules), 2 (a bad option), 3 and 4 (other commands'
/// "nothing judged" and "could not check") and 100 (a check's violation). Inside `hydo ci run`
/// ([`DIE_CODE`] 2) the status stays 2.
pub const REFUSED: i32 = 5;

/// A schedule was handed to the pool: from here on a stop is no refusal.
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// [`main_with`] runs: a stop before the first schedule is a refusal. Other users of the crate
/// (`hydo compare-versions`' drawer, the benchmark) keep [`DIE_CODE`].
pub(crate) static EXPLORING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// A refusal also prints one JSON line on stdout (the end for a program: `--json`, or stdout
/// that is not a terminal), as an exploration that ran ends with its summary line.
pub(crate) static JSON_END: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The pool is about to run a schedule.
pub(crate) fn mark_started() {
    STARTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The JSON line of a refusal: `{"refused": "<why>", "status": 5}`.
pub fn refused_line(why: &str) -> String {
    let mut m = serde_json::Map::new();
    m.insert("refused".into(), serde_json::Value::String(why.to_string()));
    m.insert("status".into(), serde_json::Value::from(REFUSED));
    serde_json::Value::Object(m).to_string()
}

/// The status a stop of the explorer exits with: [`REFUSED`] before the first schedule ran
/// (unless the caller set another [`DIE_CODE`]), else [`DIE_CODE`].
fn stop_code() -> i32 {
    use std::sync::atomic::Ordering::Relaxed;
    let code = DIE_CODE.load(Relaxed);
    if code == 1 && EXPLORING.load(Relaxed) && !STARTED.load(Relaxed) {
        REFUSED
    } else {
        code
    }
}

/// Stop the program with [`stop_code`]; `why` is the line already printed on stderr, which a
/// refusal repeats as its JSON line.
pub(crate) fn exit_die_with(why: &str) -> ! {
    use std::sync::atomic::Ordering::Relaxed;
    let code = stop_code();
    if code == REFUSED && JSON_END.load(Relaxed) {
        println!("{}", refused_line(why.trim()));
    }
    std::process::exit(code)
}

/// Stop the program with [`stop_code`].
pub(crate) fn exit_die() -> ! {
    exit_die_with("the explorer stopped")
}
