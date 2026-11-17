#![doc = include_str!("../README.md")]
//!
//! ## Notes for hydo's developers
//!
//! Function contracts (Milestone 241, docs/design/contracts.md): a developer writes
//! `// hydo: requires ...` and `// hydo: ensures ...` next to a function, and hydo checks them
//! at every call while the whole system runs deterministically under faults.
//!
//! - [`lang`]: the comments and their expressions.
//! - [`compile`] (feature `compile`): with the program's debug information, the plan: each
//!   function's entry address and the reads at entry and at return.
//! - [`plan`]: the plan an engine reads (`<image>.contracts.json`), and the records it writes
//!   (`<tag>.contracts.jsonl`).
//! - [`eval`]: the records judged on the host after the run.
//! - [`fuzz`]: one function at a time, tested natively with inputs generated from its
//!   `requires` and `input` lines and judged by its `ensures` (Milestone 275,
//!   docs/design/function-fuzzing.md).

#[cfg(feature = "compile")]
pub mod compile;
pub mod eval;
pub mod fuzz;
pub mod lang;
pub mod plan;
pub mod values;
