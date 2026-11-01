//! LIVENESS: `eventually` invariants (hydo/onboard/hydo-liveness.sh, M59), driven from the host.
//!
//! The shell extension wraps four init functions; each wrapper is one hook here, called from its
//! `SEAM(Liveness)` place in `schedule.rs`:
//!
//! - `start_periodic() { orig; live_anchor start; }` -> [`after_start_periodic`];
//! - `step_range() { orig; [ $2 = $HYDO_STEPS ] && live_anchor heal; }` -> [`after_step_range`]
//!   (also when the original returned at once because the range was empty);
//! - `start_all() { [ $2 = recovery ] && { live_anchor heal; live_anchor start; }; orig; }` ->
//!   [`before_start_all`];
//! - `finish() { live_resolve; orig; }` -> [`before_finish`].
//!
//! `live_anchor <a>` stamps the anchor's uptime once and starts one background monitor per
//! eventually invariant anchored there. `live_monitor` runs the check under `timeout <within+1>`
//! again and again, `poll` seconds apart, until it exits 0 (`ok`, or `late` when that was after
//! the deadline) or the deadline has passed (`expired`). `live_resolve` waits for each monitor in
//! manifest order and prints one `HYDO-ASSERT` eventually record per invariant; the first one
//! that is not ok becomes the run's violated invariant unless another one already is.
//!
//! The monitors are threads of the flow (like periodic_loop), each started with the environment
//! the init had when it forked the monitor. The uptime the shell reads from /proc/uptime is the
//! agent's `clock_get` (CLOCK_MONOTONIC in ms, shown in hundredths of a second like
//! /proc/uptime). Every hook does nothing for a plan without an eventually invariant, because
//! hydo-build splices the extension only into such images.
//!
//! AMBIGUOUS (HANDOFF.md, decision 18): the agent's `probe` op is not used for the monitor. A
//! probe kills an attempt at the deadline, paces attempts from the probe's start and reports
//! neither the elapsed time nor the output, so it could not give the shell's `late` outcome, its
//! `elapsed_s` or its details. The monitor runs the shell's loop command by command instead.

use super::{bb, call, env, inv_detail, jesc, log, run, run_argv, Ctx, Part, Step, CHUNK, F};
use crate::flow::Memo;
use hydo_agent_proto::msg::{ClockGet, Command, Reply};
use std::collections::BTreeMap;

/// The host's copy of the shell's liveness files in $H.
#[derive(Clone, Debug, Default)]
pub(super) struct Live {
    /// $H/live.anchor.<a>: the uptime at the anchor, in hundredths of a second (None: the
    /// uptime could not be read, the shell's empty file).
    anchors: BTreeMap<String, Option<i64>>,
    /// $H/live.pid.<i>: the monitor thread of invariant i.
    threads: BTreeMap<usize, usize>,
    /// $H/live.res.<i>: what the monitor of invariant i found.
    res: BTreeMap<usize, Res>,
}

/// One monitor's result: `<ok|late|expired> <elapsed-s> <last rc>`, and the last check's output
/// ($H/inv.<i>.out).
#[derive(Clone, Debug)]
struct Res {
    r: &'static str,
    el: String,
    rc: String,
    out: String,
}

/// A monitor thread: the invariant, the environment it was started with, and its anchor's
/// uptime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Monitor {
    pub i: usize,
    env: Vec<String>,
    t0: Option<i64>,
}

pub(super) fn label(m: &Monitor) -> String {
    format!("live:i{}", m.i)
}

fn eventually(ctx: &Ctx) -> impl Iterator<Item = usize> + '_ {
    ctx.plan
        .invariants
        .iter()
        .enumerate()
        .filter(|(_, v)| v.live.is_some())
        .map(|(i, _)| i)
}

fn active(ctx: &Ctx) -> bool {
    eventually(ctx).next().is_some()
}

/// `UP`: the guest's uptime in hundredths of a second.
fn up(fx: &mut F<'_>) -> Step<Option<i64>> {
    Ok(match call(fx, Command::ClockGet(ClockGet {}))? {
        Ok(Reply::ClockGet(c)) => Some((c.mono_ms / 10) as i64),
        _ => None,
    })
}

/// Hundredths of a second as `%.2f` prints them.
fn cs(v: i64) -> String {
    let sign = if v < 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", v.abs() / 100, v.abs() % 100)
}

/// live_anchor: stamp the anchor (the first time only) and start its monitors.
fn live_anchor(fx: &mut F<'_>, ctx: &Ctx, a: &str) -> Step<()> {
    if fx.effect_bool(|h, _| h.live.anchors.contains_key(a)) {
        return Ok(());
    }
    let t = up(fx)?;
    fx.effect(|h, _| {
        h.live.anchors.insert(a.to_string(), t);
        Memo::Unit
    });
    let mine: Vec<usize> = eventually(ctx)
        .filter(|i| ctx.plan.invariants[*i].live.as_ref().unwrap().after == a)
        .collect();
    for i in mine {
        // `live_monitor $i &`: the subshell keeps the environment of this moment
        let e = env(fx, ctx, &[]);
        let th = fx.start(Part::Live(Monitor { i, env: e, t0: t }))?;
        fx.effect(|h, _| {
            h.live.threads.insert(i, th);
            Memo::Unit
        });
    }
    log(
        fx,
        format!(
            "liveness anchor '{a}' at uptime={}s",
            t.map(cs).unwrap_or_default()
        ),
    );
    Ok(())
}

