//! `hydo ci accept <finding key> --reason ...`: add an entry to `hydo/known-findings.yaml`
//! (`hydo_manifest::known_findings`; stage 2 of docs/design/ci.md). The finding's title and its
//! minimal schedule come from a recorded `ci-run.json`, so that later runs can replay the entry
//! and tell when it no longer reproduces. `--who` is written as given: hydo never fills it from
//! a login or a host name.

use crate::ctx::{abspath, die, take};
use hydo_manifest::known_findings::{self as kf, Accepted};
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: hydo ci accept <finding key> --reason TEXT [--who NAME] [--from CI-RUN.JSON|DIR] [--file hydo/known-findings.yaml] [--project DIR]";

/// The entry for `key` (a full key, or a unique prefix of a key of `run`).
pub fn entry(
    key: &str,
    run: Option<&hydo_results::CiRun>,
    reason: &str,
    who: Option<&str>,
    date: &str,
) -> Result<Accepted, String> {
    if reason.trim().is_empty() {
        return Err("--reason: say why this finding is expected".into());
    }
    let found = run.map(|r| {
        let mut m: Vec<&hydo_results::CiFinding> = r
            .findings
            .iter()
            .filter(|f| f.key.starts_with(key))
            .collect();
        m.dedup_by(|a, b| a.key == b.key);
        m
    });
    let f = match found.as_deref() {
        Some([f]) => Some(*f),
        Some([]) | None => None,
        Some(many) => {
            return Err(format!(
                "{key} matches {} findings of the run: {}",
                many.len(),
                many.iter()
                    .map(|f| f.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    };
    let full = f.map(|f| f.key.clone()).unwrap_or_else(|| key.to_string());
    if !kf::is_key(&full) {
        let known = run
            .map(|r| {
                r.findings
                    .iter()
                    .map(|f| format!("{} ({})", f.key, f.title))
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        return Err(format!(
            "{key} is not a finding key like f1:invariant:5ef675c8c9{}",
            if known.is_empty() {
                " (give --from a ci-run.json to accept by a prefix)".to_string()
            } else {
                format!("; the run's findings: {known}")
            }
        ));
    }
    Ok(Accepted {
        key: full,
        title: f.map(|f| f.title.clone()),
        schedule: f.map(|f| f.minimal.clone()).filter(|s| !s.is_empty()),
        reason: reason.trim().to_string(),
        who: Some(who.unwrap_or("").to_string()),
        date: Some(date.to_string()),
    })
}

pub fn cmd_accept(argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let reason = take(&mut args, "--reason").unwrap_or_default();
    let who = take(&mut args, "--who");
    let from = take(&mut args, "--from");
    let file = take(&mut args, "--file");
    let project = abspath(Path::new(
        &take(&mut args, "--project").unwrap_or_else(|| ".".into()),
    ));
    if args.len() != 1 || args[0].starts_with("--") {
        die(USAGE);
    }
    let key = args[0].clone();
    let from: Option<PathBuf> = match from {
        Some(f) => {
            let p = abspath(Path::new(&f));
            Some(if p.is_dir() { p.join("ci-run.json") } else { p })
        }
        None => Some(project.join("hydo-ci/ci-run.json")).filter(|p| p.is_file()),
    };
    let run = from
        .as_ref()
        .map(|p| super::read_ci_run(p).unwrap_or_else(|e| die(&e)));
    let date = crate::ctx::strftime("%Y-%m-%d");
    let a = entry(&key, run.as_ref(), &reason, who.as_deref(), &date).unwrap_or_else(|e| die(&e));
    let path = file
        .map(|f| abspath(Path::new(&f)))
        .unwrap_or_else(|| project.join(kf::DEFAULT_PATH));
    let name = path.display().to_string();
    let old = if path.is_file() {
        Some(std::fs::read_to_string(&path).unwrap_or_else(|e| die(&format!("{name}: {e}"))))
    } else {
        None
    };
    let text = kf::append(old.as_deref(), &a, &name).unwrap_or_else(|e| die(&e.message));
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, &text).unwrap_or_else(|e| die(&format!("{}: {e}", tmp.display())));
    std::fs::rename(&tmp, &path).unwrap_or_else(|e| die(&format!("{name}: {e}")));
    print!("{}", kf::entry_text(&a));
    println!(
        "hydo ci accept: {} added to {name}{}; commit it with the change it belongs to",
        a.key,
        if a.schedule.is_none() {
            " without a schedule (no ci-run.json names the key), so a run cannot replay it to tell when it can go"
        } else {
            ""
        }
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydo_results::{CiFinding, CiRun};

    fn run() -> CiRun {
        let f = |k: &str| CiFinding {
            key: k.into(),
            title: format!("VIOLATION [{k}]"),
            minimal: "disk=cut | run C=14".into(),
            ..CiFinding::default()
        };
        CiRun {
            findings: vec![
                f("f1:invariant:5ef675c8c9"),
                f("f1:invariant:5ef675c8c9"),
                f("f1:run-error:0011223344"),
                f("f1:run-error:0099887766"),
            ],
            ..CiRun::default()
        }
    }

    #[test]
    fn a_prefix_of_a_runs_key_is_enough_and_who_is_never_guessed() {
        let a = entry("f1:inv", Some(&run()), "documented", None, "2026-11-13").unwrap();
        assert_eq!(a.key, "f1:invariant:5ef675c8c9");
        assert_eq!(a.schedule.as_deref(), Some("disk=cut | run C=14"));
        assert_eq!(a.who.as_deref(), Some(""));
        let e = entry("f1:run-error:00", Some(&run()), "x", None, "d").unwrap_err();
        assert!(e.contains("matches 2 findings"), "{e}");
        let e = entry("f1:nope", Some(&run()), "x", None, "d").unwrap_err();
        assert!(e.contains("the run's findings"), "{e}");
        assert!(entry("f1:invariant:5ef675c8c9", None, " ", None, "d").is_err());
        let a = entry("f1:invariant:aaaaaaaaaa", None, "x", Some("Team A"), "d").unwrap();
        assert_eq!((a.schedule, a.who.as_deref()), (None, Some("Team A")));
    }
}
