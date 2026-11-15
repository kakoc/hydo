//! A reboot that the guest asks for (Milestone 263, docs/design/devices.md).
//!
//! With `guest.reboot: true` a guest program asks for a reboot with the console line
//! `HYDO-REBOOT [<reason>]`. The controller keeps those lines as facts ([`request_of`]) and takes
//! the reboot at the points of the design: during a workload step (each step is its own call of
//! `workload.run`, [`steps`]), during the workload's retry ([`retry`]), when a service does not
//! get ready after a boot ([`instead_of_failed_start`]), and in a recovery boot before its checks
//! ([`before_checks`]).
//!
//! The reboot itself ([`take`]) is a clean shutdown: the periodic checks and the restart
//! supervisor stop, every service's tree and the workload's get SIGTERM and, after
//! [`GRACE_MS`] of guest time, SIGKILL; then `sync` and a cache flush of every virtio disk. The
//! run's files, the clock and the reactions that acted go into [`RebootInputs`], and the
//! conversation ends with [`Ending::Reboot`]: the engine starts a new boot from S0 with the
//! disks' bytes, and the next conversation ([`super::ScheduleController::rebooted`]) goes on.
//!
//! A plan without `guest.reboot` keeps no console lines for it and takes none of these paths: its
//! commands and transcripts are the same byte for byte.

use super::{
    bb, die_oracle, env, freeze, log, read_all, restart, run, secs, spawn_cmd, stop_periodic,
    svc_sig, timeout_argv, Conversation, Ctx, Memo, Step, F,
};
use crate::control::{Ending, RebootInputs, RebootStage};
use hydo_agent_proto::msg::Sig;
use hydo_schedule::Schedule;

/// The console line of a request.
pub const REQUEST: &str = "HYDO-REBOOT";
/// The reboots one run may take; one more ends the run with [`LOOP_VERDICT`].
pub const MAX: u32 = 64;
pub const LOOP_VERDICT: &str = "HYDO-ORACLE: ERROR reboot loop (more than 64 reboots in a run)";
/// The time between SIGTERM and SIGKILL at a shutdown, in guest milliseconds.
pub const GRACE_MS: u64 = 2000;

/// A request on a console line: its reason ("" without one).
pub(super) fn request_of(line: &str) -> Option<String> {
    let l = line.trim_end_matches('\r');
    let rest = l.strip_prefix(REQUEST)?;
    if rest.is_empty() {
        return Some(String::new());
    }
    rest.strip_prefix(' ').map(|r| r.trim().to_string())
}

/// Why a schedule cannot run on a plan with `guest.reboot: true` (checked before any VM): the
/// parts that carry state across a boot that the first version does not carry.
pub fn refusal(plan: &crate::plan::Plan, schedule: &Schedule) -> Option<String> {
    if !plan.reboot {
        return None;
    }
    let what = if plan.composed() {
        "test compositions"
    } else if plan.perf.is_some() {
        "perf:"
    } else if plan.world.is_some() {
        "fakes:, proxies: and emulators:"
    } else if plan.walk {
        "walk:"
    } else if schedule.host_token("cf").is_some() {
        "cf= (a cut at the k-th flush counts the flushes of one boot)"
    } else if schedule
        .guest
        .iter()
        .any(|t| matches!(t, hydo_schedule::GuestToken::Upgrade(_)))
    {
        "U= upgrades"
    } else {
        return None;
    };
    Some(format!(
        "guest.reboot: a run with {what} is not built yet (docs/design/devices.md)"
    ))
}

/// Whether the guest asked for a reboot in this conversation (decided once, at this point).
pub(super) fn requested(fx: &mut F<'_>, ctx: &Ctx) -> bool {
    ctx.plan.reboot && fx.effect_bool(|_, f| !f.reboots.is_empty())
}

