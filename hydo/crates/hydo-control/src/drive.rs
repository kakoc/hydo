//! Agent images end to end (docs/design/guest-control.md step C1; docs/HISTORY.md M126): the
//! conversations of one engine run, in order, over one [`Link`].
//!
//! - [`boot`]: a fresh boot to S1 ([`ScheduleController::boot`]); the host takes S0 and S1.
//! - [`run`]: one schedule from S1 ([`ScheduleController::run`], or a recorded transcript typed
//!   again, [`Policy::Replay`]); after a power cut — the controller's ([`Ending::PowerCut`],
//!   the engine's `CUT`) or the engine's (`cf=`: the workload's end becomes the cut,
//!   [`Link::engine_cut`]) — the recovery conversation from the host's S0 on the same link (the
//!   engine's next stop is the recovery VM's first). The acknowledged and failed ids go to the
//!   recovery controller as commands, so no recovery command line exists any more.
//! - [`one`]: a fresh boot whose run goes on at S1's own stop (the engine's `--control boot one`),
//!   so that its stops, input and fingerprint are a worker's CTL run of the same schedule.
//!
//! [`check`] says before a run whether the controller can drive a schedule on a plan: what waits
//! for wave 3 (the seams) is refused with a message that names it.

use crate::control::Ending;
use crate::plan::Plan;
use crate::replay::ReplayController;
use crate::runner::{self, AgentPoint, Finished, Link, RunError, RunOpts, Start};
use crate::schedule::{GuestState, RecoveryInputs, ScheduleController};
use crate::seams::Unsupported;
use crate::transcript::{Header, Transcript};
use hydo_schedule::Schedule;
use std::fmt;

/// Why an agent run could not be carried out.
#[derive(Debug)]
pub enum DriveError {
    /// The controller cannot drive this (a wave-3 seam, or something agent images cannot do).
    Unsupported(Unsupported),
    /// The conversation broke (the engine, or the protocol).
    Run(RunError),
    /// The boot did not reach S1: the boot conversation's verdict (an ERROR of the image).
    Boot(String, Box<Finished>),
}

impl fmt::Display for DriveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriveError::Unsupported(u) => write!(f, "{u}"),
            DriveError::Run(e) => write!(f, "{e}"),
            DriveError::Boot(v, _) => write!(f, "the boot did not reach S1: {v}"),
        }
    }
}

impl std::error::Error for DriveError {}

impl From<RunError> for DriveError {
    fn from(e: RunError) -> DriveError {
        DriveError::Run(e)
    }
}

impl From<Unsupported> for DriveError {
    fn from(e: Unsupported) -> DriveError {
        DriveError::Unsupported(e)
    }
}

/// The transcript header of a conversation (the controller's name is the runner's).
pub fn header(conversation: &str, schedule: Option<&Schedule>, start: &str) -> Header {
    Header {
        controller: String::new(),
        conversation: conversation.into(),
        schedule: schedule.map(|s| s.to_string()),
        cadence: runner::cadence_of(schedule),
        start: start.into(),
    }
}

/// A boot to S1: what the runs from it need.
#[derive(Clone, Debug)]
pub struct Booted {
    pub plan: Plan,
    pub s0: (AgentPoint, GuestState),
    pub s1: (AgentPoint, GuestState),
    /// The boot conversation.
    pub boot: Finished,
}

/// Whether the controller can drive `schedule` on `plan` (an agent image): the guest tokens it
/// reads, and the recovery boot after a power cut. A refusal names the seam.
pub fn check(plan: &Plan, schedule: &Schedule) -> Result<(), Unsupported> {
    let g = GuestState::default();
    ScheduleController::run(plan.clone(), schedule, &g)?;
    if schedule.has_power_cut() {
        ScheduleController::recovery(plan.clone(), Vec::new(), Vec::new(), &g)?;
    }
    Ok(())
}

/// The boot conversation from the agent's hello to S1. `boot_schedule` gives the cadence (its
/// `rb=`). With `opts.end_at_snapshot` the engine's boot ends at S1 (END; a serve worker then
/// prints READY); without it the engine run goes on at S1's stop (`one`).
pub fn boot(
    link: &mut dyn Link,
    plan: &Plan,
    boot_schedule: &Schedule,
    opts: RunOpts,
) -> Result<Booted, DriveError> {
    let mut ctl = ScheduleController::boot(plan.clone())?;
    let fin = runner::run_with(
        link,
        &mut ctl,
        Start::Boot,
        header("boot", Some(boot_schedule), "boot"),
        opts,
    )?;
    if fin.ending != Some(Ending::AtSnapshot("S1".into())) {
        let why = match &fin.ending {
            Some(Ending::Verdict(v)) => v.clone(),
            Some(e) => format!("{e:?}"),
            None => format!("the engine ended the boot: {}", fin.result),
        };
        return Err(DriveError::Boot(why, Box::new(fin)));
    }
    let point = |name: &str| -> Result<(AgentPoint, GuestState), DriveError> {
        let p = fin
            .snapshots
            .iter()
            .rev()
            .find(|p| p.snapshot == name)
            .cloned();
        let g = ctl.snapshots().get(name).cloned();
        match (p, g) {
            (Some(p), Some(g)) => Ok((p, g)),
            _ => Err(DriveError::Run(RunError::Protocol(format!(
                "the boot took no snapshot {name}"
            )))),
        }
    };
    let (s0, s1) = (point("S0")?, point("S1")?);
    Ok(Booted {
        plan: plan.clone(),
        s0,
        s1,
        boot: fin,
    })
}

