#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

mod assertion;
mod buggify;
mod eventually;
pub mod history;
mod json;
mod macros;
mod sink;
mod steer;

pub use assertion::{always, always_or_unreachable, catalog, reachable, register, setup_complete, sometimes, unreachable, Assertion};
pub use buggify::{buggify, Point};
pub use eventually::{eventually, Liveness, Watch};
pub use json::{Details, Value};
pub use steer::{maximize, minimize, Steer};

// Only the macros use these; they are public because a macro expands in the user's crate.
#[doc(hidden)]
pub use assertion::{Type, __register_static};

/// The Go SDK's `D`: a details object.
pub type D = Details;
