//! CLOCK: the realtime clock steps of `T=` (hydo-init.sh's T= branch of explore_run) and
//! `guest.clock_after_cut` (hydo/onboard/hydo-init-clock.sh), driven from the host.
//!
//! - `T=<at>:<delta>` before step `at`: read the clock (`date +%s`), set it to that value plus
//!   delta (`date -s @...`), read it again, log the jump, fail the run when the clock moved by
//!   more than 5 s off the request, and print the fault.clock_back or fault.clock_fwd record.
//!   The second read is the `clock_set` reply's time, which the agent reads right after setting
//!   the clock; when the set is refused, a `clock_get` reads it, as the shell's `date +%s` would.
//! - `guest.clock_after_cut`: at a power cut the first boot reads its clock after failed_ids
//!   (hydo-init-clock.sh appends `clock=<epoch>` to the acknowledged ids, so the acked line shows
//!   it); the recovery boot sets its clock to that epoch plus clock_after_cut seconds before it
//!   mounts the data disk. The epoch reaches the recovery conversation as a value
//!   ([`super::RecoveryInputs::clock`]), not as a token among the ids, so the controller does not
//!   write it into the guest's acked file (AMBIGUOUS, HANDOFF.md decision 21).
//!
//! Everything for clock_after_cut does nothing for a plan without it, because hydo-build splices
//! hydo-init-clock.sh only into such images. T= is part of hydo-init.sh itself.

use super::{call, die_oracle, log, utc, xassert, Ctx, Step, F};
use hydo_agent_proto::msg::{ClockGet, ClockSet, Command, Reply};

/// `$BB date +%s` (None when the agent cannot read the clock: the shell's empty output).
fn date_now(fx: &mut F<'_>) -> Step<Option<i64>> {
    Ok(match call(fx, Command::ClockGet(ClockGet {}))? {
        Ok(Reply::ClockGet(c)) => Some(c.sec),
        _ => None,
    })
}

fn text(v: Option<i64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_default()
}

/// The T= branch of explore_run before step `p`.
pub(super) fn clock_jump(fx: &mut F<'_>, p: i64, delta: i64) -> Step<()> {
    let tnow = date_now(fx)?;
    let set = call(
        fx,
        Command::ClockSet(ClockSet {
            sec: Some(tnow.unwrap_or(0) + delta),
            step: None,
        }),
    )?;
    let tnew = match set {
        Ok(Reply::ClockSet(c)) => Some(c.sec),
        _ => {
            log(fx, "date -s failed");
            date_now(fx)?
        }
    };
    log(
        fx,
        format!(
            "CLOCK JUMP of {delta}s before step {p} (realtime {} -> {})",
            text(tnow),
            text(tnew)
        ),
    );
    let tdiff = tnew.unwrap_or(0) - tnow.unwrap_or(0) - delta;
    if !(-5..=5).contains(&tdiff) {
        return die_oracle(
            fx,
            format!(
                "HYDO-ORACLE: ERROR clock jump not applied ({} -> {}, wanted {delta})",
                text(tnow),
                text(tnew)
            ),
        );
    }
    if delta < 0 {
        xassert(
            fx,
            "reachable",
            "fault.clock_back",
            "the realtime clock was stepped backwards",
        );
    } else {
        xassert(
            fx,
            "reachable",
            "fault.clock_fwd",
            "the realtime clock was stepped forwards",
        );
    }
    Ok(())
}

/// hydo-init-clock.sh's failed_ids on the power-cut path: the first boot's clock at the cut.
/// Ok(None) for a plan without clock_after_cut; Ok(Some(None)) when the clock could not be read.
pub(super) fn at_cut(fx: &mut F<'_>, ctx: &Ctx) -> Step<Option<Option<i64>>> {
    if ctx.plan.clock_after_cut.is_none() {
        return Ok(None);
    }
    Ok(Some(date_now(fx)?))
}

/// The `clock=<epoch>` line hydo-init-clock.sh appends to $HYDO_ACKED ("" without the feature).
pub(super) fn acked_token(c: Option<Option<i64>>) -> String {
    match c {
        None => String::new(),
        Some(v) => format!("clock={}\n", text(v)),
    }
}

/// The `clock=<epoch>` token of the recovery boot's oracle command, as the shell logs it.
pub(super) fn oracle_token(ctx: &Ctx) -> Option<String> {
    ctx.plan
        .clock_after_cut
        .map(|_| format!("clock={}", text(ctx.carried.clock)))
}

/// hydo-init-clock.sh's mount_data in the recovery boot: the clock continues from the first
/// boot's clock at the cut plus the time the machine was off.
pub(super) fn recovery_clock(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let Some(after) = ctx.plan.clock_after_cut else {
        return Ok(());
    };
    // `case "$c" in ""|*[!0-9]*) c=$HYDO_CLOCK_START;; esac`
    let c = ctx
        .carried
        .clock
        .filter(|c| *c >= 0)
        .unwrap_or(ctx.plan.clock_start);
    let r = call(
        fx,
        Command::ClockSet(ClockSet {
            sec: Some(c + after),
            step: None,
        }),
    )?;
    if let Ok(Reply::ClockSet(s)) = r {
        log(
            fx,
            format!(
                "recovery boot: realtime clock set to {} (first boot at the cut: {c}, +{after}s)",
                utc(s.sec)
            ),
        );
    }
    Ok(())
}
