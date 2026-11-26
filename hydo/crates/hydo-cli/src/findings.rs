//! `hydo findings` (Milestone 365): the findings store for every run, not only for CI.
//!
//! The store is the history store of `hydo ci record` (src/ci/history.rs), in the same format:
//! `v1/<project>/runs/<id>.json` (one `ci-run` document per recorded run), `findings.jsonl` (the
//! ledger of state changes) and `state.json` (its fold). `hydo findings record` turns the results
//! of a plain `hydo run` or `hydo explore` into such a `ci-run` document (`ci.name` `local`,
//! `trigger` `manual`, `recorded_from` the command) and records it with the same fold, so `hydo
//! ci page` and `hydo ci run --history` read those records too.
//!
//! - A finding seen failing is `new` the first time on its branch, and seen again (one more
//!   sighting, never a second entry) after that. A finding a replay proved fixed is `fixed`: a
//!   `hydo run` of one of its recorded schedules that passes is that replay. A finding that a run
//!   did not find keeps its state: absence after a bounded search proves nothing.
//! - Recording the same run directory again changes nothing: a record's id is made from the
//!   bytes of its `result.json` or `exploration.json`.
//! - A record holds small values only, and no path of this machine: the image and the run
//!   directory by their file names, no engine arguments, no command line of the explorer.
//! - `hydo findings list|show|history` read the store: the findings of each branch, and each
//!   finding's line of recorded runs (`--json`: the results document `findings`).