/// The workload steps [a, b), each its own call of `workload.run`. A request seen before a step
/// or while it runs ends the boot: the run goes on with the next step in a new boot. A
/// reaction's power cut ends the steps as in `react::workload`.
pub(super) fn steps(fx: &mut F<'_>, ctx: &Ctx, a: i64, b: i64) -> Step<()> {
    let has_a = !ctx.run.a.is_empty();
    for s in a..b {
        if has_a && super::react::cut_due(fx) {
            return Ok(());
        }
        if requested(fx, ctx) {
            return take(fx, ctx, RebootStage::Steps, s);
        }
        let e = env(
            fx,
            ctx,
            &[
                ("HYDO_FROM", s.to_string()),
                ("HYDO_TO", (s + 1).to_string()),
            ],
        );
        let id = fx.send(spawn_cmd(
            "workload",
            timeout_argv(ctx, &ctx.plan.workload_run),
            e,
            &format!("{}/workload.log", ctx.plan.layout.dir),
            true,
        ))?;
        let st = if fx.reply(id)?.is_err() {
            Some(127)
        } else {
            fx.until(|h, f| {
                f.exits.contains_key(&id)
                    || !f.reboots.is_empty()
                    || (has_a && h.react.cut.is_some())
            })?;
            match fx.effect(|_, f| match f.exits.get(&id) {
                Some(e) => Memo::Id(e.status as u64),
                None => Memo::Unit,
            }) {
                Memo::Id(st) => Some(st as u32),
                _ => None,
            }
        };
        match st {
            Some(st) => {
                if st == 143 {
                    log(
                        fx,
                        format!(
                            "workload steps {s}..{s} TIMED OUT after {}s (killed)",
                            ctx.plan.workload_timeout
                        ),
                    );
                }
                log(fx, format!("workload steps {s}..{s} rc={st}"));
            }
            None if requested(fx, ctx) => return take(fx, ctx, RebootStage::Steps, s + 1),
            None => {
                log(
                    fx,
                    format!("workload steps {s}..{s} interrupted: a reaction's power cut"),
                );
                return Ok(());
            }
        }
    }
    Ok(())
}

/// The workload's retry (`workload.retry`) on a plan with `guest.reboot: true`: a request while
/// it runs ends the boot, and the new boot goes on with the end of the run.
pub(super) fn retry(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    if requested(fx, ctx) {
        return take(fx, ctx, RebootStage::End, ctx.plan.steps);
    }
    let e = env(fx, ctx, &[]);
    let id = fx.send(spawn_cmd(
        "retry",
        timeout_argv(ctx, &ctx.plan.workload_retry),
        e,
        &format!("{}/workload.log", ctx.plan.layout.dir),
        true,
    ))?;
    if fx.reply(id)?.is_err() {
        log(fx, "workload retry rc=127");
        return Ok(());
    }
    fx.until(|_, f| f.exits.contains_key(&id) || !f.reboots.is_empty())?;
    match fx.effect(|_, f| match f.exits.get(&id) {
        Some(e) => Memo::Id(e.status as u64),
        None => Memo::Unit,
    }) {
        Memo::Id(st) => {
            log(fx, format!("workload retry rc={st}"));
            Ok(())
        }
        _ => take(fx, ctx, RebootStage::End, ctx.plan.steps),
    }
}

/// Where this conversation's own boot goes on after a reboot taken before its work started (a
/// service that did not get ready): the stage and step it was given.
fn here(ctx: &Ctx) -> (RebootStage, i64) {
    match &ctx.conv {
        Conversation::Reboot(r) => (r.stage.clone(), r.next),
        Conversation::Recovery { .. } => (RebootStage::Recovery, ctx.plan.steps),
        _ => (RebootStage::Steps, 0),
    }
}

/// A service did not start or get ready (`verdict`): when the guest asked for a reboot in a
/// reboot's or a recovery's boot, the reboot is taken instead of the verdict.
pub(super) fn instead_of_failed_start(fx: &mut F<'_>, ctx: &Ctx, verdict: String) -> Step<()> {
    let boot = matches!(
        ctx.conv,
        Conversation::Reboot(_) | Conversation::Recovery { .. }
    );
    if boot && requested(fx, ctx) {
        let (stage, next) = here(ctx);
        let what = verdict
            .split_once("HYDO-ORACLE: ")
            .map_or(verdict.as_str(), |(_, w)| w);
        log(
            fx,
            format!("not the verdict \"{what}\": the guest asked for a reboot, which goes first"),
        );
        return take(fx, ctx, stage, next);
    }
    die_oracle(fx, verdict)
}

/// In a recovery boot, after its services started and before its checks: a request reboots.
pub(super) fn before_checks(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    if requested(fx, ctx) {
        return take(fx, ctx, RebootStage::Recovery, ctx.plan.steps);
    }
    Ok(())
}

