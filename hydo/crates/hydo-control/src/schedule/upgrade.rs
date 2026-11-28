//! Upgrades: `U=<target>@<at>[/<gap>][+<back>]:<version>` (Milestone 238, docs/design/upgrades.md).
//!
//! A service with `versions:` runs one of its versions at a time; its commands (`setup`, the
//! command, `ready`) are those of the version it runs, and every one of them sees the version's
//! name as `$HYDO_VERSION`. A switch before step p:
//!
//! 1. stops the service as an operator's rolling upgrade does: SIGTERM to its process tree, and
//!    SIGKILL after [`GRACE_MS`] of guest time when it has not ended by then (a thread of the
//!    flow, `upgrade-grace:<svc>`, so the grace period is a `sleep` in the guest);
//! 2. switches its version;
//! 3. starts it on the same data with `start_svc` (its setup, its command, its readiness check).
//!
//! A service that does not come up on its new version ends the run with
//! `VIOLATION [upgrade.started] <svc> did not start on version <v> before step <p>`: the
//! upgrade (or the rollback) itself is the failure, as when a new version cannot read what the
//! old one wrote. A service that is down at its switch (a kill whose restart is still to come)
//! only gets its new version: its restart starts it on that version.
//!
//! Each switch also writes a `HYDO-UPGRADE {...}` note (the run's `upgrades` in `result.json`;
//! a note is for people, so no transcript hash and no fingerprint covers it) and the assertion
//! record `fault.upgraded`. Without `U=` none of this runs: a plan's services start on their own
//! version with their own commands, command for command as before (no `$HYDO_VERSION` for a
//! service without `versions:`).
//!
//! Milestone 392: `U=<target>@<at>~<off>...` makes the token's switches `off` ms into their
//! step, from a thread of their own ([`timer`]), while the step's workload runs; a service from
//! an image switches to the launcher of its version's image (`/etc/hydo/oci/<svc>@<v>/run`).

use super::{
    bb, die_oracle, env, log, run, running, secs, start_svc, svc_sig, targets, xassert, Ctx, Part,
    F,
};
use crate::flow::{Memo, Step};
use hydo_agent_proto::msg::Sig;
use hydo_schedule::{Selector, Target, Upgrade};

/// How long a service may take to end after SIGTERM before it is killed (Docker's and
/// Kubernetes's stop timeouts are 10 s and 30 s).
pub(super) const GRACE_MS: u64 = 10_000;

/// The declaration of the upgrade's assertion (only for a schedule with U=).
pub(super) const UPGRADE_DECL: (&str, &str, &str) = (
    "reachable",
    "fault.upgraded",
    "a service was switched to another version of itself",
);

/// The note prefix of one switch (`result.json`'s `upgrades`).
pub const UPGRADE_NOTE: &str = "HYDO-UPGRADE";

/// The report line that carries every switched service's version across a power cut (the
/// recovery boot starts them on it): `hydo-explore-versions: <svc>=<version> ...`.
pub const VERSIONS_PREFIX: &str = "hydo-explore-versions:";

pub(super) fn label(svc: &str) -> String {
    format!("upgrade-grace:{svc}")
}

/// How many services a token's target names, for the steps of its switches (a group's selector
/// by its size; `primary` is one member).
pub(super) fn target_len(ctx: &Ctx, t: &Target) -> usize {
    match t {
        Target::Service(_) => 1,
        Target::List(l) => l.len(),
        Target::Group { group, sel } => {
            let n = ctx.plan.group(group).map(|g| g.services.len()).unwrap_or(0);
            match sel {
                Selector::Primary => 1,
                s => s.size(n),
            }
        }
    }
}

/// The steps at which the token's switches happen (its first step resolves the target).
pub(super) fn steps(ctx: &Ctx, u: &Upgrade) -> Vec<i64> {
    let n = target_len(ctx, &u.target);
    let names: Vec<String> = (0..n).map(|i| i.to_string()).collect();
    u.switches(&names).iter().map(|w| w.step as i64).collect()
}

/// The version the service runs now (None for a service without versions).
pub(super) fn current(fx: &mut F<'_>, ctx: &Ctx, s: &str) -> Option<String> {
    let sv = ctx.plan.service(s)?;
    if !sv.has_versions() {
        return None;
    }
    let base = sv.version.clone();
    Some(fx.effect_text(|h, _| h.versions.get(s).cloned().unwrap_or(base)))
}

