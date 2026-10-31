//! hydo-control: the host side of guest control (docs/design/guest-control.md; docs/HISTORY.md
//! M120, step B1). In an agent image the guest's first process, `hydo-agent`, only carries out
//! commands; every decision is made here, on the host, as a pure function of the schedule, its
//! seeds and the events seen so far.
//!
//! - [`control`]: the [`Controller`] trait and what it sees and does at a numbered stop;
//! - [`schedule`]: [`ScheduleController`], which runs today's schedule language over the core
//!   project model ([`plan`]) by doing what `hydo/onboard/hydo-init.sh` does, in its order;
//! - [`seams`]: the init extensions and fault kinds not driven yet, one seam each (wave 3);
//! - [`transcript`]: the control transcript (every command and engine action by stop number,
//!   with the cadence) and its hash;
//! - [`replay`]: [`ReplayController`], which types a recorded transcript again;
//! - [`runner`]: one conversation over an engine ([`Link`]): the agent session, the
//!   acknowledgements, the transcript;
//! - [`lockstep_link`]: [`Link`] over `hydo_engine::lockstep` (for step C1).
//!
//! Nothing in the product calls this crate yet (step C1 does); its proof is the scripted agent
//! and engine of `tests/support/` (HANDOFF.md).

pub mod control;
mod flow;
pub mod lockstep_link;
pub mod plan;
pub mod replay;
pub mod runner;
pub mod schedule;
pub mod seams;
pub mod transcript;

pub use control::{
    Action, Controller, Ending, EngineAction, Out, Seen, StopInfo, VTime, When, Why,
};
pub use plan::{Layout, Plan};
pub use replay::ReplayController;
pub use runner::{run, AgentPoint, Finished, Link, LinkError, LinkNext, RunError, Start};
pub use schedule::{Conversation, GuestState, ScheduleController};
pub use seams::{Seam, Unsupported};
pub use transcript::{Header, Record, Transcript};
