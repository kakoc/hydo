//! Reactive exploration (M139, docs/design/guest-control.md step D2): the schedule token
//! `A=<event>[@<step>][+<ms>]:<action>`, which only host control makes possible.
//!
//! A reaction is a thread of the controller, armed before workload step `step` (0 when not
//! written). It watches the guest's console for the first assertion record of `event` whose
//! outcome is the event's (`ok:true` of a `sometimes`/`reachable` assertion, `ok:false` of the
//! others: an `always` that failed, an `unreachable` that was reached) seen after it was armed,
//! waits `ms` of guest time (a guest `sleep`, as the timers do) and acts:
//!
//! - `kill-<svc>`: SIGKILL the service's tree; it stays down until the heal's restart pass
//!   (restart_dead), as after the stack's timed kill `X=`;
//! - `stop-<svc>-<ms>`: SIGSTOP the tree, sleep, SIGCONT (the pause timer's shape);
//! - `cont-<svc>`: SIGCONT the tree: a `P=` pause healed at the reaction point (heal on no
//!   progress) instead of at its step; the step's own SIGCONT later is a no-op;
//! - `cut`: a power cut. The reaction only marks it; the main sequence, which waits for the
//!   workload's exit *or* the mark (see [`workload`]), leaves the step loop and takes the power
//!   cut's path: the workload's client and every service are frozen at once, then the ids are
//!   read and the recovery boot follows, as for `C=`.
//!
//! Reactions are live during the workload's steps only: after the last step the main sequence
//! cancels every reaction that has not seen its event and waits for the others to finish, before
//! it heals; a power cut (`C=` or a reaction's) cancels them all.
//!
//! Reproducibility: the decision is a pure function of the schedule string and of the console
//! lines seen at each numbered stop, so a run with reactions is reproduced by its schedule string
//! (the controller decides again, at the same stops) and by its control transcript (the commands
//! the reaction sent are recorded with their stop numbers, `hydo repro --transcript`). A run
//! without `A=` keeps no console lines and takes none of these paths: its transcript is the same
//! byte for byte.

use super::{
    bb, env, log, run, secs, spawn_cmd, svc_sig, timeout_argv, xassert, Ctx, Memo, Step, F,
};
use hydo_agent_proto::msg::Sig;
use hydo_schedule::{React, ReactAction};
use std::collections::BTreeSet;

/// The declaration a run with reactions reports (its record when a reaction acts).
pub const REACT_DECL: (&str, &str, &str) = (
    "reachable",
    "fault.reacted",
    "a reaction (A=) saw its event and acted",
);

/// What the reactions share with the main sequence.
#[derive(Clone, Debug, Default)]
pub(super) struct State {
    /// The reactions (indices into the schedule's A= tokens) that saw their event.
    pub fired: BTreeSet<usize>,
    /// The first reaction whose power cut is due (its delay is over).
    pub cut: Option<usize>,
}

/// An assertion record of the console: (id, whether it is the assertion's event).
pub(super) fn record_of(line: &str) -> Option<(String, bool)> {
    let j = line.strip_prefix("HYDO-ASSERT ")?;
    let v: serde_json::Value = serde_json::from_str(j.trim()).ok()?;
    let id = v.get("id")?.as_str()?;
    let ok = v.get("ok")?.as_bool()?;
    let t = v.get("t").and_then(|t| t.as_str()).unwrap_or("");
    let event = if matches!(t, "sometimes" | "reachable") {
        ok
    } else {
        !ok
    };
    Some((id.to_string(), event))
}

/// The schedule's reactions that the plan can apply, with their index; a reaction naming a
/// service the manifest does not have is ignored with a line, as the other tokens are.
pub(super) fn armed(fx: &mut F<'_>, ctx: &Ctx) -> Vec<(usize, React)> {
    let mut out = Vec::new();
    for (i, r) in ctx.run.a.iter().enumerate() {
        if let Some(s) = r.action.svc() {
            if ctx.plan.service(s).is_none() {
                log(fx, format!("A: no service {s}, ignored"));
                continue;
            }
        }
        out.push((i, r.clone()));
    }
    out
}

/// Whether a reaction's power cut is due (read once at this point of the main sequence).
pub(super) fn cut_due(fx: &mut F<'_>) -> bool {
    fx.effect_bool(|h, _| h.react.cut.is_some())
}

