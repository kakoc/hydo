//! hydo's fault explorer (the port of hydo/explore/explore.py and the modules it imported for the
//! product: manifest_space.py, fc_adaptive.py, grpc_faults.py, hvm_backend.py, and the explorer
//! side of hydo_composer.py).
//!
//! It generates a fault schedule from every seed (a pure function of the seed, the strategy and the
//! options: splitmix64 streams), runs the schedules on a pool of engine workers (each boots the
//! guest once and starts every schedule from a snapshot; [`hydo_engine::Engine`]), keeps every run
//! whose invariant fails, re-runs each failure to confirm it reproduces bit for bit, shrinks one
//! failure per (class, signature) to a minimal failing schedule, and writes `report.md`,
//! `report.json`, `runs.jsonl` and `shrink.jsonl` (and `errors.json` for images with error logs).
//!
//! The schedule strings, finding classes, shrink candidates and shrink paths are explore.py's,
//! byte for byte (`tests/identity.rs` against explore.py's answers, `tests/fake_engine.rs` end to
//! end against explore.py's outputs on a scripted engine, both recorded in `tests/golden/` before
//! the Python was removed; docs/HISTORY.md M102, M107). What changed is how runs are
//! scheduled ([`pool`], [`run`]): no batch barriers between the first runs, the re-runs and the
//! shrinks, shrink rounds that stop at their first kept candidate, snapshots in memory when the
//! machine has the room, and the pool sized from the machine.

pub mod args;
pub mod compose;
pub mod errlog;
pub mod fc;
pub mod gen;
pub mod md;
pub mod pool;
pub mod py;
pub mod report;
pub mod results;
pub mod rng;
pub mod run;
pub mod sched;
pub mod space;

pub use run::{main_with, Env};