/// Who decides in a run: the schedule's controller, or recorded transcripts typed again
/// (`hydo repro --transcript`).
#[derive(Clone, Debug)]
pub enum Policy {
    Schedule,
    Replay {
        run: Box<Transcript>,
        /// The recovery boot's transcript; without one a recovery runs by the schedule's policy.
        recovery: Option<Box<Transcript>>,
    },
}

/// A run's conversations.
#[derive(Clone, Debug)]
pub struct Ran {
    pub run: Finished,
    pub recovery: Option<Finished>,
}

impl Ran {
    /// The engine's RESULT line of the run (the last conversation's).
    pub fn result_line(&self) -> &str {
        match &self.recovery {
            Some(r) => &r.result,
            None => &self.run.result,
        }
    }

    /// The run's control hash (`ctl=`): the run conversation's transcript hash, or with a recovery
    /// boot the first 16 hex digits of SHA-256 over both hashes ("<run> <recovery>").
    pub fn ctl(&self) -> String {
        ctl_of(
            &self.run.transcript,
            self.recovery.as_ref().map(|r| &r.transcript),
        )
    }
}

/// `ctl=` of a run's transcripts (see [`Ran::ctl`]).
pub fn ctl_of(run: &Transcript, recovery: Option<&Transcript>) -> String {
    use sha2::{Digest, Sha256};
    match recovery {
        None => run.hash(),
        Some(r) => Sha256::digest(format!("{} {}", run.hash(), r.hash()).as_bytes())[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    }
}

/// Where a run conversation starts.
#[derive(Clone, Copy, Debug)]
pub enum RunFrom {
    /// A worker's CTL run: the engine restored S1 (`why=resume`).
    Restored,
    /// A fresh boot's run, at the stop where the boot took S1 ([`one`]).
    Here(crate::control::StopInfo),
}

/// One schedule from S1, and after a power cut its recovery boot from S0, over the same link.
pub fn run(
    link: &mut dyn Link,
    b: &Booted,
    schedule: &Schedule,
    from: RunFrom,
    policy: &Policy,
) -> Result<Ran, DriveError> {
    let start = match from {
        RunFrom::Restored => Start::Resume(b.s1.0.clone()),
        RunFrom::Here(stop) => Start::Here {
            point: b.s1.0.clone(),
            stop,
        },
    };
    let h = header("run", Some(schedule), "resume S1");
    let run = match policy {
        Policy::Schedule => {
            let mut ctl = ScheduleController::run(b.plan.clone(), schedule, &b.s1.1)?;
            runner::run(link, &mut ctl, start, h)?
        }
        Policy::Replay { run, .. } => {
            let mut ctl = ReplayController::new(run);
            runner::run(link, &mut ctl, start, h)?
        }
    };
    let recovery = match &run.ending {
        Some(Ending::PowerCut {
            acked,
            failed,
            world,
        }) => {
            let h = header("recovery", Some(schedule), "resume S0");
            let start = Start::Resume(b.s0.0.clone());
            Some(match policy {
                Policy::Replay {
                    recovery: Some(t), ..
                } => runner::run(link, &mut ReplayController::new(t), start, h)?,
                _ => {
                    // the ids, the fake world and the first boot's clock (M122), from the run's
                    // transcript, so a run typed again from its transcript hands over the same
                    let inputs = RecoveryInputs::from_transcript(&run.transcript).unwrap_or(
                        RecoveryInputs {
                            acked: acked.clone(),
                            failed: failed.clone(),
                            clock: None,
                            world: world.clone(),
                        },
                    );
                    let mut ctl =
                        ScheduleController::recovery_from(b.plan.clone(), &inputs, &b.s0.1)?;
                    runner::run(link, &mut ctl, start, h)?
                }
            })
        }
        _ => None,
    };
    Ok(Ran { run, recovery })
}

/// A fresh boot and one schedule (the engine's `--control boot one`): the boot conversation to
/// S1, then the run at that same stop.
pub fn one(
    link: &mut dyn Link,
    plan: &Plan,
    schedule: &Schedule,
    policy: &Policy,
) -> Result<(Booted, Ran), DriveError> {
    let b = boot(
        link,
        plan,
        schedule,
        RunOpts {
            end_at_snapshot: false,
        },
    )?;
    let stop = b.boot.last_stop.ok_or_else(|| {
        DriveError::Run(RunError::Protocol("the boot ended without a stop".into()))
    })?;
    let ran = run(link, &b, schedule, RunFrom::Here(stop), policy)?;
    Ok((b, ran))
}