/// One switch of `svc` to `to` (None: back to the version it ran before) before step `p`.
pub(super) fn switch(fx: &mut F<'_>, ctx: &Ctx, svc: &str, to: Option<&str>, p: i64) -> Step<()> {
    switch_when(fx, ctx, svc, to, p, &format!("before step {p}"))
}

/// [`switch`], with the words of when it happens (`before step 5`, `1500 ms into step 5`).
fn switch_when(
    fx: &mut F<'_>,
    ctx: &Ctx,
    svc: &str,
    to: Option<&str>,
    p: i64,
    when: &str,
) -> Step<()> {
    let Some(from) = current(fx, ctx, svc) else {
        log(fx, format!("U: {svc} has no versions, ignored"));
        return Ok(());
    };
    let back = to.is_none();
    let to = match to {
        Some(v) => v.to_string(),
        None => {
            let s = svc.to_string();
            let f = from.clone();
            fx.effect_text(|h, _| h.prev.get(&s).cloned().unwrap_or(f))
        }
    };
    let known = ctx
        .plan
        .service(svc)
        .is_some_and(|s| s.version_names().contains(&to.as_str()));
    if !known {
        log(fx, format!("U: {svc} has no version {to}, ignored"));
        return Ok(());
    }
    if to == from {
        log(
            fx,
            format!("{svc} runs version {to} already {when}; not switched"),
        );
        return Ok(());
    }
    // a restart policy's start in progress ends first (M134; never waits without a policy)
    fx.until(|h, _| !h.starting.contains(svc))?;
    let what = if back { "ROLLING BACK" } else { "UPGRADING" };
    let alive = fx.effect_bool(|h, f| running(h, f, svc));
    if alive {
        log(
            fx,
            format!("{what} {svc} from version {from} to {to} {when}: stopping it"),
        );
        // the restart policies' supervisors and the heal see a process hydo ended (as after K=)
        let s = svc.to_string();
        let spawn = fx.effect_text(|h, _| {
            let v = h.services.get_mut(&s).expect("a running service");
            v.killed = true;
            v.spawn.to_string()
        });
        svc_sig(fx, svc, Sig::Term)?;
        let grace = fx.start(Part::UpgradeGrace {
            svc: svc.to_string(),
            spawn: spawn.parse().unwrap_or(0),
        })?;
        fx.until(|h, f| !running(h, f, svc))?;
        fx.cancel(grace);
    } else {
        log(
            fx,
            format!("{what} {svc} from version {from} to {to} {when}: it is down, it starts on {to} at its restart"),
        );
    }
    let (s, f, t) = (svc.to_string(), from.clone(), to.clone());
    fx.effect(|h, _| {
        if !back {
            h.prev.insert(s.clone(), f);
        }
        h.versions.insert(s, t);
        Memo::Unit
    });
    xassert(fx, UPGRADE_DECL.0, UPGRADE_DECL.1, UPGRADE_DECL.2);
    let started = if alive {
        start_svc(fx, ctx, svc)?
    } else {
        true
    };
    fx.note(format!(
        "{UPGRADE_NOTE} {{\"service\":{},\"step\":{p},\"from\":{},\"to\":{},\"rollback\":{back},\"started\":{}}}",
        js(svc),
        js(&from),
        js(&to),
        if alive { started.to_string() } else { "null".into() }
    ));
    if !started {
        return die_oracle(
            fx,
            format!(
                "HYDO-ORACLE: VIOLATION [upgrade.started] {svc} did not start on version {to} \
                 {when} (it ran {from})"
            ),
        );
    }
    log(fx, format!("{svc} runs version {to} {when}"));
    Ok(())
}

/// The grace period after SIGTERM: the process the switch stopped (spawn `spawn`) is killed
/// when it still runs after [`GRACE_MS`].
pub(super) fn grace(fx: &mut F<'_>, ctx: &Ctx, svc: &str, spawn: u64) -> Step<()> {
    let e = env(fx, ctx, &[]);
    run(
        fx,
        vec![bb(ctx), "sleep".into(), secs(GRACE_MS, 3)],
        e,
        None,
    )?;
    let s = svc.to_string();
    let same = fx.effect_bool(|h, f| {
        h.services.get(&s).is_some_and(|v| v.spawn == spawn) && running(h, f, &s)
    });
    if same {
        log(
            fx,
            format!(
                "{svc} did not end within {} after SIGTERM: killing it",
                secs(GRACE_MS, 0)
            ),
        );
        svc_sig(fx, svc, Sig::Kill)?;
    }
    Ok(())
}