/// The reaction whose power cut is due, for the cut's log line.
pub(super) fn cut_token(ctx: &Ctx, fx: &mut F<'_>) -> String {
    let i = match fx.effect(|h, _| Memo::Id(h.react.cut.unwrap_or(0) as u64)) {
        Memo::Id(i) => i as usize,
        m => unreachable!("memo out of step: {m:?}"),
    };
    ctx.run
        .a
        .get(i)
        .map(|r| format!("A={r}"))
        .unwrap_or_default()
}

/// Whether reaction `i` saw its event (read once at this point of the main sequence).
pub(super) fn fired(fx: &mut F<'_>, i: usize) -> bool {
    fx.effect_bool(|h, _| h.react.fired.contains(&i))
}

/// The workload call of steps [a, b) when the schedule has reactions: the call is not made when
/// a reaction's power cut is due, and its wait ends at the workload's exit or at the cut. Some
/// status at the exit; None when the cut came first (the client is frozen by the cut's path).
pub(super) fn workload(fx: &mut F<'_>, ctx: &Ctx, a: i64, b: i64) -> Step<Option<u32>> {
    if cut_due(fx) {
        return Ok(None);
    }
    let e = env(
        fx,
        ctx,
        &[("HYDO_FROM", a.to_string()), ("HYDO_TO", b.to_string())],
    );
    let id = fx.send(spawn_cmd(
        "workload",
        timeout_argv(ctx, &ctx.plan.workload_run),
        e,
        &format!("{}/workload.log", ctx.plan.layout.dir),
        true,
    ))?;
    if fx.reply(id)?.is_err() {
        return Ok(Some(127));
    }
    fx.until(|h, f| f.exits.contains_key(&id) || h.react.cut.is_some())?;
    let st = match fx.effect(|_, f| match f.exits.get(&id) {
        Some(e) => Memo::Id(e.status as u64),
        None => Memo::Unit,
    }) {
        Memo::Id(s) => Some(s as u32),
        _ => None,
    };
    if st.is_none() {
        log(
            fx,
            format!(
                "workload steps {a}..{} interrupted: a reaction's power cut",
                b - 1
            ),
        );
    }
    Ok(st)
}

/// One reaction's thread (`Part::React(i)`).
pub(super) fn watch(fx: &mut F<'_>, ctx: &Ctx, i: usize) -> Step<()> {
    let r = ctx.run.a[i].clone();
    // the records seen up to the arming point do not count
    let from = match fx.effect(|_, f| Memo::Id(f.asserts.len() as u64)) {
        Memo::Id(n) => n as usize,
        m => unreachable!("memo out of step: {m:?}"),
    };
    fx.until(|_, f| {
        f.asserts
            .get(from..)
            .is_some_and(|l| l.iter().any(|(id, ev)| *ev && *id == r.event))
    })?;
    if fx.is_cancelled() {
        // cancelled at the stop where its event was seen: the main sequence decided first
        return Ok(());
    }
    fx.effect(|h, _| {
        h.react.fired.insert(i);
        Memo::Unit
    });
    log(fx, format!("REACT A={r}: saw {}", r.event));
    let e = env(fx, ctx, &[]);
    if r.delay_ms > 0 {
        run(
            fx,
            vec![bb(ctx), "sleep".into(), secs(r.delay_ms, 3)],
            e.clone(),
            None,
        )?;
    }
    match &r.action {
        ReactAction::Cut => {
            fx.effect(|h, _| {
                h.react.cut.get_or_insert(i);
                Memo::Unit
            });
            log(fx, format!("REACT A={r}: power cut due"));
        }
        ReactAction::Kill(s) => {
            if ctx.plan.service(s).is_some_and(|x| x.restart.restarts()) {
                // a restart policy's start in progress ends first (M134)
                fx.until(|h, _| !h.starting.contains(s))?;
            }
            svc_sig(fx, s, Sig::Kill)?;
            log(fx, format!("KILLED {s} (reaction A={r})"));
        }
        ReactAction::Stop(s, ms) => {
            svc_sig(fx, s, Sig::Stop)?;
            log(fx, format!("PAUSED {s} (reaction A={r})"));
            run(fx, vec![bb(ctx), "sleep".into(), secs(*ms, 3)], e, None)?;
            svc_sig(fx, s, Sig::Cont)?;
            log(fx, format!("RESUMED {s} (reaction A={r})"));
        }
        ReactAction::Cont(s) => {
            svc_sig(fx, s, Sig::Cont)?;
            log(fx, format!("RESUMED {s} (reaction A={r})"));
        }
    }
    xassert(fx, REACT_DECL.0, REACT_DECL.1, REACT_DECL.2);
    Ok(())
}
