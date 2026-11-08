//! What a schedule names, checked against what the image has (no silent passes, 2026-11-07).
//!
//! Until this check a guest token that named a service, a link, a fake, a knob or a fault kind
//! the image does not have was ignored: the controller logged "K: no service kafka, ignored" in
//! the guest's log and the run went on as the clean run, with its verdict and its fingerprint. A
//! typo in a hand-written schedule (`K=primry@5+2`) or an exploration of another system's fault
//! space (the libSQL onboarding's friction item 6 and 7) then tested nothing and said OK.
//!
//! [`check`] refuses such a schedule before any VM starts, with a message that names what the
//! token names and what the image has. It is called by `run_cfg`, so every path that drives a
//! schedule on an agent image refuses it: `hydo run`, `hydo repro`, `hydo explain`'s neighbours,
//! `hydo explore --schedule` and the explorer's own runs (`AgentEngine::check` before the
//! engine starts, the explorer's worker before its run). A schedule that names only what the
//! image has is driven exactly as before: the check adds no command, so no recorded control
//! transcript or fingerprint moves (`tests/names.rs` checks every recorded schedule of
//! `hydo/onboard/recorded.json` against its image's manifest).
//!
//! What a token may name:
//!
//! - `K=`, `P=`, `X=`: a service.
//! - `N=`, `O=`: a service, or a link `a-b` / `a-*` between services with `addr:`; the image needs
//!   `network` or `oneway` in `faults:` (else it has no `tc`, and the fault was skipped); a
//!   service's own fault acts on its `port:`, so the service needs one.
//! - `F=`: the image has `freeze` in `faults:`; the frozen service is a service, its peer a
//!   service or `workload`.
//! - `H=`: a fake or proxy (`fakes:`/`proxies:`), or one of its routes as `<name>.<route>`.
//! - `G=`: a service with `storage:` (M183); `L=`: a service whose `storage:` has its own file
//!   system (`size_mib:`). The controller would otherwise log "has no storage, ignored".
//! - `A=`: the action's service (`kill-`, `stop-`, `cont-`). The event is an assertion id that
//!   only the running program declares, so it cannot be checked before the run.
//! - `W=`: a test composer plan (`test:` or the `hydo/` folder).
//! - `B=`: `buggify: true`.
//! - `Q=`: a knob `Q` of the manifest (the bug benchmark's order-service concurrency); no agent
//!   image reads it otherwise (the shell-era stack client did).
//! - `W=`, `B=`, `F=`, `H=` without their feature: a knob of that letter, if the manifest has one
//!   (the controller hands such a letter to the knobs).
//! - any other letter: a knob of the manifest's `knobs:`.
//!
//! `C=` and `T=` name nothing.

use crate::plan::Plan;
use hydo_schedule::{GuestToken, NetTarget, Pause, ReactAction, Schedule};

/// Refuse a schedule whose guest tokens name what `plan`'s image does not have. The error is
/// one message for people.
pub fn check(plan: &Plan, s: &Schedule) -> Result<(), String> {
    for t in &s.guest {
        check_token(plan, t)?;
    }
    Ok(())
}

