//! The history store (stage 2 of docs/design/ci.md, sections 4.4 and 4.5), in its first form: a
//! directory, usually a checkout of a branch such as `hydo-history` that the team's workflow
//! commits and pushes. hydo only reads and writes files there; it never calls git or a CI
//! platform.
//!
//! ```text
//! v1/<project>/runs/<ci-run id>.json    every recorded ci-run.json
//! v1/<project>/findings.jsonl           the ledger: one finding-event per change of state
//! v1/<project>/state.json               the fold of the ledger (ci-ledger), per branch
//! ```
//!
//! - `hydo ci record <ci-run.json> --store DIR [--branch NAME]` adds a run. A finding that failed
//!   in the run is `new` on the branch the first time, `reappeared` when the ledger had it fixed;
//!   one the run proved fixed by replay becomes `fixed`. A finding the run did not find keeps its
//!   state: absence after a bounded search proves nothing. Only runs of the branch's own line
//!   are recorded (a push or a scheduled run); a pull request's run needs `--branch`.
//! - `hydo ci run --history DIR` reads the branch's ledger (the pull request's target branch, or
//!   the run's own branch): every open finding that neither exploration found is replayed here,
//!   so a finding of a nightly run is checked on every pull request, and a finding the ledger has
//!   fixed that this change brings back is `reappeared`.

use super::run::git_branch_name;
use hydo_results::{
    ci_state, CiFinding, CiFindingEvent, CiHistory, CiLedger, CiLedgerFinding, CiRun,
};
use std::path::{Path, PathBuf};

/// A file or directory name made from any text: `[A-Za-z0-9._-]`, the rest as `_`.
pub fn safe_name(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if t.is_empty() || t.starts_with('.') {
        format!("_{t}")
    } else {
        t
    }
}

fn project_dir(store: &Path, project: &str) -> PathBuf {
    store.join("v1").join(safe_name(project))
}