use crate::ci::history::{self, safe_name, Recorded};
use crate::ci::{key, run as cirun};
use crate::ctx::{die, take, Ctx};
use hydo_results::{
    ci_state, CiArtifacts, CiFinding, CiHydo, CiImage, CiLedger, CiOutcome, CiProject, CiReplay,
    CiRun, CiSummary, CiSystem, Engine, Exploration, FindingPoint, FindingSighting, FindingsDoc,
    FindingsFilter, StoredFinding,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const HELP: &str = r#"hydo findings -- the findings of your runs, kept in one store (hydo guide workflow)

    hydo findings record [RUN_DIR | EXPLORE_DIR] [--store DIR] [--project NAME] [--commit SHA]
                         [--branch NAME] [--known FILE]
        record the findings of a hydo run or a hydo explore (default hydo-out/explore) in the
        store: each finding's key, kind, title, minimal schedule, fingerprint, verdict, the image's
        sha256, hydo's version and epoch, the engine and the time. A finding seen again adds a
        sighting, never a second entry. A hydo run of a recorded schedule that passes proves its
        finding fixed. Recording one directory twice changes nothing.
        --project: the name of the project (default: the name in the manifest the image was
        built from, else in ./hydo/hydo.yaml or ./hydo.yaml, else the image's file name).
        --commit and --branch: default the repository's HEAD and branch of this directory
        (branch `local` outside a repository). --known: the accepted findings (default
        hydo/known-findings.yaml, when it exists).

    hydo findings list [--store DIR] [--project NAME] [--branch NAME] [--state open|fixed|accepted|all]
                       [--kind KIND] [--class CLASS] [--since DATE] [--json]
        the findings of the store, one per branch: the open ones by default, with when each was
        first and last seen, how often, and its minimal schedule. --since 2026-11-26 keeps those
        last seen on that day or later.

    hydo findings show KEY [--store DIR] [--project NAME] [--json]
        one finding (KEY or a unique part of it): its verdict, schedules, the replay line, and on
        each branch its state and its last sighting's image, hydo, epoch, engine and fingerprint

    hydo findings history [KEY] [--store DIR] [--project NAME] [--branch NAME] [--json]
        each finding's line of recorded runs, oldest first: new, seen failing again, not found
        (still open), fixed by replay, back again, accepted

The store: --store, else $HYDO_FINDINGS_STORE, else ~/.hydo/findings. It has the format of the
history store of hydo ci record, so hydo ci page --store DIR shows these findings too, and hydo
ci run --history DIR replays them. hydo run and hydo explore record at the end with
--record-findings. --json prints one results document of type findings
(hydo/schema/results-v1.json).
"#;

pub fn cmd_findings(ctx: &Ctx, argv: &[String]) -> i32 {
    match argv.first().map(String::as_str) {
        Some("record") => cmd_record(ctx, &argv[1..]),
        Some(v @ ("list" | "show" | "history")) => cmd_view(ctx, v, &argv[1..]),
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

/// The store a command uses, and how to name it in its output.
pub fn store_of(ctx: &Ctx, flag: Option<String>) -> PathBuf {
    match flag {
        Some(s) => PathBuf::from(s),
        None => ctx.config.cli.findings_store.clone(),
    }
}

/// What `hydo findings record` was told, or found out, about the run.
#[derive(Clone, Debug, Default)]
pub struct Opts {
    pub project: Option<String>,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub known: Option<PathBuf>,
}

// ------------------------------------------------------------------ record

fn sha_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// A path's last part: what a record keeps of a path of this machine.
fn file_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(p)
        .to_string()
}

/// A word for the shell: as it is when it needs no quotes, else in single quotes.
fn sh_word(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@+,".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The replay line of a schedule on an image, by the image's file name.
pub fn repro_line(image: Option<&str>, schedule: &str) -> String {
    match image {
        Some(i) if !i.is_empty() => format!("hydo run {} {}", sh_word(i), sh_word(schedule)),
        _ => format!("hydo run {}", sh_word(schedule)),
    }
}

/// The engine as a record keeps it: no arguments (they name files of this machine), and the
/// guest kernel by its file name.
fn scrub_engine(e: &Engine) -> Engine {
    Engine {
        args: None,
        guest_kernel: e.guest_kernel.as_deref().map(|k| {
            let mut w = k.split_whitespace();
            let first = w.next().map(file_name).unwrap_or_default();
            std::iter::once(first)
                .chain(w.map(str::to_string))
                .collect::<Vec<_>>()
                .join(" ")
        }),
        ..e.clone()
    }
}

/// The name in a manifest file, when it reads.
fn manifest_name(p: &Path) -> Option<String> {
    let t = std::fs::read_to_string(p).ok()?;
    let v = hydo_manifest::parse_text(&t, &p.display().to_string(), false).ok()?;
    v.get("name").and_then(|n| n.as_str()).map(str::to_string)
}

/// The project's name: the manifest the image was built from (its build record
/// `<image>.json`), else the manifest of this directory, else the image's file name without
/// its suffix.
pub fn project_name(image: &str, base: &Path) -> String {
    for cand in [PathBuf::from(image), base.join(image)] {
        let rec = PathBuf::from(format!("{}.json", cand.display()));
        let Ok(t) = std::fs::read_to_string(&rec) else {
            continue;
        };
        let Ok(b) = hydo_image::parse_build(&t) else {
            continue;
        };
        let m = PathBuf::from(&b.manifest.path);
        for mp in [m.clone(), base.join(&m)] {
            if let Some(n) = manifest_name(&mp) {
                return n;
            }
        }
    }
    for m in ["hydo/hydo.yaml", "hydo.yaml"] {
        if let Some(n) = manifest_name(Path::new(m)) {
            return n;
        }
    }
    let f = file_name(image);
    let stem = ["-arm64.cpio.gz", "-disk.img", ".cpio.gz", ".img"]
        .iter()
        .find_map(|s| f.strip_suffix(s))
        .unwrap_or(&f);
    if stem.is_empty() {
        "project".into()
    } else {
        stem.to_string()
    }
}

/// The state of a finding seen failing, by the branch's ledger: `new` the first time,
/// `present` while it is open, `reappeared` after a replay proved it fixed.
fn state_by_ledger(f: &mut CiFinding, ledger: &CiLedger, branch: &str) {
    let e = ledger
        .branches
        .get(branch)
        .and_then(|b| b.findings.get(&f.key));
    let state = match e {
        None => ci_state::NEW,
        Some(e) if e.state == "fixed" => {
            f.fixed_at = e.fixed_at.clone();
            ci_state::REAPPEARED
        }
        Some(_) => ci_state::PRESENT,
    };
    f.state = state.into();
    f.proof = Some("no-base".into());
}

/// The verdict line that showed an exploration's finding: its minimal schedule's confirmation,
/// else the failing first run's.
fn verdict_of(doc: &Exploration, minimal: &str) -> Option<String> {
    doc.shrinks
        .iter()
        .find(|s| s.minimal == minimal)
        .map(|s| {
            s.confirmations
                .first()
                .and_then(|c| c.verdict.clone())
                .or_else(|| s.min_verdict.clone())
                .unwrap_or_else(|| s.verdict.clone())
        })
        .or_else(|| {
            doc.failures
                .iter()
                .find(|f| f.run.schedule == minimal)
                .map(|f| f.run.verdict.text.clone())
        })
}

/// A file's modification time in UTC, or now.
fn mtime_utc(p: &Path) -> String {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| history::utc(d.as_secs()))
        .unwrap_or_else(history::utc_now)
}

/// The record of a run or exploration directory (`hydo findings record`), its findings' states
/// decided by the branch's ledger.
pub fn local_run(
    dir: &Path,
    o: &Opts,
    project: &str,
    branch: &str,
    ledger: &CiLedger,
    known: &hydo_manifest::known_findings::KnownFindings,
) -> Result<CiRun, String> {
    let results = hydo_results::read_dir(dir).map_err(|e| e.to_string())?;
    let (doc_path, from) = match &results {
        hydo_results::Results::Run { path, .. } => (path.clone(), "hydo run"),
        hydo_results::Results::Exploration { path, .. } => (path.clone(), "hydo explore"),
    };
    let bytes = std::fs::read(&doc_path).map_err(|e| format!("{}: {e}", doc_path.display()))?;
    let dir_name = dir
        .canonicalize()
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| file_name(&dir.display().to_string()));
    let mut r = CiRun {
        id: format!("local:{}", &sha_hex(&bytes)[..16]),
        started: Some(mtime_utc(&doc_path)),
        ci: CiSystem {
            name: "local".into(),
            ..CiSystem::default()
        },
        project: CiProject {
            name: project.into(),
            dir: ".".into(),
            ..CiProject::default()
        },
        trigger: "manual".into(),
        commit: o.commit.clone(),
        branch: Some(branch.into()),
        recorded_from: Some(from.into()),
        ..CiRun::default()
    };
    let mut found: Vec<CiFinding>;
    match results {
        hydo_results::Results::Exploration { doc: None, .. } => {
            return Err(format!(
                "{}: the exploration has no exploration.json yet (it still runs, or it stopped \
                 before its end)",
                dir.display()
            ));
        }
        hydo_results::Results::Exploration { doc: Some(doc), .. } => {
            let image = file_name(&doc.image.path);
            let (fs, nondet, _) = cirun::findings_of(&doc);
            found = fs;
            for f in &mut found {
                f.verdict = verdict_of(&doc, &f.minimal);
                f.repro = Some(repro_line(Some(&image), &f.minimal));
                f.source = Some("explored".into());
            }
            let (coverage, runs) = cirun::coverage(&doc, dir);
            r.coverage = coverage;
            r.nondeterminism = nondet;
            r.hydo = CiHydo {
                version: doc
                    .hydo_version
                    .clone()
                    .unwrap_or_else(|| hydo_image::hydo_version().into()),
                commit: None,
                epoch: doc.epoch.unwrap_or(1),
            };
            r.engine = scrub_engine(&doc.engine);
            r.image = CiImage {
                sha256: doc.image.sha256.clone(),
                path: Some(image),
                ..CiImage::default()
            };
            r.project.manifest_sha256 = doc.image.manifest_sha256.clone();
            r.budget.mode = "explore".into();
            r.budget.seeds = doc
                .config
                .seeds
                .clone()
                .or_else(|| doc.config.schedule.as_ref().map(|_| "schedule".into()))
                .unwrap_or_default();
            r.budget.strategy = doc.config.strategy.clone();
            r.budget.shard = doc
                .config
                .shard
                .as_ref()
                .map(|s| format!("{}/{}", s.index, s.count));
            r.budget.range = doc.config.shard.as_ref().map(|s| s.range.clone());
            r.summary = CiSummary {
                runs: doc.summary.runs.unwrap_or(runs.max(doc.summary.schedules)),
                failures: doc.failures.len() as u64,
                ..CiSummary::default()
            };
            r.artifacts.exploration = Some(dir_name);
        }
        hydo_results::Results::Run { run, .. } => {
            let image = run.image.as_ref().map(|i| file_name(&i.path));
            found = Vec::new();
            if let Some((kind, title, k)) = key::of_verdict(&run.verdict.text, &run.findings) {
                found.push(CiFinding {
                    key: k,
                    kind,
                    title,
                    minimal: run.schedule.clone(),
                    schedules: vec![run.schedule.clone()],
                    fp: Some(run.fingerprints.fp.clone()),
                    classes: run.class.clone().into_iter().collect(),
                    repro: Some(repro_line(image.as_deref(), &run.schedule)),
                    source: Some("run".into()),
                    verdict: Some(run.verdict.text.clone()),
                    ..CiFinding::default()
                });
            }
            for f in run.findings_of(hydo_results::kind::ERROR_LOG) {
                if f.status != hydo_results::status::NEW {
                    continue;
                }
                let svc = f.details.get("service").and_then(|s| s.as_str());
                found.push(CiFinding {
                    key: key::key(hydo_results::kind::ERROR_LOG, &f.id),
                    kind: hydo_results::kind::ERROR_LOG.into(),
                    title: match svc {
                        Some(s) => format!("[{s}] {}", f.message),
                        None => f.message.clone(),
                    },
                    minimal: run.schedule.clone(),
                    schedules: vec![run.schedule.clone()],
                    repro: Some(repro_line(image.as_deref(), &run.schedule)),
                    source: Some("run".into()),
                    ..CiFinding::default()
                });
            }
            // a replay: the open findings of the branch whose recorded schedule this is
            let keys: Vec<String> = found.iter().map(|f| f.key.clone()).collect();
            let passed = run.verdict.kind == hydo_results::status::OK;
            if let Some(b) = ledger.branches.get(branch) {
                for e in b.findings.values().filter(|e| e.state == "open") {
                    if keys.contains(&e.key)
                        || !(e.minimal == run.schedule || e.schedules.contains(&run.schedule))
                    {
                        continue;
                    }
                    let (state, proof) = if passed {
                        (ci_state::FIXED, "head-replay-ok")
                    } else {
                        (ci_state::CHANGED, "head-replay-other")
                    };
                    found.push(CiFinding {
                        key: e.key.clone(),
                        kind: e.kind.clone(),
                        title: e.title.clone(),
                        state: state.into(),
                        proof: Some(proof.into()),
                        minimal: run.schedule.clone(),
                        schedules: vec![run.schedule.clone()],
                        fp: Some(run.fingerprints.fp.clone()),
                        repro: Some(repro_line(image.as_deref(), &run.schedule)),
                        replays: vec![CiReplay {
                            on: "head".into(),
                            schedule: run.schedule.clone(),
                            verdict: Some(run.verdict.text.clone()),
                            fp: Some(run.fingerprints.fp.clone()),
                            key: keys.first().cloned(),
                            ..CiReplay::default()
                        }],
                        source: Some("history".into()),
                        verdict: Some(run.verdict.text.clone()),
                        ..CiFinding::default()
                    });
                }
            }
            r.hydo = CiHydo {
                version: run
                    .hydo_version
                    .clone()
                    .unwrap_or_else(|| hydo_image::hydo_version().into()),
                commit: None,
                epoch: run.epoch.unwrap_or(1),
            };
            r.engine = run.engine.as_ref().map(scrub_engine).unwrap_or_default();
            r.image = CiImage {
                sha256: run.image.as_ref().and_then(|i| i.sha256.clone()),
                path: image,
                ..CiImage::default()
            };
            r.project.manifest_sha256 = run.image.as_ref().and_then(|i| i.manifest_sha256.clone());
            r.budget.mode = "run".into();
            r.budget.seeds = "-".into();
            r.budget.strategy = "one schedule".into();
            r.summary = CiSummary {
                runs: 1,
                failures: u64::from(run.verdict.kind != hydo_results::status::OK),
                ..CiSummary::default()
            };
        }
    }
    for f in found.iter_mut().filter(|f| f.state.is_empty()) {
        state_by_ledger(f, ledger, branch);
        if let Some(a) = known.get(&f.key) {
            cirun::accept(f, a);
        }
    }
    let failing: Vec<&CiFinding> = found.iter().filter(|f| history::seen_failing(f)).collect();
    r.outcome = CiOutcome {
        exit: u64::from(!failing.is_empty()),
        fail_on: Vec::new(),
        failed_by: failing
            .iter()
            .map(|f| format!("{} {}", f.state, f.title))
            .collect(),
    };
    r.summary.findings = found.len() as u64;
    for f in &found {
        *r.summary.states.entry(f.state.clone()).or_default() += 1;
    }
    r.findings = found;
    r.artifacts = CiArtifacts {
        report: String::new(),
        junit: String::new(),
        ..r.artifacts
    };
    Ok(r)
}

/// What one record changed: the events of `hydo ci record`, and the findings seen again
/// without a change of state.
pub struct Recorded1 {
    pub id: String,
    pub project: String,
    pub branch: String,
    pub events: Vec<hydo_results::CiFindingEvent>,
    pub again: Vec<String>,
    /// The directory was recorded before; nothing changed.
    pub already: bool,
}

/// Record a run or exploration directory into the store.
pub fn record_dir(store: &Path, dir: &Path, o: &Opts) -> Result<Recorded1, String> {
    let results = hydo_results::read_dir(dir).map_err(|e| e.to_string())?;
    let image = match &results {
        hydo_results::Results::Run { run, .. } => run.image.as_ref().map(|i| i.path.clone()),
        hydo_results::Results::Exploration { doc, .. } => {
            doc.as_ref().map(|d| d.image.path.clone())
        }
    }
    .unwrap_or_default();
    let base = dir
        .canonicalize()
        .ok()
        .and_then(|d| d.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let project = o
        .project
        .clone()
        .unwrap_or_else(|| project_name(&image, &base));
    let here = Path::new(".");
    let commit = o
        .commit
        .clone()
        .or_else(|| cirun::git(here, &["rev-parse", "HEAD"]));
    let branch = o
        .branch
        .clone()
        .or_else(|| cirun::git_branch_name(here))
        .unwrap_or_else(|| "local".into());
    let known = match &o.known {
        Some(p) => {
            let t = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            hydo_manifest::known_findings::parse(&t, &p.display().to_string())
                .map_err(|e| e.message)?
        }
        None => {
            let p = Path::new(hydo_manifest::known_findings::DEFAULT_PATH);
            match std::fs::read_to_string(p) {
                Ok(t) => hydo_manifest::known_findings::parse(&t, &p.display().to_string())
                    .map_err(|e| e.message)?,
                Err(_) => Default::default(),
            }
        }
    };
    let ledger = history::read_ledger(store, &project)?;
    let opts = Opts {
        commit,
        ..o.clone()
    };
    let run = local_run(dir, &opts, &project, &branch, &ledger, &known)?;
    let file = store
        .join("v1")
        .join(safe_name(&project))
        .join("runs")
        .join(format!("{}.json", safe_name(&run.id)));
    if file.exists() {
        return Ok(Recorded1 {
            id: run.id,
            project,
            branch,
            events: Vec::new(),
            again: Vec::new(),
            already: true,
        });
    }
    let events = history::record(store, &run, &branch, None)?;
    let changed: std::collections::BTreeSet<&str> = events.iter().map(|e| e.key.as_str()).collect();
    let again = run
        .findings
        .iter()
        .filter(|f| history::seen_failing(f) && !changed.contains(f.key.as_str()))
        .map(|f| f.title.clone())
        .collect();
    Ok(Recorded1 {
        id: run.id,
        project,
        branch,
        events,
        again,
        already: false,
    })
}

/// The one line that says what a record did.
pub fn record_line(store: &Path, r: &Recorded1) -> String {
    if r.already {
        return format!(
            "hydo findings: {} of {} is already in {}; nothing changed",
            r.id,
            r.project,
            store.display()
        );
    }
    let mut parts: Vec<String> = r
        .events
        .iter()
        .map(|e| {
            let what = match e.event.as_str() {
                "reappeared" => "back again",
                "fixed" => "fixed by replay",
                o => o,
            };
            format!("{what} {}", e.title)
        })
        .collect();
    if !r.again.is_empty() {
        parts.push(format!("seen again {}", r.again.join("; ")));
    }
    format!(
        "hydo findings: recorded {} of {} on {} into {}: {}",
        r.id,
        r.project,
        r.branch,
        store.display(),
        if parts.is_empty() {
            "no finding".to_string()
        } else {
            parts.join("; ")
        }
    )
}

fn cmd_record(ctx: &Ctx, argv: &[String]) -> i32 {
    if crate::help::asked(argv) {
        print!("{HELP}");
        return 0;
    }
    let mut args = argv.to_vec();
    let store = store_of(ctx, take(&mut args, "--store"));
    let o = Opts {
        project: take(&mut args, "--project"),
        commit: take(&mut args, "--commit"),
        branch: take(&mut args, "--branch"),
        known: take(&mut args, "--known").map(PathBuf::from),
    };
    if args.len() > 1 || args.iter().any(|a| a.starts_with("--")) {
        die("usage: hydo findings record [RUN_DIR | EXPLORE_DIR] [--store DIR] [--project NAME] [--commit SHA] [--branch NAME] [--known FILE]");
    }
    let dir = args
        .first()
        .cloned()
        .unwrap_or_else(|| crate::ctx::pjoin(crate::ctx::OUT, "explore"));
    match record_dir(&store, Path::new(&dir), &o) {
        Ok(r) => {
            println!("{}", record_line(&store, &r));
            0
        }
        Err(e) => die(&format!("hydo findings record: {e}")),
    }
}

/// `--record-findings` of `hydo run` and `hydo explore`: record the directory into the store of
/// `HYDO_FINDINGS_STORE` (or `~/.hydo/findings`). A failure to record is said and does not change
/// the command's exit status.
pub fn record_after(ctx: &Ctx, dir: &Path, to_stderr: bool) {
    let store = store_of(ctx, None);
    let line = match record_dir(&store, dir, &Opts::default()) {
        Ok(r) => record_line(&store, &r),
        Err(e) => format!("hydo findings: not recorded: {e}"),
    };
    if to_stderr {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

// ------------------------------------------------------------------ read

/// The command that made a recorded run.
fn by_of(r: &CiRun) -> String {
    r.recorded_from
        .clone()
        .unwrap_or_else(|| "hydo ci run".into())
}

fn engine_of(r: &CiRun) -> Option<String> {
    r.engine.backend.as_ref().map(|be| {
        format!(
            "{be}/{}",
            r.engine
                .host_cpu
                .as_ref()
                .map(|c| c.vendor.as_str())
                .unwrap_or("-")
        )
    })
}

fn point_of(r: &Recorded) -> FindingPoint {
    FindingPoint {
        run: r.run.id.clone(),
        commit: r.run.commit.clone(),
        time: r.run.started.clone().or_else(|| r.entry.recorded.clone()),
    }
}

/// Every finding of a store's branches with its line of recorded runs, from a fold of the runs
/// in the order they were recorded (the fold of `hydo ci record`).
pub fn collect(store: &Path, project: Option<&str>) -> Result<Vec<StoredFinding>, String> {
    let mut out = Vec::new();
    for p in history::projects(store) {
        if project.is_some_and(|want| want != p && safe_name(want) != p) {
            continue;
        }
        let runs = history::recorded_runs(store, &p)?;
        let mut branches: Vec<String> = Vec::new();
        let mut by: BTreeMap<String, Vec<&Recorded>> = BTreeMap::new();
        for r in &runs {
            if !by.contains_key(&r.branch) {
                branches.push(r.branch.clone());
            }
            by.entry(r.branch.clone()).or_default().push(r);
        }
        for b in branches {
            out.extend(branch_line(&p, &b, &by[&b]));
        }
    }
    Ok(out)
}

fn branch_line(project: &str, branch: &str, runs: &[&Recorded]) -> Vec<StoredFinding> {
    let mut ledger = CiLedger::default();
    let mut order: Vec<String> = Vec::new();
    let mut fs: BTreeMap<String, StoredFinding> = BTreeMap::new();
    for r in runs {
        let events = history::fold(&mut ledger, &r.run, branch);
        let mut ev: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for e in &events {
            ev.entry(e.key.as_str()).or_default().push(e.event.as_str());
        }
        let at = point_of(r);
        let mut touched: std::collections::BTreeSet<String> = Default::default();
        for f in &r.run.findings {
            let es = ev.get(f.key.as_str()).cloned().unwrap_or_default();
            let failing = history::seen_failing(f);
            let fixed = es.contains(&"fixed");
            if !failing && !fixed {
                continue;
            }
            if !fs.contains_key(&f.key) {
                if !failing {
                    continue;
                }
                order.push(f.key.clone());
                fs.insert(
                    f.key.clone(),
                    StoredFinding {
                        key: f.key.clone(),
                        kind: f.kind.clone(),
                        project: project.into(),
                        branch: branch.into(),
                        first_seen: Some(at.clone()),
                        ..StoredFinding::default()
                    },
                );
            }
            touched.insert(f.key.clone());
            let s = fs.get_mut(&f.key).expect("a finding");
            let event = if fixed {
                "fixed"
            } else if es.contains(&"new") {
                "new"
            } else if es.contains(&"reappeared") {
                "back"
            } else {
                "failing"
            };
            let acceptance = es
                .iter()
                .find(|e| **e == "accepted" || **e == "unaccepted")
                .map(|e| e.to_string());
            s.sightings.push(FindingSighting {
                run: at.run.clone(),
                commit: at.commit.clone(),
                time: at.time.clone(),
                by: by_of(&r.run),
                event: event.into(),
                state: Some(f.state.clone()),
                acceptance,
                proof: f.proof.clone(),
                schedule: Some(f.minimal.clone()),
                fp: f.fp.clone(),
                verdict: f.verdict.clone(),
                image_sha256: r.run.image.sha256.clone(),
                hydo: Some(r.run.hydo.version.clone()),
                epoch: Some(r.run.hydo.epoch),
                engine: engine_of(&r.run),
            });
            if fixed {
                s.fixed_at = Some(at.clone());
                continue;
            }
            s.fixed_at = None;
            if event == "back" {
                s.reappeared = true;
            }
            s.last_seen = Some(at.clone());
            s.minimal = f.minimal.clone();
            s.verdict = f.verdict.clone().or_else(|| s.verdict.clone());
            s.fp = f.fp.clone();
            s.image_sha256 = r.run.image.sha256.clone();
            s.hydo = Some(r.run.hydo.version.clone());
            s.epoch = Some(r.run.hydo.epoch);
            s.engine = engine_of(&r.run);
            s.repro = f
                .repro
                .clone()
                .filter(|x| !x.contains('/') && !x.contains('`'))
                .or_else(|| Some(repro_line(r.run.image.path.as_deref(), &f.minimal)));
            for c in &f.classes {
                if !s.classes.contains(c) {
                    s.classes.push(c.clone());
                }
            }
        }
        // the open findings this run did not find: absence proves nothing
        for (k, s) in fs.iter_mut() {
            if touched.contains(k) {
                continue;
            }
            let open = ledger
                .branches
                .get(branch)
                .and_then(|b| b.findings.get(k))
                .is_some_and(|f| f.state == "open");
            if open {
                s.sightings.push(FindingSighting {
                    run: at.run.clone(),
                    commit: at.commit.clone(),
                    time: at.time.clone(),
                    by: by_of(&r.run),
                    event: "not-found".into(),
                    image_sha256: r.run.image.sha256.clone(),
                    hydo: Some(r.run.hydo.version.clone()),
                    epoch: Some(r.run.hydo.epoch),
                    engine: engine_of(&r.run),
                    ..FindingSighting::default()
                });
            }
        }
    }
    let lb = ledger.branches.get(branch);
    order
        .into_iter()
        .filter_map(|k| fs.remove(&k))
        .map(|mut s| {
            if let Some(l) = lb.and_then(|b| b.findings.get(&s.key)) {
                s.title = l.title.clone();
                s.state = l.state.clone();
                s.accepted = l.accepted;
                s.seen = l.seen;
                s.schedules = l.schedules.clone();
                if s.minimal.is_empty() {
                    s.minimal = l.minimal.clone();
                }
            }
            s
        })
        .collect()
}

/// Whether a finding passes the filter (but `key`).
fn keep(s: &StoredFinding, f: &FindingsFilter) -> bool {
    if f.branch.as_ref().is_some_and(|b| *b != s.branch) {
        return false;
    }
    match f.state.as_deref() {
        Some("open") if s.state != "open" => return false,
        Some("fixed") if s.state != "fixed" => return false,
        Some("accepted") if !s.accepted => return false,
        _ => {}
    }
    if f.kind.as_ref().is_some_and(|k| *k != s.kind) {
        return false;
    }
    if f.class.as_ref().is_some_and(|c| !s.classes.contains(c)) {
        return false;
    }
    if let Some(since) = &f.since {
        let last = s
            .sightings
            .iter()
            .filter(|x| x.event != "not-found")
            .filter_map(|x| x.time.as_deref())
            .max();
        if last.is_none_or(|t| t < since.as_str()) {
            return false;
        }
    }
    true
}

/// The findings whose key is `k` or holds it; Err when two different keys hold it.
fn by_key(all: Vec<StoredFinding>, k: &str) -> Result<Vec<StoredFinding>, String> {
    let exact: Vec<StoredFinding> = all.iter().filter(|s| s.key == k).cloned().collect();
    if !exact.is_empty() {
        return Ok(exact);
    }
    let m: Vec<StoredFinding> = all.into_iter().filter(|s| s.key.contains(k)).collect();
    let keys: std::collections::BTreeSet<&str> = m.iter().map(|s| s.key.as_str()).collect();
    match keys.len() {
        0 => Err(format!("no finding of the store has a key with {k}")),
        1 => Ok(m),
        _ => Err(format!(
            "{k} is in {} keys ({}); give more of the key",
            keys.len(),
            keys.into_iter().collect::<Vec<_>>().join(", ")
        )),
    }
}

/// The document of a view.
pub fn view(store: &Path, shown: &str, v: &str, f: &FindingsFilter) -> Result<FindingsDoc, String> {
    let mut all = collect(store, f.project.as_deref())?;
    if let Some(k) = &f.key {
        all = by_key(all, k)?;
    }
    let mut fs: Vec<StoredFinding> = all.into_iter().filter(|s| keep(s, f)).collect();
    if v == "list" {
        for s in &mut fs {
            s.sightings.clear();
        }
    }
    Ok(FindingsDoc {
        hydo_version: Some(hydo_image::hydo_version().into()),
        view: v.into(),
        store: shown.into(),
        filter: f.clone(),
        findings: fs,
        ..FindingsDoc::default()
    })
}

fn when_words(t: Option<&str>) -> String {
    match t {
        Some(t) => t.replace('T', " ").trim_end_matches('Z').to_string() + " UTC",
        None => "-".into(),
    }
}

fn point_words(p: Option<&FindingPoint>) -> String {
    match p {
        None => "-".into(),
        Some(p) => match &p.commit {
            Some(c) => format!(
                "{} (commit {})",
                when_words(p.time.as_deref()),
                c.chars().take(10).collect::<String>()
            ),
            None => when_words(p.time.as_deref()),
        },
    }
}

fn state_words(s: &StoredFinding) -> String {
    let mut w = match (s.state.as_str(), s.reappeared) {
        ("fixed", _) => "fixed".to_string(),
        (_, true) => "back again".to_string(),
        _ => "open".to_string(),
    };
    if s.accepted {
        w.push_str(", accepted");
    }
    w
}

fn event_words(e: &str) -> &str {
    match e {
        "new" => "first seen",
        "failing" => "seen failing again",
        "back" => "back again after a fix",
        "fixed" => "fixed: its schedule passed",
        "not-found" => "not found (still open)",
        o => o,
    }
}

/// The text of a view, for people.
pub fn text(d: &FindingsDoc) -> String {
    let mut s = String::new();
    let keys: std::collections::BTreeSet<&str> =
        d.findings.iter().map(|f| f.key.as_str()).collect();
    let noun = if keys.len() == 1 {
        "finding"
    } else {
        "findings"
    };
    let what = match d.filter.state.as_deref() {
        Some("all") => noun.to_string(),
        None if d.view != "list" => noun.to_string(),
        Some(st) => format!("{st} {noun}"),
        None => format!("open {noun}"),
    };
    if d.findings.is_empty() {
        s.push_str(&format!("The store {} has no {what}", d.store));
        if let Some(p) = &d.filter.project {
            s.push_str(&format!(" of {p}"));
        }
        s.push_str(".\n");
        return s;
    }
    s.push_str(&format!(
        "The store {}: {} {what}{}.\n",
        d.store,
        keys.len(),
        if d.findings.len() > keys.len() {
            format!(" on {} branch lines", d.findings.len())
        } else {
            String::new()
        }
    ));
    let mut last_head = String::new();
    for f in &d.findings {
        let head = format!("{} on {}", f.project, f.branch);
        if head != last_head {
            s.push_str(&format!("\n{head}\n"));
            last_head = head;
        }
        s.push_str(&format!("  {}  {}\n", f.key, f.title));
        s.push_str(&format!(
            "    {}; first seen {}, last seen {}; {} sighting{}\n",
            state_words(f),
            point_words(f.first_seen.as_ref()),
            point_words(f.last_seen.as_ref()),
            f.seen,
            if f.seen == 1 { "" } else { "s" }
        ));
        if f.state == "fixed" {
            s.push_str(&format!("    fixed {}\n", point_words(f.fixed_at.as_ref())));
        }
        if !f.classes.is_empty() {
            s.push_str(&format!("    class {}\n", f.classes.join(", ")));
        }
        s.push_str(&format!("    minimal schedule: {}\n", f.minimal));
        if d.view == "list" {
            continue;
        }
        if d.view == "show" {
            if let Some(v) = &f.verdict {
                s.push_str(&format!("    verdict: {v}\n"));
            }
            for o in f.schedules.iter().filter(|x| **x != f.minimal) {
                s.push_str(&format!("    also: {o}\n"));
            }
            if let Some(r) = &f.repro {
                s.push_str(&format!("    replay: {r}\n"));
            }
            let mut last = Vec::new();
            if let Some(h) = &f.hydo {
                last.push(format!("hydo {h}"));
            }
            if let Some(e) = f.epoch {
                last.push(format!("epoch {e}"));
            }
            if let Some(e) = &f.engine {
                last.push(format!("engine {e}"));
            }
            if let Some(i) = &f.image_sha256 {
                last.push(format!(
                    "image sha256 {}",
                    i.chars().take(16).collect::<String>()
                ));
            }
            if let Some(p) = &f.fp {
                last.push(format!("fp={p}"));
            }
            if !last.is_empty() {
                s.push_str(&format!("    last seen with {}\n", last.join(", ")));
            }
            continue;
        }
        for x in &f.sightings {
            let mut line = format!(
                "    {}  {:<12}  {}",
                when_words(x.time.as_deref()),
                x.by,
                event_words(&x.event)
            );
            if let Some(a) = &x.acceptance {
                line.push_str(&format!(", {a}"));
            }
            if let Some(c) = &x.commit {
                line.push_str(&format!(
                    "  commit {}",
                    c.chars().take(10).collect::<String>()
                ));
            }
            if let Some(p) = &x.fp {
                line.push_str(&format!("  fp={p}"));
            }
            if let Some(i) = &x.image_sha256 {
                line.push_str(&format!(
                    "  image {}",
                    i.chars().take(12).collect::<String>()
                ));
            }
            s.push_str(&line);
            s.push('\n');
        }
    }
    s
}

fn cmd_view(ctx: &Ctx, v: &str, argv: &[String]) -> i32 {
    if crate::help::asked(argv) {
        print!("{HELP}");
        return 0;
    }
    let mut args = argv.to_vec();
    let given = take(&mut args, "--store");
    let store = store_of(ctx, given.clone());
    let shown = given.unwrap_or_else(|| store.display().to_string());
    let json = args.iter().any(|a| a == "--json");
    args.retain(|a| a != "--json");
    let mut f = FindingsFilter {
        project: take(&mut args, "--project"),
        branch: take(&mut args, "--branch"),
        state: take(&mut args, "--state"),
        kind: take(&mut args, "--kind"),
        class: take(&mut args, "--class"),
        since: take(&mut args, "--since"),
        key: None,
    };
    if let Some(st) = &f.state {
        if !matches!(st.as_str(), "open" | "fixed" | "accepted" | "all") {
            die("--state is open, fixed, accepted or all");
        }
    }
    if v == "list" && f.state.is_none() {
        f.state = Some("open".into());
    }
    if let Some(bad) = args.iter().find(|a| a.starts_with("--")) {
        die(&format!(
            "hydo findings {v}: unknown option {bad} (hydo findings --help)"
        ));
    }
    match (v, args.len()) {
        ("list", 0) | ("history", 0) => {}
        ("show", 1) | ("history", 1) => f.key = Some(args[0].clone()),
        ("show", _) => die("usage: hydo findings show KEY [--store DIR] [--project NAME] [--json]"),
        _ => die(&format!(
            "usage: hydo findings {v} ... (hydo findings --help)"
        )),
    }
    if !store.is_dir() {
        if json {
            let d = FindingsDoc {
                hydo_version: Some(hydo_image::hydo_version().into()),
                view: v.into(),
                store: shown,
                filter: f,
                ..FindingsDoc::default()
            };
            println!("{}", serde_json::to_string_pretty(&d).unwrap_or_default());
        } else {
            println!(
                "The store {shown} does not exist yet: hydo findings record, or hydo run and hydo \
                 explore with --record-findings, write it."
            );
        }
        return 0;
    }
    match view(&store, &shown, v, &f) {
        Ok(d) if json => {
            println!("{}", serde_json::to_string_pretty(&d).unwrap_or_default());
            0
        }
        Ok(d) => {
            print!("{}", text(&d));
            0
        }
        Err(e) => die(&format!("hydo findings {v}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_quoted_only_when_it_needs_it() {
        assert_eq!(sh_word("a.img"), "a.img");
        assert_eq!(sh_word("disk=cut | run C=1"), "'disk=cut | run C=1'");
        assert_eq!(sh_word("it's"), "'it'\\''s'");
        assert_eq!(
            repro_line(Some("redis-arm64.cpio.gz"), "disk=none | run"),
            "hydo run redis-arm64.cpio.gz 'disk=none | run'"
        );
    }

    #[test]
    fn the_project_falls_back_to_the_image_name() {
        let nowhere = Path::new("/nonexistent-dir-of-hydo-tests");
        assert_eq!(project_name("/x/y/redis-arm64.cpio.gz", nowhere), "redis");
        assert_eq!(project_name("db-disk.img", nowhere), "db");
    }

    #[test]
    fn an_engine_keeps_no_paths() {
        let e = Engine {
            backend: Some("hvm".into()),
            args: Some(vec!["--hvm-kernel".into(), "/opt/x/Image".into()]),
            guest_kernel: Some("/opt/x/Image-explore-cg 1a2b3c4d".into()),
            ..Engine::default()
        };
        let s = scrub_engine(&e);
        assert_eq!(s.args, None);
        assert_eq!(s.guest_kernel.as_deref(), Some("Image-explore-cg 1a2b3c4d"));
        assert_eq!(s.backend.as_deref(), Some("hvm"));
    }
}
