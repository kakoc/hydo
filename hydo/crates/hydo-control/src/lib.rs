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
//! - [`lockstep_link`]: [`Link`] over `hydo_engine::lockstep`, with the engine actions of agent
//!   images (the workload's end, the power cut);
//! - [`drive`]: the conversations of one engine run (boot, run, the recovery after a power cut,
//!   a fresh boot's run), by the schedule or by recorded transcripts (step C1, M126);
//! - [`agent`]: agent images on the engine interface ([`agent::AgentEngine`]), which the explorer
//!   and the `hydo` command use for every image whose `/etc/hydo/control` says `agent` (M126).

pub mod agent;
pub mod busybox;
pub mod control;
pub mod drive;
mod flow;
pub mod lockstep_link;
pub mod names;
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
pub use runner::{
    run, run_with, AgentPoint, Finished, Link, LinkError, LinkNext, RunError, RunOpts, Start,
};
pub use schedule::{Conversation, GuestState, RecoveryInputs, ScheduleController};
pub use seams::{Seam, Unsupported};
pub use transcript::{Header, Record, Transcript};
