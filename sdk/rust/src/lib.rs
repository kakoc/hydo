//! hydo's assertion SDK for Rust guest programs: the way a program running under hydo (hvx or
//! the Mac VMM) describes its invariants. The same API, wire format and deduplication rules
//! as the Go, Python and JavaScript SDKs (`sdk/go`, `sdk/python`, `sdk/js`); the contract is
//! `hydo/ASSERTIONS.md`. This crate is the reference: the shared cases in `sdk/cases` are
//! written by its tests.
//!
//! ```ignore
//! use hydo_assert::{declare, details, hydo_always, hydo_always_cmp, hydo_buggify, hydo_reachable};
//! use std::time::Duration;
//!
//! declare! {
//!     static ACKED: always("kv.acked_durable", "every acknowledged write reads back with its value");
//!     static BACK: eventually("kv.back", "the server answers within 5 s of its start", Duration::from_secs(5));
//! }
//!
//! fn main() {
//!     hydo_assert::catalog();                          // once, early: the HYDO-ASSERT-DECL lines
//!     ACKED.check(got == want, details!{"key": k, "got": got});
//!     hydo_always!(n <= cap, "the log never exceeds its cap", {"n": n});
//!     hydo_always_cmp!(n, "<=", cap, "kv.log_size" => "the log stays under its cap");
//!     hydo_reachable!("kv.recovered" => "the server replayed a non-empty log");
//!     if hydo_buggify!("kv.put.lost_reply" => "the write lands but the reply is lost") { return; }
//!     hydo_assert::setup_complete();
//! }
//! ```
//!
//! Records go straight to `/dev/ttyS0` (override with `HYDO_ASSERT_OUT=<path>|stderr|off`),
//! so they reach the console even when stdout is redirected. With the `noop` feature
//! everything compiles to nothing and every BUGGIFY point returns false.
//!
//! Determinism: a process writes each (assertion, outcome) pair once (the first pass and the
//! first failure, with that evaluation's details) and never writes a sometimes that evaluated
//! false; details are JSON objects with sorted keys, encoded byte-for-byte as Go's
//! `encoding/json` encodes them; no clocks except in `eventually` (the guest's monotonic
//! clock, deterministic under hydo).

mod assertion;
mod buggify;
mod eventually;
pub mod history;
mod json;
mod macros;
mod sink;

pub use assertion::{always, always_or_unreachable, catalog, reachable, register, setup_complete, sometimes, unreachable, Assertion};
pub use buggify::{buggify, Point};
pub use eventually::{eventually, Liveness, Watch};
pub use json::{Details, Value};

// Only the macros use these; they are public because a macro expands in the user's crate.
#[doc(hidden)]
pub use assertion::{Type, __register_static};

/// The Go SDK's `D`: a details object.
pub type D = Details;
