//! Faults aimed at several services or at a role (Milestone 195, docs/design/jepsen-gaps.md):
//! `K=`, `P=` and `X=` take a list (`K=n1,n2@5+2`) or a selector over a group of the manifest's
//! `groups:` (`K=nodes.majority@5+2`, `K=nodes.primary@5+2`), and several `N=` (or `O=`) tokens
//! are one fault when they share their window and netem arguments.
//!
//! A selector is resolved before the step its token names: `one`, `minority`, `majority` and
//! `all` from the group's members in the manifest's order (`hydo_schedule::Selector::pick`, a
//! pure function of the token), `primary` by running the group's `primary:` command in the
//! guest, which prints the service name of the current primary. The resolution is logged
//! (`K: nodes.primary = n2`), so the control transcript records which services the fault hit,
//! and the commands that follow (the signals, the tc filters) name them: `hydo repro
//! --transcript` replays them exactly. A schedule keeps the selector's text (the intent); the
//! transcript keeps what it meant in that run.
//!
//! A plain service target takes exactly the path it took before Milestone 195, with no command
//! and no log line more, so no recorded transcript or fingerprint moves.

use super::netfault::NetTok;
use super::{bb, die_oracle, env, log, run, run_argv, Ctx, F};
use crate::flow::Step;
use crate::seams::Unsupported;
use hydo_schedule::{NetTarget, Selector, Target};

/// How often the primary's probe is asked before the run ends in an error, one second of guest
/// time apart (an election takes a few seconds).
pub(super) const PROBE_TRIES: u32 = 10;
/// The most output of the probe kept.
const PROBE_CAPTURE: u32 = 4096;

/// Read a K=, P= or X= target when the schedule is read (explore_run): a service or a list
/// member the image does not have, or a group it does not have, is ignored with a log line, as a
/// plain unknown service always was (the names check refuses such a schedule before the run; the
/// identity corpora keep the old path through `run_unchecked`). None: the fault is ignored.
pub(super) fn read(fx: &mut F<'_>, ctx: &Ctx, letter: char, t: &Target) -> Option<Target> {
    match t {
        Target::Service(s) => {
            if ctx.plan.service(s).is_none() {
                log(fx, format!("{letter}: no service {s}, ignored"));
                return None;
            }
            Some(t.clone())
        }
        Target::List(l) => {
            let mut kept = Vec::new();
            for s in l {
                if ctx.plan.service(s).is_none() {
                    log(fx, format!("{letter}: no service {s}, ignored"));
                } else {
                    kept.push(s.clone());
                }
            }
            match kept.len() {
                0 => None,
                1 => Some(Target::Service(kept.remove(0))),
                _ => Some(Target::List(kept)),
            }
        }
        Target::Group { group, .. } => {
            if ctx.plan.group(group).is_none() {
                log(fx, format!("{letter}: no group {group}, ignored"));
                return None;
            }
            Some(t.clone())
        }
    }
}

/// The services a target acts on now. A group selector is resolved here and logged.
pub(super) fn resolve(fx: &mut F<'_>, ctx: &Ctx, letter: char, t: &Target) -> Step<Vec<String>> {
    match t {
        Target::Service(s) => Ok(vec![s.clone()]),
        Target::List(l) => Ok(l.clone()),
        Target::Group { group, sel } => {
            let members = ctx
                .plan
                .group(group)
                .map(|g| g.services.clone())
                .unwrap_or_default();
            let got = match sel {
                Selector::Primary => vec![primary(fx, ctx, group)?],
                _ => match sel.pick(&members) {
                    Some(v) => v,
                    None => {
                        return die_oracle(
                            fx,
                            format!(
                                "HYDO-ORACLE: ERROR {letter}={t}: the group {group} has {} \
                                 members, too few for {}",
                                members.len(),
                                sel.word()
                            ),
                        )
                    }
                },
            };
            log(fx, format!("{letter}: {t} = {}", got.join(",")));
            Ok(got)
        }
    }
}

/// The group's current primary: the first line its `primary:` command prints, when it names a
/// member. The command is asked up to [`PROBE_TRIES`] times, a second of guest time apart; after
/// that the run ends in an error (a fault aimed at the primary that hit nothing would be a silent
/// pass).
pub(super) fn primary(fx: &mut F<'_>, ctx: &Ctx, group: &str) -> Step<String> {
    let g = ctx.plan.group(group).cloned().unwrap_or_else(|| {
        // read() and the names check keep unknown groups out
        crate::plan::PlanGroup {
            name: group.into(),
            services: Vec::new(),
            primary: String::new(),
        }
    });
    if g.primary.is_empty() {
        return die_oracle(
            fx,
            format!(
                "HYDO-ORACLE: ERROR the group {group} has no primary: command, so \
                 {group}.primary names no service"
            ),
        );
    }
    let mut last = String::new();
    for i in 0..PROBE_TRIES {
        if i > 0 {
            let e = env(fx, ctx, &[]);
            run(fx, vec![bb(ctx), "sleep".into(), "1".into()], e, None)?;
        }
        let e = env(fx, ctx, &[]);
        let r = run(fx, run_argv(ctx, &g.primary), e, Some(PROBE_CAPTURE))?;
        let first = r
            .out
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string();
        if r.status == 0 && g.services.contains(&first) {
            return Ok(first);
        }
        last = format!("exit status {}, first line {first:?}", r.status);
    }
    die_oracle(
        fx,
        format!(
            "HYDO-ORACLE: ERROR {group}.primary: the group's primary: command named no member \
             in {PROBE_TRIES} tries ({last})"
        ),
    )
}

/// Several N= (or O=) tokens as one fault (M195). Before Milestone 195 the last token of the
/// letter replaced the others without a word, so a partition written link by link ran as its
/// last link. Now:
///
/// - a token whose target is the target of a later one is still replaced by it (the identity
///   corpus records `N=a@1-3:delay,9ms N=a@2-5:delay,20ms` as the second token alone);
/// - the tokens left are one fault when they share the window, the direction and the netem
///   arguments and every target is a link, a partition or a group's shape: the links of all of
///   them are cut together, with one netem queue per device, and lifted together;
/// - otherwise the schedule is refused: a service's own port fault, or a second window, cannot be
///   in force beside another fault (the network faults use one netem queue per device).
pub(super) fn merge_net(letter: char, toks: Vec<NetTok>) -> Result<Option<NetTok>, Unsupported> {
    let mut kept: Vec<NetTok> = Vec::new();
    for t in toks {
        kept.retain(|k| k.target != t.target);
        kept.push(t);
    }
    if kept.len() <= 1 {
        return Ok(kept.pop());
    }
    let first = kept[0].clone();
    for t in &kept {
        if let NetTarget::Service(s) = &t.target {
            return Err(Unsupported::other(format!(
                "{letter}={s}@... (a fault on the service's port) cannot be in force together \
                 with another {letter}= token; several {letter}= tokens are one fault only over \
                 links (a-b, a-*), a partition (a,b/c) or a group's shape"
            )));
        }
        if t.from != first.from || t.to != first.to || t.dir != first.dir || t.netem != first.netem
        {
            return Err(Unsupported::other(format!(
                "{letter}= appears more than once with different windows, directions or netem \
                 arguments; several {letter}= tokens are one fault only with the same window and \
                 netem (one netem queue per device): give them the same ones, or write one \
                 partition N=a,b/c@<from>-<to>:<netem>"
            )));
        }
    }
    let mut merged = first;
    merged.more = kept[1..].iter().map(|t| t.target.clone()).collect();
    Ok(Some(merged))
}