/// The ledger of a project, empty when the store has none yet.
pub fn read_ledger(store: &Path, project: &str) -> Result<CiLedger, String> {
    let p = project_dir(store, project).join("state.json");
    if !p.exists() {
        return Ok(CiLedger {
            project: project.into(),
            ..CiLedger::default()
        });
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    match hydo_results::reader::document(&text).map_err(|e| format!("{}: {e}", p.display()))? {
        hydo_results::reader::Record::CiLedger(l) => Ok(*l),
        _ => Err(format!("{}: not a ci-ledger document", p.display())),
    }
}

/// A branch's ledger, as `hydo ci run --history` reads it.
pub struct Hist {
    pub store: String,
    pub branch: String,
    pub ledger: CiLedger,
}

impl Hist {
    fn findings(&self) -> impl Iterator<Item = &CiLedgerFinding> {
        self.ledger
            .branches
            .get(&self.branch)
            .into_iter()
            .flat_map(|b| b.findings.values())
    }

    /// The open findings, as findings of a base to replay here.
    pub fn open(&self) -> Vec<CiFinding> {
        self.findings()
            .filter(|f| f.state == "open")
            .map(|f| CiFinding {
                key: f.key.clone(),
                kind: f.kind.clone(),
                title: f.title.clone(),
                state: ci_state::PRESENT.into(),
                minimal: f.minimal.clone(),
                schedules: if f.schedules.is_empty() {
                    vec![f.minimal.clone()]
                } else {
                    f.schedules.clone()
                },
                ..CiFinding::default()
            })
            .collect()
    }

    /// Where the ledger recorded a key fixed, when it did.
    pub fn fixed_at(&self, key: &str) -> Option<String> {
        self.findings()
            .find(|f| f.key == key && f.state == "fixed")
            .map(|f| {
                f.fixed_at
                    .clone()
                    .unwrap_or_else(|| "an earlier commit".into())
            })
    }

    pub fn summary(&self) -> CiHistory {
        CiHistory {
            store: self.store.clone(),
            branch: Some(self.branch.clone()),
            open: self.findings().filter(|f| f.state == "open").count() as u64,
            fixed: self.findings().filter(|f| f.state == "fixed").count() as u64,
        }
    }
}

/// The ledger of `branch` in the store (an empty one when the store has none).
pub fn load(store: &Path, shown: &str, project: &str, branch: &str) -> Result<Hist, String> {
    let ledger = read_ledger(store, project)?;
    Ok(Hist {
        store: shown.into(),
        branch: branch.into(),
        ledger,
    })
}

/// A finding's state without its acceptance.
fn underlying(f: &CiFinding) -> &str {
    match &f.accepted {
        Some(a) if f.state == ci_state::ACCEPTED => &a.state,
        _ => &f.state,
    }
}

/// Whether a run saw the finding failing with its key: found by its exploration, or a replay of
/// its schedule failed the same way.
fn seen_failing(f: &CiFinding) -> bool {
    let s = underlying(f);
    match s {
        ci_state::NEW | ci_state::PRE_EXISTING | ci_state::REAPPEARED => true,
        ci_state::PRESENT => true,
        ci_state::CHANGED => f.source.as_deref().is_none_or(|x| x == "explored"),
        _ => false,
    }
}

/// Fold one run into the ledger of `branch`; the events of what changed.
pub fn fold(ledger: &mut CiLedger, run: &CiRun, branch: &str) -> Vec<CiFindingEvent> {
    let at = run.commit.clone().map(|c| c[..c.len().min(12)].to_string());
    let b = ledger.branches.entry(branch.to_string()).or_default();
    let mut events = Vec::new();
    let engine = run.engine.backend.as_ref().map(|be| {
        format!(
            "{be}/{}",
            run.engine
                .host_cpu
                .as_ref()
                .map(|c| c.vendor.as_str())
                .unwrap_or("-")
        )
    });
    let ev = |f: &CiFinding, what: &str| CiFindingEvent {
        key: f.key.clone(),
        project: run.project.name.clone(),
        branch: branch.into(),
        event: what.into(),
        commit: run.commit.clone(),
        ci_run: run.id.clone(),
        title: f.title.clone(),
        minimal: f.minimal.clone(),
        proof: f.proof.clone(),
        engine: engine.clone(),
        hydo: Some(run.hydo.version.clone()),
        image_input_key: run.image.input_key.clone(),
        ..CiFindingEvent::default()
    };
    for f in &run.findings {
        let accepted = f.state == ci_state::ACCEPTED;
        if seen_failing(f) {
            match b.findings.get_mut(&f.key) {
                None => {
                    events.push(ev(f, "new"));
                    if accepted {
                        events.push(ev(f, "accepted"));
                    }
                    b.findings.insert(
                        f.key.clone(),
                        CiLedgerFinding {
                            key: f.key.clone(),
                            kind: f.kind.clone(),
                            title: f.title.clone(),
                            state: "open".into(),
                            accepted,
                            minimal: f.minimal.clone(),
                            schedules: f.schedules.clone(),
                            first_seen: at.clone(),
                            last_seen: at.clone(),
                            fixed_at: None,
                            seen: 1,
                        },
                    );
                }
                Some(e) => {
                    if e.state == "fixed" {
                        events.push(ev(f, "reappeared"));
                        e.state = "open".into();
                    }
                    if accepted && !e.accepted {
                        events.push(ev(f, "accepted"));
                    } else if !accepted && e.accepted {
                        events.push(ev(f, "unaccepted"));
                    }
                    e.accepted = accepted;
                    e.title = f.title.clone();
                    for s in &f.schedules {
                        if !e.schedules.contains(s) {
                            e.schedules.push(s.clone());
                        }
                    }
                    e.last_seen = at.clone();
                    e.seen += 1;
                }
            }
        } else if underlying(f) == ci_state::FIXED {
            if let Some(e) = b.findings.get_mut(&f.key).filter(|e| e.state == "open") {
                events.push(ev(f, "fixed"));
                e.state = "fixed".into();
                e.fixed_at = at.clone();
            }
        }
    }
    b.runs += 1;
    b.last_run = Some(run.id.clone());
    b.last_commit = at;
    events
}

/// `hydo ci record`: add a run to the store.
pub fn record(store: &Path, run: &CiRun, branch: &str) -> Result<Vec<CiFindingEvent>, String> {
    let dir = project_dir(store, &run.project.name);
    let runs = dir.join("runs");
    std::fs::create_dir_all(&runs).map_err(|e| format!("{}: {e}", runs.display()))?;
    let mut ledger = read_ledger(store, &run.project.name)?;
    let file = runs.join(format!("{}.json", safe_name(&run.id)));
    if file.exists() {
        return Err(format!(
            "{}: the run {} is already recorded",
            file.display(),
            run.id
        ));
    }
    let events = fold(&mut ledger, run, branch);
    hydo_results::write_document(&file, run).map_err(|e| format!("{}: {e}", file.display()))?;
    let jl = dir.join("findings.jsonl");
    let mut text = std::fs::read_to_string(&jl).unwrap_or_default();
    for e in &events {
        text.push_str(&serde_json::to_string(e).map_err(|e| e.to_string())?);
        text.push('\n');
    }
    std::fs::write(&jl, text).map_err(|e| format!("{}: {e}", jl.display()))?;
    let st = dir.join("state.json");
    hydo_results::write_document(&st, &ledger).map_err(|e| format!("{}: {e}", st.display()))?;
    Ok(events)
}

pub fn cmd_record(argv: &[String]) -> i32 {
    use crate::ctx::{die, take};
    let mut args = argv.to_vec();
    let store = take(&mut args, "--store")
        .unwrap_or_else(|| die("usage: hydo ci record <ci-run.json> --store DIR [--branch NAME]"));
    let branch_flag = take(&mut args, "--branch");
    if args.len() != 1 {
        die("usage: hydo ci record <ci-run.json> --store DIR [--branch NAME]");
    }
    let p = Path::new(&args[0]);
    let p = if p.is_dir() {
        p.join("ci-run.json")
    } else {
        p.to_path_buf()
    };
    let run = super::read_ci_run(&p).unwrap_or_else(|e| die(&e));
    if run.trigger == "pull_request" && branch_flag.is_none() {
        die("hydo ci record: a pull request's run is recorded only with --branch NAME (the history is the line of a branch: pushes to it and its scheduled runs; docs/design/ci.md section 4.3)");
    }
    if !run.shards.is_empty() || run.budget.shard.is_none() {
        // a merged run, or a run without shards
    } else {
        die("hydo ci record: this is one shard's run; record the merged run (hydo ci merge)");
    }
    let branch = branch_flag
        .or(run.branch.clone())
        .or_else(|| git_branch_name(Path::new(".")))
        .unwrap_or_else(|| die("hydo ci record: the run names no branch; give --branch NAME"));
    let store = PathBuf::from(store);
    match record(&store, &run, &branch) {
        Ok(ev) => {
            println!(
                "hydo ci record: {} on {branch} into {}: {}",
                run.id,
                store.display(),
                if ev.is_empty() {
                    "no finding changed state".to_string()
                } else {
                    ev.iter()
                        .map(|e| format!("{} {}", e.event, e.title))
                        .collect::<Vec<_>>()
                        .join("; ")
                }
            );
            0
        }
        Err(e) => die(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(key: &str, state: &str) -> CiFinding {
        CiFinding {
            key: key.into(),
            kind: "invariant".into(),
            title: format!("VIOLATION [{key}]"),
            state: state.into(),
            minimal: format!("disk=none | run K={key}@1+1"),
            schedules: vec![format!("disk=none | run K={key}@1+1")],
            source: Some("explored".into()),
            ..CiFinding::default()
        }
    }

    fn run(id: &str, commit: &str, fs: Vec<CiFinding>) -> CiRun {
        CiRun {
            id: id.into(),
            commit: Some(commit.into()),
            project: hydo_results::CiProject {
                name: "redis".into(),
                ..Default::default()
            },
            findings: fs,
            ..CiRun::default()
        }
    }

    /// The ledger records state changes only, and absence never fixes a finding.
    #[test]
    fn the_ledger_folds_runs_by_replay_not_absence() {
        let mut l = CiLedger::default();
        let e = fold(
            &mut l,
            &run("r1", "c1", vec![f("a", "new"), f("b", "new")]),
            "main",
        );
        assert_eq!(
            e.iter().map(|x| x.event.as_str()).collect::<Vec<_>>(),
            ["new", "new"]
        );
        // b not found (absence): stays open; a found again: no event
        let e = fold(&mut l, &run("r2", "c2", vec![f("a", "present")]), "main");
        assert!(e.is_empty());
        assert_eq!(l.branches["main"].findings["b"].state, "open");
        assert_eq!(l.branches["main"].findings["a"].seen, 2);
        // b proven fixed by replay
        let mut fb = f("b", "fixed");
        fb.source = Some("history".into());
        let e = fold(&mut l, &run("r3", "c3", vec![fb]), "main");
        assert_eq!(e[0].event, "fixed");
        assert_eq!(
            l.branches["main"].findings["b"].fixed_at.as_deref(),
            Some("c3")
        );
        // b again: reappeared; a accepted
        let mut fa = f("a", "accepted");
        fa.accepted = Some(hydo_results::CiAccepted {
            reason: "x".into(),
            state: "present".into(),
            ..Default::default()
        });
        let e = fold(&mut l, &run("r4", "c4", vec![f("b", "new"), fa]), "main");
        assert_eq!(
            e.iter().map(|x| x.event.as_str()).collect::<Vec<_>>(),
            ["reappeared", "accepted"]
        );
        let h = Hist {
            store: "s".into(),
            branch: "main".into(),
            ledger: l.clone(),
        };
        assert_eq!(h.open().len(), 2);
        assert_eq!(h.fixed_at("b"), None);
        assert_eq!(h.summary().open, 2);
        // another branch has its own line
        assert!(fold(&mut l, &run("r5", "c5", vec![f("a", "new")]), "release")[0].event == "new");
    }

    #[test]
    fn names_are_safe() {
        assert_eq!(safe_name("github:1187/2"), "github_1187_2");
        assert_eq!(safe_name("../x"), "_.._x");
    }
}
