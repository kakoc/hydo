//! The bug benchmark runner (the port of hydo/bench/run.py, confirm.py, listrun.py, depth1map.py,
//! d7prob.py and addbuggify.py, and of hydo/explore/archive/bench_more_faults.py; M108).
//!
//! The benchmark (hydo/bench/README.md, M54-M72) plants one defect at a time (d0-d12) into a copy
//! of the order service and measures how many generated schedules a strategy needs before one of
//! them shows the defect. A detection is a run whose schedule keeps every store durable on an
//! honest disk (finding class `durable-defaults`) and whose verdict is a VIOLATION, and which the
//! unpatched control image passes with the same schedule. It uses the explorer
//! ([`hydo_explore`]) as a library: its schedule generators, its parser and printer, its finding
//! classes and its worker pool over the hvx engine, so a seed gives the schedule string that
//! explore.py and run.py gave it, and a run gives the engine's verdict and fingerprint.
//!
//! What it does, in subcommands of `hydo-bench`:
//! - `run`: cells of strategy x defect x repetitions with early stop, the control check of every
//!   candidate, and a re-run of each detection (run.py; `--more-faults --cf-range` adds the timed
//!   kills and k-th-flush cuts of bench_more_faults.py); `table` prints the detection table.
//! - `confirm`: every defect's trigger schedules twice on the defect image and once on its control.
//! - `list`: a fixed list of schedules on one or more images (listrun.py).
//! - `depth1map`: the trigger map of a defect from two depth-1 sweeps (no VM runs).
//! - `d7prob`: how often the generators draw d7's measured trigger set (no VM runs).
//! - `addbuggify`: route an order service's store calls through the BUGGIFY wrappers (a build step
//!   of build-order-services.sh; of build-bench-bg.sh until M146).
//!
//! The output files (`results.jsonl`, `runs-rep<r>.jsonl`, `confirm.jsonl`, `<image>.jsonl`) and
//! the printed lines are the Python tools', field for field (json.dumps' separators and key order).

pub mod analysis;
pub mod buggify;
pub mod cells;
pub mod cli;
pub mod confirm;
pub mod defects;
pub mod listrun;
pub mod pools;

pub use cli::main_with;