/// live_monitor: poll the check from its anchor until it holds or its deadline passes.
pub(super) fn live_monitor(fx: &mut F<'_>, ctx: &Ctx, m: &Monitor) -> Step<()> {
    let inv = &ctx.plan.invariants[m.i];
    let lv = inv.live.as_ref().expect("an eventually invariant");
    let w = lv.within;
    let (r, el, rc, out) = loop {
        // `$BB timeout $((w + 1)) $BB sh -c ". /etc/hydo/prelude.sh; <cmd>" > $H/inv.$i.out 2>&1`
        let mut argv = vec![bb(ctx), "timeout".into(), (w + 1).to_string()];
        argv.extend(run_argv(ctx, &inv.cmd));
        let ran = run(fx, argv, m.env.clone(), Some(CHUNK as u32))?;
        let b = up(fx)?;
        let el = b.unwrap_or(0) - m.t0.unwrap_or(0);
        if ran.status == 0 {
            break (
                if el <= w * 100 { "ok" } else { "late" },
                el,
                ran.status,
                ran.out,
            );
        }
        if el > w * 100 {
            break ("expired", el, ran.status, ran.out);
        }
        run(
            fx,
            vec![bb(ctx), "sleep".into(), lv.poll.to_string()],
            m.env.clone(),
            None,
        )?;
    };
    let res = Res {
        r,
        el: cs(el),
        rc: rc.to_string(),
        out,
    };
    fx.effect(|h, _| {
        h.live.res.insert(m.i, res);
        Memo::Unit
    });
    Ok(())
}

/// live_resolve: wait for every monitor, print the records, remember the first violation.
fn live_resolve(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let all: Vec<usize> = eventually(ctx).collect();
    for i in all {
        let inv = &ctx.plan.invariants[i];
        let lv = inv.live.as_ref().unwrap();
        let a = lv.after.as_str();
        // an anchor never reached: it is now
        live_anchor(fx, ctx, a)?;
        if let Some(&th) = fx.shared.live.threads.get(&i) {
            fx.join(th)?;
        }
        // the monitor has written its result (a missing one reads as `expired ? ?`)
        let res = fx.shared.live.res.get(&i).cloned().unwrap_or(Res {
            r: "expired",
            el: "?".into(),
            rc: "?".into(),
            out: fx
                .shared
                .last_out
                .get(&inv.key)
                .cloned()
                .unwrap_or_default(),
        });
        let det = inv_detail(&res.out);
        let (ok, line) = match res.r {
            "ok" => (
                true,
                format!(
                    "satisfied {}s after {a} (within {}s): {det}",
                    res.el, lv.within
                ),
            ),
            "late" => (
                false,
                format!(
                    "satisfied only {}s after {a}, deadline {}s: {det}",
                    res.el, lv.within
                ),
            ),
            _ => (
                false,
                format!(
                    "not satisfied {}s after {a}, deadline {}s (last check rc={}: {det})",
                    res.el, lv.within, res.rc
                ),
            ),
        };
        // `echo "<line>" >> $H/inv.$i.out`, then the record's details from the whole file
        let out = format!("{}{line}\n", res.out);
        let d = inv_detail(&out);
        let key = inv.key.clone();
        fx.effect(|h, _| {
            h.last_out.insert(key, out);
            Memo::Unit
        });
        fx.report(format!(
            "HYDO-ASSERT {{\"t\":\"eventually\",\"id\":\"{}\",\"ok\":{ok},\"msg\":\"{}\",\"details\":{{\"after\":\"{a}\",\"elapsed_s\":\"{}\",\"out\":\"{}\",\"within_s\":{}}}}}",
            inv.id,
            jesc(&inv.msg),
            res.el,
            jesc(&d),
            lv.within
        ));
        log(
            fx,
            format!(
                "liveness {}: {} after {}s (deadline {}s after {a})",
                inv.id, res.r, res.el, lv.within
            ),
        );
        if !ok {
            let key = inv.key.clone();
            fx.effect(|h, _| {
                if h.violated.is_none() {
                    h.violated = Some(key);
                }
                Memo::Unit
            });
        }
    }
    Ok(())
}

/// The start_periodic wrapper: the `start` anchor once the checkers run.
pub(super) fn after_start_periodic(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    if !active(ctx) {
        return Ok(());
    }
    live_anchor(fx, ctx, "start")
}

/// The step_range wrapper: the `heal` anchor after the range that ends at the last step.
pub(super) fn after_step_range(fx: &mut F<'_>, ctx: &Ctx, b: i64) -> Step<()> {
    if !active(ctx) || b != ctx.plan.steps {
        return Ok(());
    }
    live_anchor(fx, ctx, "heal")
}

/// The start_all wrapper: in the recovery boot both anchors are reached before the services
/// start (crash recovery counts against the deadline).
pub(super) fn before_start_all(fx: &mut F<'_>, ctx: &Ctx, recovery: bool) -> Step<()> {
    if !active(ctx) || !recovery {
        return Ok(());
    }
    live_anchor(fx, ctx, "heal")?;
    live_anchor(fx, ctx, "start")
}

/// The finish wrapper: resolve every eventually invariant before the final invariants.
pub(super) fn before_finish(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    if !active(ctx) {
        return Ok(());
    }
    live_resolve(fx, ctx)
}

#[cfg(test)]
mod tests {
    use super::cs;

    #[test]
    fn hundredths_print_as_awk_prints_them() {
        assert_eq!(cs(0), "0.00");
        assert_eq!(cs(5), "0.05");
        assert_eq!(cs(1234), "12.34");
        assert_eq!(cs(-50), "-0.50");
    }
}