fn list(v: impl Iterator<Item = String>) -> String {
    let v: Vec<String> = v.collect();
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

fn services(plan: &Plan) -> String {
    list(plan.services.iter().map(|s| s.name.clone()))
}

fn service(plan: &Plan, t: &GuestToken, svc: &str) -> Result<(), String> {
    if plan.service(svc).is_some() {
        return Ok(());
    }
    Err(format!(
        "{t} names the service {svc}, which this image does not have (its services: {})",
        services(plan)
    ))
}

fn net_target(plan: &Plan, t: &GuestToken, target: &NetTarget) -> Result<(), String> {
    let has_tc = !plan.layout.tc.is_empty();
    if !has_tc {
        return Err(format!(
            "{t} is a network fault, and this image has none: its manifest's faults: lists \
             neither network nor oneway (add them and build the image again)"
        ));
    }
    let addr = |s: &str| plan.service(s).map(|v| !v.addr.is_empty());
    let with_addr = || {
        list(
            plan.services
                .iter()
                .filter(|s| !s.addr.is_empty())
                .map(|s| s.name.clone()),
        )
    };
    match target {
        NetTarget::Service(s) => {
            service(plan, t, s)?;
            let v = plan.service(s).expect("checked above");
            if v.port.is_empty() {
                return Err(format!(
                    "{t} acts on the port of the service {s}, which has no port: in the \
                     manifest (give it port:, or name a link a-b between services with addr:)"
                ));
            }
            Ok(())
        }
        NetTarget::Link(a, b) => {
            for x in [a, b] {
                match addr(x) {
                    None => service(plan, t, x)?,
                    Some(false) => {
                        return Err(format!(
                            "{t} names a link of the service {x}, which has no addr: (services \
                             with addr:: {})",
                            with_addr()
                        ))
                    }
                    Some(true) => {}
                }
            }
            if a == b {
                return Err(format!("{t}: a link joins two different services"));
            }
            Ok(())
        }
        NetTarget::LinkAll(a) => {
            match addr(a) {
                None => service(plan, t, a)?,
                Some(false) => {
                    return Err(format!(
                        "{t} names the links of the service {a}, which has no addr: (services \
                         with addr:: {})",
                        with_addr()
                    ))
                }
                Some(true) => {}
            }
            if !plan
                .services
                .iter()
                .any(|s| s.name != *a && !s.addr.is_empty())
            {
                return Err(format!(
                    "{t}: the service {a} has no other service with addr: to be linked to"
                ));
            }
            Ok(())
        }
    }
}

fn check_token(plan: &Plan, t: &GuestToken) -> Result<(), String> {
    // A letter the controller has no feature of the image for goes to the manifest's knobs
    // (set_knob's last branch), where a knob of that letter takes it: the bug benchmark's
    // manifest declares Q (the order service's concurrency) as a knob.
    let knob = plan.knob(&t.letter().to_string()).is_some();
    match t {
        GuestToken::Compose(_) if !plan.composed() && knob => return Ok(()),
        GuestToken::Buggify(_) if !plan.buggify && knob => return Ok(()),
        GuestToken::Freeze(_) if !plan.freeze && knob => return Ok(()),
        GuestToken::Protocol(_) if plan.world.is_none() && knob => return Ok(()),
        GuestToken::Concurrency { .. } if knob => return Ok(()),
        _ => {}
    }
    match t {
        GuestToken::Kill { svc, .. }
        | GuestToken::TimedKill { svc, .. }
        | GuestToken::Pause(Pause::Steps { svc, .. })
        | GuestToken::Pause(Pause::Timer { svc, .. }) => service(plan, t, svc),
        GuestToken::Net { target, .. } | GuestToken::OneWay { target, .. } => {
            net_target(plan, t, target)
        }
        GuestToken::Cut(_) | GuestToken::Clock { .. } => Ok(()),
        GuestToken::Corrupt { svc, .. } => {
            service(plan, t, svc)?;
            if plan.storage_of(svc).is_some() {
                return Ok(());
            }
            Err(format!(
                "{t} corrupts the storage of the service {svc}, which has no storage: in the \
                 manifest (services with storage:: {})",
                list(plan.storage.iter().map(|s| s.svc.clone()))
            ))
        }
        GuestToken::PowerLoss { svc, .. } => {
            service(plan, t, svc)?;
            if plan.storage_of(svc).is_some_and(|s| s.part.is_some()) {
                return Ok(());
            }
            Err(format!(
                "{t} is a power loss of the service {svc}, whose storage: has no file system of \
                 its own (give it storage: {{dir:, size_mib:}}; services with one: {})",
                list(
                    plan.storage
                        .iter()
                        .filter(|s| s.part.is_some())
                        .map(|s| s.svc.clone())
                )
            ))
        }
        GuestToken::Freeze(f) => {
            if !plan.freeze {
                return Err(format!(
                    "{t} is a freeze, and this image has none: its manifest's faults: does not \
                     list freeze (add it and build the image again)"
                ));
            }
            service(plan, t, &f.svc)?;
            if f.peer != "workload" && plan.service(&f.peer).is_none() {
                return Err(format!(
                    "{t} names the peer {}, which is neither a service of this image nor \
                     workload (its services: {})",
                    f.peer,
                    services(plan)
                ));
            }
            Ok(())
        }
        GuestToken::Protocol(h) => match &plan.world {
            None => Err(format!(
                "{t} is a protocol fault, and this image has no fakes: or proxies: to put it on"
            )),
            Some(w) if w.targets.contains(&h.target) => Ok(()),
            Some(w) => Err(format!(
                "{t} names {}, which is not a fake, a proxy or one of their routes in this \
                 image (it has: {})",
                h.target,
                list(w.targets.iter().cloned())
            )),
        },
        GuestToken::React(r) => match &r.action {
            ReactAction::Cut => Ok(()),
            ReactAction::Kill(svc) | ReactAction::Stop(svc, _) | ReactAction::Cont(svc) => {
                service(plan, t, svc)
            }
        },
        GuestToken::Compose(_) => {
            if plan.composed() {
                Ok(())
            } else if plan.compose.is_some() {
                Err(format!(
                    "{t} is a test composer composition, and this image's plan has no test \
                     composer commands (only checks or regressions; its workload is the \
                     manifest's workload.run)"
                ))
            } else {
                Err(format!(
                    "{t} is a test composer composition, and this image has no test composer \
                     plan (test: or a hydo/ folder)"
                ))
            }
        }
        GuestToken::Buggify(_) => {
            if plan.buggify {
                Ok(())
            } else {
                Err(format!(
                    "{t} sets BUGGIFY, and this image's manifest does not say buggify: true"
                ))
            }
        }
        GuestToken::Concurrency { .. } => Err(format!(
            "{t}: Q= is read by no agent image unless its manifest has a knob Q (the shell-era \
             stack client read it); the workload's own commands decide how many requests run \
             at once"
        )),
        GuestToken::Knob { letter, .. } => {
            if plan.knob(&letter.to_string()).is_some() {
                Ok(())
            } else {
                Err(format!(
                    "{t} sets the knob {letter}, which this image's manifest does not have \
                     (its knobs: {})",
                    list(plan.knobs.iter().map(|k| k.token.clone()))
                ))
            }
        }
    }
}

/// The step a token's fault starts at (None for a token without one: `B=`, `W=`, `Q=`, knobs).
fn start_step(t: &GuestToken) -> Option<u64> {
    match t {
        GuestToken::Kill { at, .. }
        | GuestToken::TimedKill { at, .. }
        | GuestToken::Pause(Pause::Steps { at, .. })
        | GuestToken::Pause(Pause::Timer { at, .. })
        | GuestToken::Cut(at)
        | GuestToken::Clock { at, .. }
        | GuestToken::Corrupt { at, .. }
        | GuestToken::PowerLoss { at, .. } => Some(*at),
        GuestToken::Net { from, .. } | GuestToken::OneWay { from, .. } => Some(*from),
        GuestToken::Protocol(h) => Some(h.from),
        GuestToken::Freeze(f) => Some(f.step),
        GuestToken::React(r) => Some(r.at),
        _ => None,
    }
}

/// Faults that start after the workload's last step (a warning, not a refusal): the controller
/// drops every fault point beyond `workload.steps` (with test commands: the rounds), so such a
/// token does nothing and the run is the run without it. A fault at the last step itself runs
/// after the workload's last request, before the heal and the checks, which is a fault of its
/// own (a power cut after the last acknowledgement), so it is not warned about. One sentence
/// per token.
pub fn step_warnings(plan: &Plan, s: &Schedule) -> Vec<String> {
    let steps = u64::try_from(plan.steps).unwrap_or(0);
    s.guest
        .iter()
        .filter_map(|t| {
            let at = start_step(t)?;
            (at > steps).then(|| {
                format!(
                    "{t} starts at step {at}, after the workload's last step ({steps}): it does \
                     nothing, and the run is the run without it"
                )
            })
        })
        .collect()
}
