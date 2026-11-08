//! `hydo ci`: hydo in a team's continuous integration, stage 1 of docs/design/ci.md.
//!
//! - `hydo ci run` (src/ci/run.rs): build or reuse the image, explore, compare with the base
//!   commit by replay, replay the saved regressions, and write `ci-run.json`, `report.md`,
//!   `junit.xml` and an exit code (0 passed, 1 failed by the policy, 2 not completed). It reads
//!   flags, `hydo/ci.yaml` (hydo_manifest::ci) and the CI system's variables (src/ci/env.rs), and
//!   runs the same on a laptop.
//! - `hydo ci compare <base ci-run.json> <head ci-run.json>`: the report of two recorded runs,
//!   offline, by finding key (no replay, so nothing is proven fixed).
//!
//! hydo never calls a CI platform's API: comments and status checks are the adapters' work
//! (`action/` for GitHub, `ci/gitlab/` for GitLab, the generic recipe in `hydo guide ci`).

pub mod env;
pub mod inputkey;
pub mod key;
pub mod report;
pub mod run;

use crate::ctx::{die, take, Ctx};
use hydo_results::{ci_state, CiRun};
use std::path::{Path, PathBuf};

pub const HELP: &str = r#"hydo ci -- hydo in continuous integration (hydo guide ci)

    hydo ci run [--project DIR] [--config hydo/ci.yaml] [--mode pr|push|nightly] [--out DIR]
                [--commit SHA] [--base REV | --no-base] [--base-branch NAME] [--change N]
                [--base-run CI-RUN.JSON] [--image IMG] [--base-image IMG] [--backend kvm|hvx]
                [--seeds A-B] [--strategy S] [--option OPT ...] [--workers N] [--set KEY=VALUE ...]
                [--fail-on new,reappeared,regression,...] [--cache DIR]
        build (or reuse) the image, explore it, replay each finding on the base commit's image
        (new, pre-existing), replay the base's findings here (fixed, present, changed), replay
        the saved regressions, and write ci-run.json, report.md and junit.xml into --out
        (default hydo-ci). Exit status: 0 passed, 1 failed by fail_on, 2 not completed.
        The CI system's variables give the commit, the base branch and the change number on
        GitHub Actions, GitLab CI, CircleCI, Jenkins, Buildkite and Azure Pipelines; flags win.

    hydo ci compare <base ci-run.json> <head ci-run.json> [--out DIR]
        the report of two recorded CI runs by finding key, offline (no replay: a finding the head
        did not find is listed as not found, never as fixed)
"#;

pub fn cmd_ci(ctx: &Ctx, argv: &[String]) -> i32 {
    match argv.first().map(String::as_str) {
        Some("run") => run::cmd_run(ctx, &argv[1..]),
        Some("compare") => cmd_compare(&argv[1..]),
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            0
        }
        _ => {
            print!("{HELP}");
            2
        }
    }
}

/// A recorded CI run.
pub fn read_ci_run(p: &Path) -> Result<CiRun, String> {
    let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
    match hydo_results::reader::document(&text).map_err(|e| format!("{}: {e}", p.display()))? {
        hydo_results::reader::Record::CiRun(r) => Ok(*r),
        _ => Err(format!("{}: not a ci-run document", p.display())),
    }
}

