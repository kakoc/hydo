//! hydo's assertion SDK for Rust guest programs: the way a program running under hydo (hvx or
//! the Mac VMM) describes its invariants. Same API surface,
//! same wire format and same deduplication rules as the Go SDK (`hvx/stack/hydoassert`); the
//! contract is `hvx/ASSERTIONS.md`.
//!
//! ```ignore
//! use hydo_assert::{declare, details, hydo_always, hydo_buggify, hydo_reachable};
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
mod json;
mod macros;
mod sink;

pub use assertion::{
    always, always_or_unreachable, catalog, emit_line, reachable, register, setup_complete, sometimes, unreachable, Assertion, Type,
};
pub use buggify::{buggify, Point};
pub use eventually::{eventually, Liveness, Watch};
pub use json::{Details, Value};

#[doc(hidden)]
pub use assertion::__register_static;

/// The Go SDK's `D`: a details object.
pub type D = Details;