/// The clean shutdown and the end of this boot's conversation (see the module documentation).
pub(super) fn take(fx: &mut F<'_>, ctx: &Ctx, stage: RebootStage, next: i64) -> Step<()> {
    let n = ctx.reboots + 1;
    if n > MAX {
        return die_oracle(fx, LOOP_VERDICT.to_string());
    }
    let reason = fx.effect_text(|_, f| f.reboots.first().cloned().unwrap_or_default());
    let at = match stage {
        RebootStage::Steps => format!("step {next}"),
        RebootStage::End => "the end of the run".into(),
        RebootStage::Recovery => "the recovery boot's checks".into(),
    };
    let why = if reason.is_empty() {
        String::new()
    } else {
        format!(" ({reason})")
    };
    log(
        fx,
        format!("REBOOT {n}: the guest asked for it{why}; a clean shutdown, then a new boot goes on with {at}"),
    );
    stop_periodic(fx);
    restart::stop(fx);
    freeze::kill(fx)?;
    svc_sig(fx, "workload", Sig::Term)?;
    svc_sig(fx, "retry", Sig::Term)?;
    for s in &ctx.plan.services {
        svc_sig(fx, &s.name, Sig::Term)?;
    }
    let e = env(fx, ctx, &[]);
    run(
        fx,
        vec![bb(ctx), "sleep".into(), secs(GRACE_MS, 3)],
        e.clone(),
        None,
    )?;
    svc_sig(fx, "workload", Sig::Kill)?;
    svc_sig(fx, "retry", Sig::Kill)?;
    for s in &ctx.plan.services {
        svc_sig(fx, &s.name, Sig::Kill)?;
    }
    // write back what the kernel holds, then flush every disk's cache, as a device's kernel does
    // at shutdown (`sync` alone sends no flush to a disk without a file system on it)
    let b = bb(ctx);
    let script = format!(
        "{b} sync; for d in /sys/block/vd*; do [ -e \"$d\" ] && {b} sync -d /dev/${{d##*/}}; done; true"
    );
    let r = run(
        fx,
        vec![b.clone(), "sh".into(), "-c".into(), script],
        e,
        Some(4096),
    )?;
    if !r.out.trim().is_empty() {
        log(fx, format!("shutdown sync: {}", r.out.trim()));
    }
    let l = &ctx.plan.layout;
    let acked = read_all(fx, &l.acked())?;
    let outcomes = read_all(fx, &l.outcomes())?;
    let clock = super::clock::now(fx)?;
    let fired: Vec<usize> = {
        let t = fx.effect_text(|h, _| {
            h.react
                .fired
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",")
        });
        let mut v: Vec<usize> = t.split(',').filter_map(|x| x.parse().ok()).collect();
        if let Conversation::Reboot(r) = &ctx.conv {
            v.extend(r.fired.iter().copied());
        }
        v.sort();
        v.dedup();
        v
    };
    let lines = |t: &str| -> Vec<String> { t.lines().map(str::to_string).collect() };
    let mut inputs = RebootInputs {
        n,
        stage: stage.clone(),
        next,
        clock,
        acked: lines(&acked),
        outcomes: lines(&outcomes),
        fired,
        reason,
        ..RebootInputs::default()
    };
    if let Conversation::Recovery { acked, failed } = &ctx.conv {
        inputs.rec_acked = acked.clone();
        inputs.rec_failed = failed.clone();
        inputs.rec_outcomes = ctx.carried.outcomes.clone();
    }
    fx.settle()?;
    fx.end(Ending::Reboot(inputs))
}

/// The start of a reboot's boot (Milestone 263): the clock at the shutdown, the run's files as
/// they were, the data disk, every service.
pub(super) fn boot(fx: &mut F<'_>, ctx: &Ctx, r: &RebootInputs) -> Step<()> {
    log(
        fx,
        format!(
            "command: reboot {} (the guest asked for it; the run goes on with {})",
            r.n,
            match r.stage {
                RebootStage::Steps => format!("step {}", r.next),
                _ => "the end of the run".into(),
            }
        ),
    );
    super::clock::set_to(fx, r.clock.unwrap_or(ctx.plan.clock_start))?;
    let l = &ctx.plan.layout;
    for (file, lines) in [(l.acked(), &r.acked), (l.outcomes(), &r.outcomes)] {
        let text: String = lines.iter().map(|x| format!("{x}\n")).collect();
        super::post(fx, super::write_cmd(&file, "", false))?;
        for c in super::chunks(&text) {
            super::post(fx, super::write_cmd(&file, &c, true))?;
        }
    }
    super::mount_data(
        fx,
        ctx,
        "HYDO-ORACLE: VIOLATION data disk unmountable after a reboot",
    )?;
    super::start_all(
        fx,
        ctx,
        "HYDO-ORACLE: VIOLATION service cannot start after a reboot",
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_its_console_line() {
        assert_eq!(request_of("HYDO-REBOOT"), Some(String::new()));
        assert_eq!(request_of("HYDO-REBOOT\r"), Some(String::new()));
        assert_eq!(
            request_of("HYDO-REBOOT v4 failed its health check"),
            Some("v4 failed its health check".into())
        );
        assert_eq!(request_of("HYDO-REBOOTX"), None);
        assert_eq!(request_of("ota: HYDO-REBOOT"), None);
    }
}
