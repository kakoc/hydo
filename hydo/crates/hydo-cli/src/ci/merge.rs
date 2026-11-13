//! `hydo ci merge`: the shards of one CI run (`hydo ci run --shard I/N`) into one `ci-run.json`,
//! one `report.md` and one `junit.xml` (stage 2 of docs/design/ci.md, section 7.1). It runs no VM.
//!
//! Each shard explored its own contiguous part of the seed range, compared it with the base over
//! the same seeds, and decided its findings by replay. The merge combines them by finding key, in
//! shard order (the lowest seeds first), so the result does not depend on which shard finished
//! first. For one key it keeps what a single run over the whole range would have decided:
//!
//! - found by some shard's exploration: `new` when a shard proved it new by a base replay,
//!   `reappeared` likewise; else `present` when some shard's base also found it; else `changed`,
//!   `pre-existing`, `present` or `unverified`, in that order;
//! - found by no shard's exploration (a finding of the base, the history or an accepted entry,
//!   replayed here): `present` when some replay failed with the key, else `changed`, else
//!   `unverified`, and `fixed` only when every shard proved it fixed;
//! - accepted when a shard accepted it; an accepted entry is stale only when no shard found it.
//!
//! The shards must be one run: the same project, commit, base, image, engine, hydo, budget and
//! number of shards. A missing shard makes the merged run partial (it never calls a finding
//! fixed). Shards shrink their own failures, so a signature found by two shards is shrunk twice;
//! the minimal schedule kept is the lowest shard's, which is the one a single run would shrink
//! (the explorer shrinks the first failure of each signature in seed order).

