//! hydo-agent-proto: the protocol between hydo's host and `hydo-agent`, the guest's first process
//! (docs/design/guest-control.md, "The protocol"). The types here, the schema generated from them
//! (`hydo/schema/agent-v1.json`) and the example transcripts (`hydo/schema/examples/agent/v1/`)
//! are its specification.
//!
//! - [`json`]: the one encoding (canonical JSON, integers only), written without dependencies so
//!   that the guest agent (std and libc only) and the host share it;
//! - [`frame`]: the agent's console framing `HYDO-AG <len> <crc32> <json>`;
//! - [`msg`]: the commands, replies and events of version 1;
//! - [`line`]: the envelopes (host lines, agent lines), strict for the agent, lenient for the host;
//! - [`session`]: the host side of a conversation, with the version rule;
//! - [`schema`]: the generated JSON Schema;
//! - [`transcript`]: the example transcript format.
//!
//! README.md has a short tour; wave 2's controller uses [`session::Session`] and the types.

mod field;

pub mod frame;
pub mod json;
pub mod line;
pub mod msg;
pub mod schema;
pub mod session;
pub mod transcript;

pub use field::FieldError;
pub use line::{AgentLine, ErrorLine, HostLine, Refusal};
pub use msg::{Command, Event, Reply, HOST_PROTOCOLS, PROTO};
pub use session::{Incoming, Outgoing, Session, SessionError};
