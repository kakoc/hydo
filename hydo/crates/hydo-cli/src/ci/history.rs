//! The history store (stage 2 of docs/design/ci.md, sections 4.4 and 4.5), in its first form: a
//! directory, usually a checkout of a branch such as `hydo-history` that the team's workflow
//! commits and pushes. hydo only reads and writes files there; it never calls git or a CI
//! platform.
//!
//! ```text
//! v1/<project>/runs/<ci-run id>.json    every recorded ci-run.json
//! v1/<project>/findings.jsonl           the ledger: one finding-event per change of state
//! v1/<project>/state.json               the fold of the ledger (ci-ledger), per branch, with
//!                                       the runs in the order they were recorded (M286)
//! v1/<project>/reports/<ci-run id>.html the run's own report page (`--report`, M286)
//! index.html                            the history page (`hydo ci page`, M286)
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
pub fn seen_failing(f: &CiFinding) -> bool {
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

/// Now in UTC, as `2026-11-17T10:04:00Z`.
pub fn utc_now() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    utc(t)
}

/// Seconds since 1970 in UTC, as `2026-11-17T10:04:00Z` (the civil calendar from days).
pub fn utc(t: u64) -> String {
    let (days, rest) = ((t / 86_400) as i64, t % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// The project names a store holds (`v1/<name>/`).
pub fn projects(store: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(store.join("v1"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    v.sort();
    v
}

/// One recorded run, read back.
pub struct Recorded {
    pub branch: String,
    pub entry: hydo_results::CiLedgerRun,
    pub run: CiRun,
}

/// The project's recorded runs, per branch in the order they were recorded. A store written
/// before Milestone 286 has no order in its ledger: its run files are then taken in name order,
/// each on the branch its run names.
pub fn recorded_runs(store: &Path, project: &str) -> Result<Vec<Recorded>, String> {
    let dir = project_dir(store, project);
    let ledger = read_ledger(store, project)?;
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (b, br) in &ledger.branches {
        for e in &br.recorded {
            let p = dir.join(&e.file);
            let run = super::read_ci_run(&p)?;
            seen.insert(e.file.clone());
            out.push(Recorded {
                branch: b.clone(),
                entry: e.clone(),
                run,
            });
        }
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("runs"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    for p in files {
        let file = format!(
            "runs/{}",
            p.file_name().and_then(|n| n.to_str()).unwrap_or_default()
        );
        if seen.contains(&file) {
            continue;
        }
        let run = super::read_ci_run(&p)?;
        out.push(Recorded {
            branch: run.branch.clone().unwrap_or_else(|| "(no branch)".into()),
            entry: hydo_results::CiLedgerRun {
                id: run.id.clone(),
                file,
                commit: run.commit.clone(),
                ..Default::default()
            },
            run,
        });
    }
    Ok(out)
}

/// What recorded runs measured: the seconds per seed of the exploration of the head and of the
/// base (when the base was explored), over the latest runs (at most `last`) of the same engine,
/// strategy and options; `None` when no recorded run measured it.
pub struct Measured {
    pub head_per_seed: f64,
    pub base_per_seed: Option<f64>,
    pub runs: u64,
}

pub fn measured(
    store: &Path,
    project: &str,
    backend: &str,
    strategy: &str,
    options: &[String],
    last: usize,
) -> Result<Option<Measured>, String> {
    let mut runs = recorded_runs(store, project)?;
    // the latest first: the recording time when the ledger has it, else the file order
    runs.reverse();
    runs.sort_by(|a, b| b.entry.recorded.cmp(&a.entry.recorded));
    let (mut hs, mut hn, mut bs, mut bn, mut n) = (0.0, 0u64, 0.0, 0u64, 0u64);
    for r in runs.iter().filter(|r| {
        r.run.engine.backend.as_deref() == Some(backend)
            && r.run.budget.strategy == strategy
            && r.run.budget.options == options
    }) {
        let b = &r.run.budget;
        let seeds = b
            .completed
            .unwrap_or_else(|| super::seeds::count(&super::seeds::parse_ranges(&b.seeds)));
        let Some(e) = b.explore_s.filter(|_| seeds > 0) else {
            continue;
        };
        hs += e;
        hn += seeds;
        if let Some(x) = b.base_explore_s {
            bs += x;
            bn += seeds;
        }
        n += 1;
        if n as usize >= last {
            break;
        }
    }
    Ok((hn > 0).then(|| Measured {
        head_per_seed: hs / hn as f64,
        base_per_seed: (bn > 0).then(|| bs / bn as f64),
        runs: n,
    }))
}

/// `hydo ci record`: add a run to the store, with the run's report page (HTML) to keep there.
pub fn record(
    store: &Path,
    run: &CiRun,
    branch: &str,
    report: Option<&str>,
) -> Result<Vec<CiFindingEvent>, String> {
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
    let name = safe_name(&run.id);
    let report = match report {
        Some(html) => {
            let rd = dir.join("reports");
            std::fs::create_dir_all(&rd).map_err(|e| format!("{}: {e}", rd.display()))?;
            let p = rd.join(format!("{name}.html"));
            std::fs::write(&p, html).map_err(|e| format!("{}: {e}", p.display()))?;
            Some(format!("reports/{name}.html"))
        }
        None => None,
    };
    if let Some(b) = ledger.branches.get_mut(branch) {
        b.recorded.push(hydo_results::CiLedgerRun {
            id: run.id.clone(),
            file: format!("runs/{name}.json"),
            commit: run.commit.clone(),
            recorded: Some(utc_now()),
            report,
        });
    }
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
    let store = take(&mut args, "--store").unwrap_or_else(|| {
        die("usage: hydo ci record <ci-run.json> --store DIR [--branch NAME] [--report]")
    });
    let branch_flag = take(&mut args, "--branch");
    let with_report = args.iter().any(|a| a == "--report");
    args.retain(|a| a != "--report");
    if args.len() != 1 {
        die("usage: hydo ci record <ci-run.json> --store DIR [--branch NAME] [--report]");
    }
    let given = Path::new(&args[0]);
    let p = if given.is_dir() {
        given.join("ci-run.json")
    } else {
        given.to_path_buf()
    };
    let run = super::read_ci_run(&p).unwrap_or_else(|e| die(&e));
    if run.trigger == "pull_request" && branch_flag.is_none() {
        die("hydo ci record: a pull request's run is recorded only with --branch NAME (the history is the line of a branch: pushes to it and its scheduled runs; `hydo guide ci`, section 12, \"History across runs\")");
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
    // the run's own page, made here from its files (never a page the run brought: a pull
    // request's files are data)
    let page = with_report.then(|| {
        let d = p.parent().unwrap_or(Path::new("."));
        crate::htmlreport::ci_dir_page(d, &args[0]).unwrap_or_else(|e| die(&e))
    });
    match record(&store, &run, &branch, page.as_deref()) {
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
