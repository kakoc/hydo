//! Restart policies (`services[].restart:`, docs/HISTORY.md M134): a service whose process ends by
//! itself while the test runs is started again as Docker Compose's `restart:` would, at points
//! the run defines. The shell init has no such key (a dead service stays down until the heal's
//! restart passes, `restart_dead`); the default `no` keeps exactly that, and a plan without a
//! policy has no supervisor at all, so its transcripts are the ones it had before.
//!
//! - **When a supervisor watches.** One thread per service with a policy (`restart:<svc>`), from
//!   the start of the workload (right after `start_periodic`, in a run from S1 and in solo mode)
//!   and in the recovery boot after `start_all`, until the conversation ends. A power cut stops
//!   the supervisors before the services are frozen (nothing is started after the cut, as the
//!   shell's `kill $ptimer`). The boot to S1 has none: a service that dies while it is being
//!   provisioned is the start-up's failure. While `start_all` starts the services (the boot and
//!   the recovery boot), the policy acts inside each service's readiness check instead
//!   (`await_ready_starting`, Milestone 204): a process that ends during its check is restarted
//!   as below, within the check's time limit, so a service that crashes because a dependency is
//!   not ready yet comes up as under Docker.
//! - **What it reacts to.** The exit of the service's current process that hydo did not cause:
//!   a process that hydo's own faults killed (`K=`, `X=`) keeps the schedule's meaning (`K=`
//!   restarts it before step at+down, the heal restarts the rest), whatever the policy. Pauses,
//!   freezes and the cut's SIGSTOP end no process.
//! - **The decision** is Docker's restart manager (moby `restartmanager`): `always` and
//!   `unless-stopped` restart after every exit, `on-failure` after an exit status that is not 0
//!   (a signal's 128+n counts), `on-failure:<max>` at most `<max>` times in one conversation (0:
//!   no limit). Before each restart it waits 100 ms of guest time, doubled after every restart up
//!   to 60 s, and back to 100 ms when the process had run 10 s or more (guest time: the agent's
//!   CLOCK_MONOTONIC from the restart that started it; a process the supervisor did not start is
//!   counted as long-running).
//! - **Determinism.** The host sees the exit at a numbered stop; the delay is a `sleep` in the
//!   guest; the restart is `start_svc` (setup, the command, its readiness check, as every other
//!   start), sent at the stop where the sleep's end is seen. So a restart sits at a defined stop of
//!   the run, and the transcript records it like every other command.
//! - **Against the main sequence.** Only one start of a service runs at a time (`Host::starting`,
//!   claimed by every `start_svc`): `restart_dead` and the K= restart wait for a restart in
//!   progress and then see the service alive, and a supervisor whose delay ends after the main
//!   sequence started the service again does nothing.

use super::{bb, call, log, run, secs, start_svc, Ctx, Part, F};
use crate::flow::{Memo, Step};
use hydo_agent_proto::msg::{ClockGet, Command, Reply};
use hydo_manifest::Restart;

/// The supervisors' threads (to stop them at a power cut).
#[derive(Clone, Debug, Default)]
pub(super) struct State {
    threads: Vec<usize>,
}

pub(super) fn label(svc: &str) -> String {
    format!("restart:{svc}")
}

/// Docker's first delay, its multiplier's cap, and the run time that resets it.
pub(super) const FIRST_MS: u64 = 100;
pub(super) const MAX_MS: u64 = 60_000;
const RESET_MS: u64 = 10_000;

/// Start a supervisor for every service with a policy (none for a plan without one).
pub(super) fn start(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    for s in &ctx.plan.services {
        if !s.restart.restarts() {
            continue;
        }
        let t = fx.start(Part::Restart(s.name.clone()))?;
        fx.effect(|h, _| {
            h.restart.threads.push(t);
            Memo::Unit
        });
    }
    Ok(())
}

/// Stop every supervisor (a power cut).
pub(super) fn stop(fx: &mut F<'_>) {
    let ts = fx.effect_text(|h, _| {
        h.restart
            .threads
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join(",")
    });
    for t in ts.split(',').filter(|t| !t.is_empty()) {
        fx.cancel(t.parse().expect("a thread number"));
    }
}