/// The run's switches, resolved at the first step of each token (a group's selector then picks
/// its members, `primary` by its probe): (step, token index, order within the token, service,
/// version or None for a rollback, the token's offset into the step in ms: Milestone 392).
pub(super) type Pending = Vec<(i64, usize, usize, String, Option<String>, Option<u64>)>;

/// The tokens whose first step is `p`: their targets resolved, their switches added to `pending`.
pub(super) fn resolve_at(fx: &mut F<'_>, ctx: &Ctx, p: i64, pending: &mut Pending) -> Step<()> {
    for (i, u) in ctx.run.u.iter().enumerate() {
        if u.at as i64 != p {
            continue;
        }
        let Some(t) = targets::read(fx, ctx, 'U', &u.target) else {
            continue;
        };
        let svcs = targets::resolve(fx, ctx, 'U', &t)?;
        for (k, w) in u.switches(&svcs).into_iter().enumerate() {
            pending.push((w.step as i64, i, k, w.service, w.version, u.off));
        }
    }
    pending.sort_by_key(|x| (x.0, x.1, x.2));
    Ok(())
}

/// The switches due before step `p`, in order (those of a token without `~<off>`).
pub(super) fn apply_at(fx: &mut F<'_>, ctx: &Ctx, p: i64, pending: &mut Pending) -> Step<()> {
    let due: Vec<_> = pending
        .iter()
        .filter(|x| x.0 == p && x.5.is_none())
        .cloned()
        .collect();
    for (_, _, _, svc, v, _) in due {
        switch(fx, ctx, &svc, v.as_deref(), p)?;
    }
    Ok(())
}

/// Milestone 392: the switches of step `p` that a token's `~<off>` makes while the step runs,
/// by their offset (stable: in the order of [`apply_at`] among one offset).
pub(super) fn timed_at(p: i64, pending: &Pending) -> Vec<(u64, String, Option<String>)> {
    let mut v: Vec<(u64, String, Option<String>)> = pending
        .iter()
        .filter(|x| x.0 == p)
        .filter_map(|x| x.5.map(|o| (o, x.3.clone(), x.4.clone())))
        .collect();
    v.sort_by_key(|x| x.0);
    v
}

/// Milestone 392: the thread of the switches made while step `p` runs. It sleeps the first
/// switch's offset from the step's start (each later offset of the step: the difference to the
/// one before, after that switch) and makes the switch as [`switch`] does before a step:
/// SIGTERM, the grace period, the new version's start and its readiness check. The step's
/// workload goes on meanwhile; the main thread joins this one before the next step's faults.
pub(super) fn timer(
    fx: &mut F<'_>,
    ctx: &Ctx,
    p: i64,
    switches: &[(u64, String, Option<String>)],
) -> Step<()> {
    let mut slept = 0u64;
    for (off, svc, v) in switches {
        if *off > slept {
            let e = env(fx, ctx, &[]);
            run(
                fx,
                vec![bb(ctx), "sleep".into(), secs(off - slept, 3)],
                e,
                None,
            )?;
            slept = *off;
        }
        switch_when(
            fx,
            ctx,
            svc,
            v.as_deref(),
            p,
            &format!("{off} ms into step {p}"),
        )?;
    }
    Ok(())
}

/// A JSON string.
fn js(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// The versions of the services a switch moved, as the report line's words (`svc=version`), in
/// the services' order; empty when none moved.
pub(super) fn carried(fx: &mut F<'_>, ctx: &Ctx) -> String {
    let names: Vec<String> = ctx.plan.services.iter().map(|s| s.name.clone()).collect();
    fx.effect_text(|h, _| {
        names
            .iter()
            .filter_map(|n| h.versions.get(n).map(|v| format!("{n}={v}")))
            .collect::<Vec<_>>()
            .join(" ")
    })
}