use super::{report, run, seeds};
use crate::ctx::{abspath, die, take};
use hydo_results::{
    ci_state, CiCoverage, CiFinding, CiProperties, CiRun, CiShardRun, CiStaleAcceptance,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Whether a shard's exploration found the finding (older runs have no `source`: a finding
/// of the base replayed here has a `head-` proof).
fn explored_here(f: &CiFinding) -> bool {
    match f.source.as_deref() {
        Some(s) => s == "explored",
        None => !f.proof.as_deref().unwrap_or("").starts_with("head-"),
    }
}

fn underlying(f: &CiFinding) -> String {
    match &f.accepted {
        Some(a) if f.state == ci_state::ACCEPTED => a.state.clone(),
        _ => f.state.clone(),
    }
}

/// One key's merged finding from its entries in shard order (the rules above).
fn merge_key(entries: &[&CiFinding]) -> CiFinding {
    let st: Vec<String> = entries.iter().map(|f| underlying(f)).collect();
    let pick = |want: &str, from_explored: Option<bool>| {
        entries
            .iter()
            .zip(&st)
            .find(|(f, s)| {
                s.as_str() == want && from_explored.is_none_or(|e| explored_here(f) == e)
            })
            .map(|(f, _)| (*f).clone())
    };
    let h = entries.iter().any(|f| explored_here(f));
    // the base found the key in some shard: a shard's explored finding it calls present, or a
    // finding of the base replayed in a shard
    let base_has = entries.iter().zip(&st).any(|(f, s)| {
        (explored_here(f) && s == ci_state::PRESENT) || f.source.as_deref() == Some("base")
    });
    let chosen = if h {
        pick(ci_state::REAPPEARED, Some(true))
            .or_else(|| pick(ci_state::NEW, Some(true)))
            .or_else(|| {
                if base_has {
                    None
                } else {
                    pick(ci_state::CHANGED, Some(true))
                }
            })
            .or_else(|| {
                base_has
                    .then(|| {
                        pick(ci_state::PRESENT, Some(true)).or_else(|| {
                            entries.iter().find(|f| explored_here(f)).map(|f| {
                                let mut g = (*f).clone();
                                g.state = ci_state::PRESENT.into();
                                g.proof = Some("base-explored-in-another-shard".into());
                                g
                            })
                        })
                    })
                    .flatten()
            })
            .or_else(|| pick(ci_state::PRE_EXISTING, Some(true)))
            .or_else(|| pick(ci_state::PRESENT, Some(true)))
            .unwrap_or_else(|| {
                let f = entries
                    .iter()
                    .find(|f| explored_here(f))
                    .expect("an explored entry");
                (*f).clone()
            })
    } else {
        pick(ci_state::PRESENT, None)
            .or_else(|| pick(ci_state::CHANGED, None))
            .or_else(|| pick(ci_state::UNVERIFIED, None))
            .or_else(|| {
                st.iter()
                    .all(|s| s == ci_state::FIXED)
                    .then(|| entries[0].clone())
            })
            .unwrap_or_else(|| entries[0].clone())
    };
    let mut m = chosen;
    // the state without the acceptance; acceptance comes back below
    if let Some(a) = m.accepted.take() {
        if m.state == ci_state::ACCEPTED {
            m.state = a.state;
        }
    }
    for f in entries {
        for s in &f.schedules {
            if !m.schedules.contains(s) {
                m.schedules.push(s.clone());
            }
        }
        for c in &f.classes {
            if !m.classes.contains(c) {
                m.classes.push(c.clone());
            }
        }
    }
    m.replays = entries.iter().flat_map(|f| f.replays.clone()).collect();
    if let Some(a) = entries.iter().find_map(|f| f.accepted.clone()) {
        m.accepted = Some(hydo_results::CiAccepted {
            state: m.state.clone(),
            ..a
        });
    }
    m
}

/// The shards in shard order, checked to be one run.
fn check(shards: &mut [CiRun]) -> Result<u64, String> {
    let Some(first) = shards.first() else {
        return Err("no shard to merge".into());
    };
    let n_of = |r: &CiRun| -> Result<(u64, u64), String> {
        r.budget
            .shard
            .as_deref()
            .and_then(seeds::parse_shard)
            .ok_or_else(|| format!("{}: not a shard's run (hydo ci run --shard I/N)", r.id))
    };
    let (_, n) = n_of(first)?;
    let same = |what: &str, a: String, b: String, id: &str| {
        if a == b {
            Ok(())
        } else {
            Err(format!(
                "the shards are not one run: {id} has {what} {b:?}, the first shard {a:?}"
            ))
        }
    };
    let key = |r: &CiRun| {
        vec![
            ("the project", r.project.name.clone()),
            ("the commit", r.commit.clone().unwrap_or_default()),
            (
                "the base commit",
                r.base
                    .as_ref()
                    .and_then(|b| b.commit.clone())
                    .unwrap_or_default(),
            ),
            ("the image", r.image.sha256.clone().unwrap_or_default()),
            ("the engine", r.engine.backend.clone().unwrap_or_default()),
            ("hydo", r.hydo.version.clone()),
            ("the mode", r.budget.mode.clone()),
            ("the strategy", r.budget.strategy.clone()),
            ("the options", r.budget.options.join(" ")),
            ("the range", r.budget.range.clone().unwrap_or_default()),
        ]
    };
    let k0 = key(first);
    let mut seen = BTreeSet::new();
    for r in shards.iter() {
        let (i, m) = n_of(r)?;
        same("the number of shards", n.to_string(), m.to_string(), &r.id)?;
        for ((what, a), (_, b)) in k0.iter().zip(key(r)) {
            same(what, a.clone(), b, &r.id)?;
        }
        if !seen.insert(i) {
            return Err(format!("shard {i}/{n} is given twice"));
        }
    }
    shards.sort_by_key(|r| n_of(r).map(|x| x.0).unwrap_or(0));
    Ok(n)
}

/// The merged run (`fail_on`: the policy, else the first shard's).
pub fn merge(mut shards: Vec<CiRun>, fail_on: Option<Vec<String>>) -> Result<CiRun, String> {
    let n = check(&mut shards)?;
    let have: BTreeSet<u64> = shards
        .iter()
        .filter_map(|r| r.budget.shard.as_deref().and_then(seeds::parse_shard))
        .map(|x| x.0)
        .collect();
    let missing: Vec<u64> = (1..=n).filter(|i| !have.contains(i)).collect();
    let mut m = shards[0].clone();
    let range = m.budget.range.clone().unwrap_or_default();
    m.budget.seeds = range.clone();
    m.budget.shard = None;
    m.budget.range = None;
    let mut done = Vec::new();
    let (mut planned, mut completed) = (0, 0);
    for r in &shards {
        let c = r
            .budget
            .completed_seeds
            .clone()
            .unwrap_or_else(|| r.budget.seeds.clone());
        done.extend(seeds::parse_ranges(&c));
        planned += r
            .budget
            .planned
            .unwrap_or_else(|| seeds::count(&seeds::parse_ranges(&r.budget.seeds)));
        completed += r
            .budget
            .completed
            .unwrap_or_else(|| seeds::count(&seeds::parse_ranges(&c)));
    }
    for i in &missing {
        if let Some((lo, hi)) = hydo_manifest::ci::parse_seeds(&range) {
            if let Some((a, b)) = seeds::shard_of(lo, hi, *i, n) {
                planned += b - a + 1;
            }
        }
    }
    m.budget.completed_seeds = Some(seeds::ranges_text(done));
    m.budget.planned = Some(planned);
    m.budget.completed = Some(completed);
    m.budget.partial = !missing.is_empty() || shards.iter().any(|r| r.budget.partial);
    // the numbers
    m.summary.runs = shards.iter().map(|r| r.summary.runs).sum();
    m.summary.failures = shards.iter().map(|r| r.summary.failures).sum();
    m.summary.replays = shards.iter().map(|r| r.summary.replays).sum();
    m.summary.vacuous_passes = shards.iter().map(|r| r.summary.vacuous_passes).sum();
    m.summary.wall_s = shards.iter().map(|r| r.summary.wall_s).fold(0.0, f64::max);
    let mut cov = CiCoverage::default();
    let mut not_ex: Option<BTreeSet<String>> = None;
    let mut declared = 0;
    for r in &shards {
        for (k, v) in &r.coverage.faults {
            *cov.faults.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &r.coverage.services {
            *cov.services.entry(k.clone()).or_default() += v;
        }
        if let Some(p) = &r.coverage.properties {
            declared = declared.max(p.declared);
            let s: BTreeSet<String> = p.not_exercised.iter().cloned().collect();
            not_ex = Some(match not_ex {
                None => s,
                Some(o) => o.intersection(&s).cloned().collect(),
            });
        }
    }
    if let Some(ne) = not_ex {
        cov.properties = Some(CiProperties {
            declared,
            reached: declared.saturating_sub(ne.len() as u64),
            not_exercised: ne.into_iter().collect(),
        });
    }
    m.coverage = cov;
    m.nondeterminism = shards
        .iter()
        .flat_map(|r| r.nondeterminism.clone())
        .collect();
    let mut regs = Vec::new();
    let mut names = BTreeSet::new();
    for r in &shards {
        for g in &r.regressions {
            if names.insert(g.name.clone()) {
                regs.push(g.clone());
            }
        }
    }
    m.regressions = regs;
    // the findings, by key in shard order
    let mut order: Vec<String> = Vec::new();
    let mut by: BTreeMap<String, Vec<&CiFinding>> = BTreeMap::new();
    for r in &shards {
        for f in &r.findings {
            if !by.contains_key(&f.key) {
                order.push(f.key.clone());
            }
            by.entry(f.key.clone()).or_default().push(f);
        }
    }
    let mut findings = Vec::new();
    let mut stale: Vec<CiStaleAcceptance> = Vec::new();
    for k in &order {
        let mut f = merge_key(&by[k]);
        if f.state == ci_state::FIXED && m.budget.partial {
            f.state = ci_state::UNVERIFIED.into();
            f.proof = Some("head-replay-ok-partial".into());
        }
        if let Some(a) = f.accepted.take() {
            if f.state == ci_state::FIXED {
                stale.push(CiStaleAcceptance {
                    key: f.key.clone(),
                    title: f.title.clone(),
                    reason: a.reason.clone(),
                    why: "replay-ok".into(),
                    replay: f.replays.iter().rev().find(|r| r.on == "head").cloned(),
                });
            } else {
                f.accepted = Some(hydo_results::CiAccepted {
                    state: f.state.clone(),
                    ..a
                });
                f.state = ci_state::ACCEPTED.into();
            }
        }
        findings.push(f);
    }
    // an accepted entry is stale when no shard found its key
    for r in &shards {
        for s in &r.stale_accepted {
            if !by.contains_key(&s.key) && !stale.iter().any(|x| x.key == s.key) {
                stale.push(s.clone());
            }
        }
    }
    m.findings = findings;
    m.stale_accepted = stale;
    m.shards = shards
        .iter()
        .map(|r| CiShardRun {
            shard: r.budget.shard.clone().unwrap_or_default(),
            id: r.id.clone(),
            seeds: r.budget.seeds.clone(),
            completed_seeds: r.budget.completed_seeds.clone(),
            runs: r.summary.runs,
            failures: r.summary.failures,
            replays: r.summary.replays,
            wall_s: r.summary.wall_s,
            exit: r.outcome.exit,
        })
        .collect();
    if let Some(f) = fail_on {
        m.outcome.fail_on = f;
    }
    // a shard that could not complete makes the merged run not completed
    m.outcome.exit = if shards.iter().any(|r| r.outcome.exit >= 2) {
        2
    } else {
        0
    };
    m.artifacts.exploration = None;
    m.artifacts.base_exploration = None;
    run::decide(&mut m);
    if !missing.is_empty() {
        m.outcome.failed_by.push(format!(
            "shard{} {} of {n} missing (the merged run is partial)",
            if missing.len() == 1 { "" } else { "s" },
            missing
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
        // a pass that explored only part of the seeds would be a silent one
        if m.outcome.exit < 2 {
            m.outcome.exit = 1;
        }
    }
    Ok(m)
}

pub fn cmd_merge(argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    let out = take(&mut args, "--out").unwrap_or_else(|| "hydo-ci".into());
    let fail_on = take(&mut args, "--fail-on").map(|f| {
        f.split(',')
            .map(|w| {
                hydo_manifest::ci::FailOn::parse(w.trim())
                    .unwrap_or_else(|| die(&format!("--fail-on: {w:?} is not a state")))
                    .word()
                    .to_string()
            })
            .collect::<Vec<_>>()
    });
    if args.is_empty() || args.iter().any(|a| a.starts_with("--")) {
        die(
            "usage: hydo ci merge <shard ci-run.json or directory> ... [--out DIR] [--fail-on ...]",
        );
    }
    let shards: Vec<CiRun> = args
        .iter()
        .map(|a| {
            let p = abspath(Path::new(a));
            let p = if p.is_dir() { p.join("ci-run.json") } else { p };
            super::read_ci_run(&p).unwrap_or_else(|e| die(&e))
        })
        .collect();
    let m = merge(shards, fail_on).unwrap_or_else(|e| die(&format!("hydo ci merge: {e}")));
    let md = report::markdown(&m, &[]);
    let d = PathBuf::from(out);
    let _ = std::fs::create_dir_all(&d);
    let w = |n: &str, t: &str| {
        std::fs::write(d.join(n), t).unwrap_or_else(|e| die(&format!("{n}: {e}")))
    };
    w("report.md", &md);
    w("junit.xml", &report::junit(&m));
    hydo_results::write_document(&d.join("ci-run.json"), &m)
        .unwrap_or_else(|e| die(&format!("ci-run.json: {e}")));
    print!("{md}");
    println!(
        "hydo ci merge: {} shards into {} (exit status {})",
        m.shards.len(),
        d.join("ci-run.json").display(),
        m.outcome.exit
    );
    m.outcome.exit as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydo_results::{CiBudget, CiOutcome};

    fn f(key: &str, state: &str, source: &str, sched: &str) -> CiFinding {
        CiFinding {
            key: key.into(),
            kind: "invariant".into(),
            title: format!("VIOLATION [{key}]"),
            state: state.into(),
            minimal: sched.into(),
            schedules: vec![sched.into()],
            source: Some(source.into()),
            ..CiFinding::default()
        }
    }

    fn shard(i: u64, seeds: &str, fs: Vec<CiFinding>) -> CiRun {
        CiRun {
            id: format!("local:{i}"),
            budget: CiBudget {
                mode: "pr".into(),
                seeds: seeds.into(),
                strategy: "process".into(),
                shard: Some(format!("{i}/2")),
                range: Some("1-20".into()),
                completed_seeds: Some(seeds.into()),
                ..CiBudget::default()
            },
            findings: fs,
            outcome: CiOutcome {
                fail_on: vec!["new".into(), "reappeared".into(), "regression".into()],
                ..CiOutcome::default()
            },
            ..CiRun::default()
        }
    }

    /// The rules of the module's documentation, on the cases two shards can disagree about.
    #[test]
    fn shards_merge_by_key_in_shard_order() {
        let s1 = shard(
            1,
            "1-10",
            vec![
                f("a", "new", "explored", "A1"),
                f("b", "pre-existing", "explored", "B1"),
                f("c", "fixed", "base", "C1"),
                f("d", "fixed", "base", "D1"),
            ],
        );
        let s2 = shard(
            2,
            "11-20",
            vec![
                f("a", "present", "explored", "A2"),
                f("b", "present", "base", "B2"),
                f("c", "present", "explored", "C2"),
                f("d", "fixed", "base", "D2"),
            ],
        );
        let m = merge(vec![s2.clone(), s1.clone()], None).unwrap();
        let st: Vec<(&str, &str, &str)> = m
            .findings
            .iter()
            .map(|f| (f.key.as_str(), f.state.as_str(), f.minimal.as_str()))
            .collect();
        assert_eq!(
            st,
            [
                ("a", "new", "A1"),
                ("b", "present", "B1"),
                ("c", "present", "C2"),
                ("d", "fixed", "D1")
            ]
        );
        assert_eq!(m.findings[0].schedules, ["A1", "A2"]);
        assert_eq!(m.budget.seeds, "1-20");
        assert_eq!(m.budget.completed_seeds.as_deref(), Some("1-20"));
        assert!(!m.budget.partial);
        assert_eq!(m.outcome.exit, 1);
        assert_eq!(m.shards.len(), 2);
        // the same result whatever the order the shards are given in
        assert_eq!(merge(vec![s1.clone(), s2.clone()], None).unwrap(), m);
        // a missing shard: partial, nothing fixed
        let p = merge(vec![s1.clone()], None).unwrap();
        assert!(p.budget.partial);
        assert_eq!(p.budget.planned, Some(20));
        assert_eq!(p.findings[3].state, "unverified");
        assert!(p
            .outcome
            .failed_by
            .iter()
            .any(|x| x.contains("shard 2 of 2 missing")));
        // not one run
        let mut other = s2.clone();
        other.commit = Some("x".into());
        assert!(merge(vec![s1.clone(), other], None)
            .unwrap_err()
            .contains("the commit"));
        assert!(merge(vec![s1.clone(), s1], None)
            .unwrap_err()
            .contains("twice"));
    }

    /// An accepted finding stays accepted, and an entry is stale only when no shard found it.
    #[test]
    fn acceptance_survives_the_merge() {
        let acc = |mut x: CiFinding| {
            x.accepted = Some(hydo_results::CiAccepted {
                reason: "documented".into(),
                state: x.state.clone(),
                ..Default::default()
            });
            x.state = "accepted".into();
            x
        };
        let mut s1 = shard(1, "1-10", vec![acc(f("a", "new", "explored", "A1"))]);
        let mut s2 = shard(2, "11-20", vec![]);
        s2.stale_accepted.push(CiStaleAcceptance {
            key: "a".into(),
            reason: "documented".into(),
            why: "replay-ok".into(),
            ..Default::default()
        });
        s1.stale_accepted.push(CiStaleAcceptance {
            key: "z".into(),
            reason: "old".into(),
            why: "replay-ok".into(),
            ..Default::default()
        });
        let m = merge(vec![s1, s2], None).unwrap();
        assert_eq!(m.findings[0].state, "accepted");
        assert_eq!(m.findings[0].accepted.as_ref().unwrap().state, "new");
        assert_eq!(m.outcome.exit, 0);
        let st: Vec<&str> = m.stale_accepted.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(st, ["z"]);
    }
}