/// The guest's CLOCK_MONOTONIC in ms (None when the agent did not answer).
fn mono(fx: &mut F<'_>) -> Step<Option<u64>> {
    Ok(match call(fx, Command::ClockGet(ClockGet {}))? {
        Ok(Reply::ClockGet(c)) => Some(c.mono_ms),
        _ => None,
    })
}

/// The supervisor of one service.
pub(super) fn watch(fx: &mut F<'_>, ctx: &Ctx, svc: &str) -> Step<()> {
    let pol = ctx.plan.service(svc).map(|s| s.restart).unwrap_or_default();
    let name = pol.name();
    // the spawn whose exit was handled last, the delay of the last restart, the restarts made,
    // and (spawn, guest time) of the process this supervisor started last
    let mut handled: Option<u64> = None;
    let mut delay: u64 = 0;
    let mut count: u64 = 0;
    let mut mine: Option<(u64, Option<u64>)> = None;
    loop {
        // the current process ended, and not by one of hydo's kills
        // (or its start was refused, which counts as exit status 127, the shell's "not found")
        fx.until(|h, f| {
            h.services.get(svc).is_some_and(|s| {
                Some(s.spawn) != handled
                    && !s.killed
                    && (f.exits.contains_key(&s.spawn)
                        || matches!(f.replies.get(&s.spawn), Some(Err(_))))
            })
        })?;
        let (spawn, status) = match fx.effect(|h, f| {
            let id = h.services[svc].spawn;
            Memo::Text(format!(
                "{id} {}",
                f.exits.get(&id).map(|e| e.status).unwrap_or(127)
            ))
        }) {
            Memo::Text(t) => {
                let (a, b) = t.split_once(' ').expect("two numbers");
                (a.parse::<u64>().unwrap(), b.parse::<u32>().unwrap())
            }
            m => unreachable!("memo out of step: {m:?}"),
        };
        handled = Some(spawn);
        let now = mono(fx)?;
        let ran_long = match (mine, now) {
            (Some((id, Some(t0))), Some(t1)) if id == spawn => t1.saturating_sub(t0) >= RESET_MS,
            _ => true,
        };
        if ran_long {
            delay = 0;
        }
        let again = match pol {
            Restart::No => false,
            Restart::Always | Restart::UnlessStopped => true,
            Restart::OnFailure { max } => status != 0 && (max == 0 || count < max),
        };
        if !again {
            let why = match pol {
                Restart::OnFailure { max } if status != 0 => {
                    format!("{max} restarts made")
                }
                _ => "exit status 0".to_string(),
            };
            log(
                fx,
                format!("{svc} exited (status {status}); not restarted (restart: {name}, {why})"),
            );
            continue;
        }
        delay = if delay == 0 {
            FIRST_MS
        } else {
            (delay * 2).min(MAX_MS)
        };
        log(
            fx,
            format!(
                "{svc} exited (status {status}); restarting it in {delay} ms (restart: {name})"
            ),
        );
        let e = super::env(fx, ctx, &[]);
        run(fx, vec![bb(ctx), "sleep".into(), secs(delay, 3)], e, None)?;
        // the main sequence may have started it again (restart_dead, K=), or a K= kill may have
        // made it the schedule's to bring back, while the delay ran
        let go = fx.effect_bool(|h, _| {
            !h.starting.contains(svc)
                && h.services
                    .get(svc)
                    .is_some_and(|s| s.spawn == spawn && !s.killed)
        });
        if !go {
            log(
                fx,
                format!("{svc}: already started again, no restart (restart: {name})"),
            );
            continue;
        }
        count += 1;
        log(
            fx,
            format!("restarting {svc} (restart: {name}, restart {count})"),
        );
        let up = start_svc(fx, ctx, svc)?;
        let id = match fx.effect(|h, _| Memo::Id(h.services[svc].spawn)) {
            Memo::Id(id) => id,
            m => unreachable!("memo out of step: {m:?}"),
        };
        let t0 = mono(fx)?;
        mine = Some((id, t0));
        if !up {
            log(
                fx,
                format!("{svc} did not come up after its restart (restart: {name})"),
            );
        }
    }
}