/// The head run's findings with their states decided by key against the base run's: a key the
/// base had is `present`, a new key is `new` (proof `by-key`), and a base key the head did not
/// find is listed as `unverified` (proof `not-found-here`): absence after a bounded search proves
/// nothing, and only a replay (`hydo ci run`) proves a finding fixed.
pub fn compare(base: &CiRun, head: &CiRun) -> CiRun {
    let mut r = head.clone();
    let open = |f: &&hydo_results::CiFinding| f.state != ci_state::FIXED;
    let base_keys: std::collections::BTreeSet<&str> = base
        .findings
        .iter()
        .filter(open)
        .map(|f| f.key.as_str())
        .collect();
    let head_keys: std::collections::BTreeSet<String> = head
        .findings
        .iter()
        .filter(open)
        .map(|f| f.key.clone())
        .collect();
    r.findings = head.findings.iter().filter(open).cloned().collect();
    for f in &mut r.findings {
        if base_keys.contains(f.key.as_str()) {
            f.state = ci_state::PRESENT.into();
            f.proof = Some("by-key".into());
        } else {
            f.state = ci_state::NEW.into();
            f.proof = Some("by-key".into());
        }
    }
    for f in base.findings.iter().filter(open) {
        if !head_keys.contains(&f.key) {
            let mut g = f.clone();
            g.state = ci_state::UNVERIFIED.into();
            g.proof = Some("not-found-here".into());
            g.replays = vec![hydo_results::CiReplay {
                on: "head".into(),
                schedule: f.minimal.clone(),
                refused: Some(
                    "not found by this run's exploration and not replayed (hydo ci compare \
                     compares recorded runs; hydo ci run replays)"
                        .into(),
                ),
                ..Default::default()
            }];
            r.findings.push(g);
        }
    }
    r.base = Some(hydo_results::CiBase {
        branch: base.branch.clone(),
        commit: base.commit.clone(),
        ci_run: Some(base.id.clone()),
        findings_from: "base-run".into(),
        ..Default::default()
    });
    run::decide(&mut r);
    r
}

fn cmd_compare(argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    let out = take(&mut args, "--out");
    if args.len() != 2 {
        die("usage: hydo ci compare <base ci-run.json> <head ci-run.json> [--out DIR]");
    }
    let base = read_ci_run(Path::new(&args[0])).unwrap_or_else(|e| die(&e));
    let head = read_ci_run(Path::new(&args[1])).unwrap_or_else(|e| die(&e));
    let r = compare(&base, &head);
    let md = report::markdown(&r, &[]);
    print!("{md}");
    if let Some(d) = out {
        let d = PathBuf::from(d);
        let _ = std::fs::create_dir_all(&d);
        let w = |n: &str, t: &str| {
            std::fs::write(d.join(n), t).unwrap_or_else(|e| die(&format!("{n}: {e}")))
        };
        w("report.md", &md);
        w("junit.xml", &report::junit(&r));
        let _ = hydo_results::write_document(&d.join("ci-run.json"), &r);
    }
    r.outcome.exit as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydo_results::CiFinding;

    fn run(id: &str, keys: &[(&str, &str)]) -> CiRun {
        CiRun {
            id: id.into(),
            findings: keys
                .iter()
                .map(|(k, s)| CiFinding {
                    key: (*k).into(),
                    title: format!("VIOLATION [{k}]"),
                    state: (*s).into(),
                    minimal: format!("disk=none | run K={k}@1+1"),
                    ..CiFinding::default()
                })
                .collect(),
            outcome: hydo_results::CiOutcome {
                fail_on: vec!["new".into(), "reappeared".into(), "regression".into()],
                ..Default::default()
            },
            ..CiRun::default()
        }
    }

    /// `hydo ci compare` decides by key only, so it never calls a finding fixed: a base finding
    /// the head did not find is listed as not found here.
    #[test]
    fn compare_by_key_never_proves_fixed() {
        let base = run("b", &[("a", "new"), ("b", "present"), ("c", "fixed")]);
        let head = run("h", &[("a", "present"), ("d", "new")]);
        let r = compare(&base, &head);
        let st: Vec<(&str, &str)> = r
            .findings
            .iter()
            .map(|f| (f.key.as_str(), f.state.as_str()))
            .collect();
        assert_eq!(st, [("a", "present"), ("d", "new"), ("b", "unverified")]);
        assert_eq!(r.outcome.exit, 1);
        assert_eq!(r.base.as_ref().unwrap().ci_run.as_deref(), Some("b"));
        let md = report::markdown(&r, &[]);
        assert!(md.contains("**Could not be replayed**"), "{md}");
    }
}
