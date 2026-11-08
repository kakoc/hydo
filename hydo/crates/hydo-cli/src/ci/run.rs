//! `hydo ci run` (docs/design/ci.md sections 6.1, 7.2, 8.4): one command, the same on a laptop and
//! in every CI system.
//!
//! 1. Settings: flags, then `hydo/ci.yaml`, then the CI system's variables (src/ci/env.rs), then
//!    git (the commit, the base: `--base`, GitLab's diff base, or the merge base with the target
//!    branch).
//! 2. The image: its input key (src/ci/inputkey.rs) names it in the image cache
//!    (`$HYDO_HOME/ci/images/<key>/`); a hit skips the `build:` command and `hydo build`.
//! 3. The exploration of this commit's image with the mode's budget (a fixed seed range, so that
//!    base and head explore the same schedules).
//! 4. The base: its findings from `--base-run` (its recorded `ci-run.json`), or by exploring the
//!    base commit's image (built in a `git worktree` of the base commit, or taken from the cache)
//!    with the same budget; when both images are the same bytes, the base's findings are this
//!    commit's.
//! 5. Every finding is decided by replay, never by absence: a finding of this commit replayed on
//!    the base's image (passes: `new`; fails the same way: `pre-existing`), a finding of the base
//!    replayed here (passes: `fixed`; fails the same way: `present`; otherwise: `changed`), a
//!    schedule that is refused or gives no result: `unverified` (or `new`, when the base refuses
//!    a schedule of this commit: it lacks what the schedule names). For a new finding, the two
//!    replays are compared with `hydo diff`.
//! 6. The saved regressions replayed on this commit's image.
//! 7. `ci-run.json`, `report.md`, `junit.xml`, and the exit status: 0 passed, 1 failed by the
//!    policy (`fail_on`), 2 not completed (every `hydo: ` stop in this command exits 2).

use super::{env, inputkey, key, report};
use crate::ctx::{abspath, die, take, Ctx, Platform, DIE_CODE};
use crate::runs;
use hydo_manifest::ci::{CiConfig, FailOn};
use hydo_results::{
    ci_state, CiArtifacts, CiBase, CiBudget, CiCoverage, CiFinding, CiHydo, CiImage,
    CiNondeterminism, CiOutcome, CiProject, CiProperties, CiRegressionCounts, CiReplay, CiRun,
    CiSummary, CiSystem, Exploration,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;

/// Every value of `--NAME VALUE` (repeatable flags).
fn take_all(args: &mut Vec<String>, flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(v) = take(args, flag) {
        out.push(v);
    }
    out
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    match args.iter().position(|a| a == flag) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

/// `git -C dir <args>`'s trimmed output, when it succeeds.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn sha_of(p: &Path) -> Option<String> {
    hydo_engine::image::sha256_file(p).ok()
}

/// The image `hydo build` wrote into `dir` (one file with this platform's image suffix).
fn image_in(ctx: &Ctx, dir: &Path) -> Option<PathBuf> {
    let suffix = match ctx.platform {
        Platform::Linux => "-disk.img",
        Platform::Mac => "-arm64.cpio.gz",
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(suffix))
        })
        .collect();
    found.sort();
    (found.len() == 1).then(|| found.remove(0))
}

/// An image a CI run uses.
struct Built {
    path: PathBuf,
    ci: CiImage,
}

/// The project's manifest and its `name`.
fn manifest_of(project: &Path, cfg: &CiConfig) -> PathBuf {
    project.join(cfg.manifest.as_deref().unwrap_or("hydo/hydo.yaml"))
}

fn project_name(manifest: &Path) -> String {
    std::fs::read_to_string(manifest)
        .ok()
        .and_then(|t| hydo_manifest::parse_text(&t, &manifest.display().to_string(), false).ok())
        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .unwrap_or_else(|| "project".into())
}

/// The `build:` command in the project's directory.
fn run_build(project: &Path, cmd: &str, label: &str) -> Result<(), String> {
    println!("hydo ci: {label}: {cmd}");
    let st = std::process::Command::new("sh")
        .args(["-c", cmd])
        .current_dir(project)
        .status()
        .map_err(|e| format!("{label}: cannot run the build command {cmd:?}: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!(
            "{label}: the build command {cmd:?} failed ({st}) in {}",
            project.display()
        ))
    }
}

/// The image of the project in `project`: from the image cache by its input key, or built (the
/// `build:` command, then `hydo build`) and put into the cache.
fn ensure_image(
    ctx: &Ctx,
    project: &Path,
    cfg: &CiConfig,
    sets: &[(String, String)],
    cache: &Path,
    label: &str,
) -> Result<Built, String> {
    let manifest = manifest_of(project, cfg);
    if !manifest.is_file() {
        return Err(format!("{label}: no {}", manifest.display()));
    }
    let arch = match ctx.platform {
        Platform::Linux => "x86_64",
        Platform::Mac => "arm64",
    };
    let build_cmd = cfg.build.as_deref();
    let mut ran_build = false;
    // without declared inputs the build command's outputs are hashed, so it runs first
    if let (Some(c), true) = (build_cmd, cfg.inputs.is_empty()) {
        run_build(project, c, label)?;
        ran_build = true;
    }
    let opts = hydo_manifest::LoadOptions {
        arch: arch.into(),
        sets: sets.to_vec(),
        onboard_dir: ctx.onboard(),
        ..Default::default()
    };
    let loaded = match hydo_manifest::load(&manifest, &opts) {
        Ok(l) => l,
        Err(_) if build_cmd.is_some() && !ran_build => {
            run_build(project, build_cmd.unwrap_or_default(), label)?;
            ran_build = true;
            hydo_manifest::load(&manifest, &opts)
                .map_err(|e| format!("{label}: {}: {e}", manifest.display()))?
        }
        Err(e) => return Err(format!("{label}: {}: {e}", manifest.display())),
    };
    let mut srcs = Vec::new();
    inputkey::src_paths(&loaded.manifest.data, &mut srcs);
    let hydo_id = format!(
        "{} {}",
        hydo_image::hydo_version(),
        hydo_image::hydo_commit().unwrap_or("-")
    );
    let (lines, ik) = inputkey::input_key(&inputkey::Inputs {
        hydo: hydo_id,
        arch,
        sets,
        manifest: &manifest,
        project,
        srcs: &srcs,
        inputs: &cfg.inputs,
        built_here: build_cmd.is_some(),
    });
    let images = cache.join("images");
    let dir = images.join(&ik);
    if let Some(img) = image_in(ctx, &dir) {
        println!("hydo ci: {label}: image {ik} from the image cache");
        return Ok(Built {
            ci: CiImage {
                input_key: Some(ik),
                sha256: sha_of(&img),
                reused: true,
                given: false,
                path: Some(img.display().to_string()),
            },
            path: img,
        });
    }
    if let (Some(c), false) = (build_cmd, ran_build) {
        run_build(project, c, label)?;
    }
    let tmp = images.join(format!(".{ik}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let mut bargs = vec![
        manifest.display().to_string(),
        "--out".to_string(),
        tmp.display().to_string(),
    ];
    for (k, v) in sets {
        bargs.push("--set".into());
        bargs.push(format!("{k}={v}"));
    }
    println!("hydo ci: {label}: hydo build {}", bargs.join(" "));
    let rc = crate::build::cmd_build(ctx, &bargs);
    if rc != 0 {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("{label}: hydo build failed (status {rc})"));
    }
    let _ = std::fs::write(tmp.join("input-key.txt"), lines.join("\n") + "\n");
    if std::fs::rename(&tmp, &dir).is_err() {
        // another job put the same image there first: the same bytes
        let _ = std::fs::remove_dir_all(&tmp);
    }
    let img = image_in(ctx, &dir)
        .ok_or_else(|| format!("{label}: hydo build wrote no image into {}", dir.display()))?;
    Ok(Built {
        ci: CiImage {
            input_key: Some(ik),
            sha256: sha_of(&img),
            reused: false,
            given: false,
            path: Some(img.display().to_string()),
        },
        path: img,
    })
}

/// An image given ready-made (`--image`, `--base-image`).
fn given(p: &str) -> Built {
    let path = abspath(Path::new(p));
    if !path.is_file() {
        die(&format!("no such image: {p}"));
    }
    Built {
        ci: CiImage {
            input_key: None,
            sha256: sha_of(&path),
            reused: false,
            given: true,
            path: Some(path.display().to_string()),
        },
        path,
    }
}

/// What one CI run explores.
struct Plan {
    seeds: String,
    strategy: String,
    options: Vec<String>,
    workers: Option<u64>,
}

/// The exploration of `image` into `dir` (in this process, as `hydo explore`).
fn explore(
    ctx: &Ctx,
    image: &Path,
    p: &Plan,
    dir: &Path,
    label: &str,
) -> Result<Exploration, String> {
    let _ = std::fs::remove_dir_all(dir);
    let mut a = vec![
        "--image".to_string(),
        image.display().to_string(),
        "--seeds".into(),
        p.seeds.clone(),
        "--strategy".into(),
        p.strategy.clone(),
        "--out".into(),
        dir.display().to_string(),
    ];
    if let Some(w) = p.workers {
        a.push("--workers".into());
        a.push(w.to_string());
    }
    a.extend(p.options.iter().cloned());
    println!("hydo ci: {label}: hydo explore {}", a.join(" "));
    let rc = crate::delegated::cmd_explore(ctx, &a);
    match hydo_results::read_dir(dir) {
        Ok(hydo_results::Results::Exploration { doc: Some(d), .. }) => Ok(*d),
        _ => Err(format!(
            "{label}: the exploration wrote no {} (status {rc})",
            dir.join(hydo_results::EXPLORATION_DOC).display()
        )),
    }
}

/// Every finding key a run gives: its verdict's, and each new error kind's.
fn run_keys(run: &hydo_results::Run) -> Vec<String> {
    let mut k: Vec<String> = key::of_verdict(&run.verdict.text, &run.findings)
        .map(|x| vec![x.2])
        .unwrap_or_default();
    for f in run.findings_of(hydo_results::kind::ERROR_LOG) {
        if f.status == hydo_results::status::NEW {
            k.push(key::key(hydo_results::kind::ERROR_LOG, &f.id));
        }
    }
    k
}

/// The fault class of each route (minimal schedule) of an exploration's findings.
type RouteClasses = BTreeMap<String, String>;

/// One finding (by key) of an exploration, the failures that were not deterministic, and the
/// fault class of each route.
fn findings_of(doc: &Exploration) -> (Vec<CiFinding>, Vec<CiNondeterminism>, RouteClasses) {
    let mut by: BTreeMap<String, CiFinding> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut nondet = Vec::new();
    let mut classes = RouteClasses::new();
    let mut add = |by: &mut BTreeMap<String, CiFinding>,
                   order: &mut Vec<String>,
                   kind: String,
                   title: String,
                   k: String,
                   minimal: &str,
                   fp: Option<String>,
                   class: &str,
                   repro: Option<String>| {
        let f = by.entry(k.clone()).or_insert_with(|| {
            order.push(k.clone());
            CiFinding {
                key: k,
                kind,
                title,
                minimal: minimal.to_string(),
                fp,
                repro,
                ..CiFinding::default()
            }
        });
        if !f.schedules.iter().any(|s| s == minimal) {
            f.schedules.push(minimal.to_string());
            classes.insert(minimal.to_string(), class.to_string());
        }
        if !class.is_empty() && !f.classes.iter().any(|c| c == class) {
            f.classes.push(class.to_string());
        }
    };
    for f in &doc.failures {
        if !f.reproduced {
            nondet.push(CiNondeterminism {
                schedule: f.run.schedule.clone(),
                fp: f.run.fingerprints.fp.clone(),
                rerun_fps: f
                    .reproductions
                    .iter()
                    .filter_map(|r| r.fp.clone())
                    .collect(),
            });
        }
    }
    for s in &doc.shrinks {
        let fp: Vec<String> = s
            .confirmations
            .iter()
            .filter_map(|c| c.fp.clone())
            .collect();
        if !s.identical && fp.len() > 1 {
            nondet.push(CiNondeterminism {
                schedule: s.minimal.clone(),
                fp: fp[0].clone(),
                rerun_fps: fp[1..].to_vec(),
            });
            continue;
        }
        let verdict = s
            .confirmations
            .first()
            .and_then(|c| c.verdict.clone())
            .or_else(|| s.min_verdict.clone())
            .unwrap_or_else(|| s.verdict.clone());
        let fails: Vec<hydo_results::Finding> = doc
            .failures
            .iter()
            .find(|f| f.signature == s.signature)
            .map(|f| f.run.findings.clone())
            .unwrap_or_default();
        if let Some((kind, title, k)) = key::of_verdict(&verdict, &fails) {
            add(
                &mut by,
                &mut order,
                kind,
                title,
                k,
                &s.minimal,
                fp.first().cloned(),
                &s.class,
                s.repro_command.clone(),
            );
        }
    }
    // a failure whose signature was not shrunk keeps its own schedule
    for f in doc.failures.iter().filter(|f| f.reproduced) {
        if let Some((kind, title, k)) = key::of_verdict(&f.run.verdict.text, &f.run.findings) {
            if !by.contains_key(&k) {
                add(
                    &mut by,
                    &mut order,
                    kind,
                    title,
                    k,
                    &f.run.schedule,
                    Some(f.run.fingerprints.fp.clone()),
                    f.run.class.as_deref().unwrap_or(""),
                    None,
                );
            }
        }
    }
    if let Some(ek) = &doc.error_kinds {
        for k in ek.kinds.iter().filter(|k| k.new) {
            let minimal = k
                .shrink
                .as_ref()
                .map(|s| s.minimal.clone())
                .or_else(|| k.first_new.as_ref().map(|f| f.schedule.clone()))
                .unwrap_or_else(|| k.first_schedule.clone());
            add(
                &mut by,
                &mut order,
                hydo_results::kind::ERROR_LOG.into(),
                format!("[{}] {}", k.service, k.template),
                key::key(hydo_results::kind::ERROR_LOG, &k.signature),
                &minimal,
                None,
                "",
                None,
            );
        }
    }
    (
        order.into_iter().filter_map(|k| by.remove(&k)).collect(),
        nondet,
        classes,
    )
}

/// Replays of minimal schedules, numbered in one directory.
struct Replayer<'a> {
    ctx: &'a Ctx,
    dir: PathBuf,
    out: &'a Path,
    n: u64,
}

impl Replayer<'_> {
    /// One schedule from a fresh boot on `image`: the replay record and every finding key it gave.
    fn run(&mut self, b: &Built, on: &str, sched: &str) -> (CiReplay, Vec<String>, PathBuf) {
        self.n += 1;
        let dir = self.dir.join(format!("{:03}-{on}", self.n));
        let _ = std::fs::remove_dir_all(&dir);
        let rel = dir
            .strip_prefix(self.out)
            .unwrap_or(&dir)
            .display()
            .to_string();
        println!("hydo ci: replay on the {on}'s image: {sched}");
        let mut r = CiReplay {
            on: on.into(),
            schedule: sched.into(),
            dir: Some(rel),
            ..CiReplay::default()
        };
        let keys = match runs::try_run_to_dir(
            self.ctx,
            &b.path,
            b.ci.sha256.clone(),
            sched,
            &dir,
            &runs::RunOpts::default(),
        ) {
            Err(w) => {
                println!("hydo ci:   refused: {w}");
                r.refused = Some(w);
                Vec::new()
            }
            Ok(None) => {
                println!("hydo ci:   no result (engine.log in {})", dir.display());
                Vec::new()
            }
            Ok(Some(run)) => {
                println!("hydo ci:   {}", run.verdict.text);
                r.verdict = Some(run.verdict.text.clone());
                r.fp = Some(run.fingerprints.fp.clone());
                let k = run_keys(&run);
                r.key = k.first().cloned();
                k
            }
        };
        (r, keys, dir)
    }
}

/// `hydo diff`'s first judged difference of two run directories (A the base, B this change), in
/// one sentence; `diff.json` and `diff.txt` written into `into`.
fn diff_line(base: &Path, head: &Path, into: &Path) -> Option<String> {
    let a = crate::diff::load(base).ok()?;
    let b = crate::diff::load(head).ok()?;
    let d = crate::diff::diff(&a, &b);
    let _ = std::fs::create_dir_all(into);
    let _ = crate::diff::write(into, &d);
    let o = d.first_outcome.as_ref().or(d.first.as_ref())?;
    let what = o.details.first().cloned().or_else(|| {
        o.lines.first().map(|l| {
            format!(
                "{} {}",
                if l.a.is_some() {
                    "only on the base:"
                } else {
                    "only in this change:"
                },
                l.text
            )
        })
    })?;
    let after = o
        .lines
        .iter()
        .find_map(|l| l.after.clone())
        .map(|a| format!(", after `{}`", a.replace('`', "'")))
        .unwrap_or_default();
    Some(format!(
        "the first difference in what the checks judge ({} level{after}) is `{}` (A is the \
         base, B this change)",
        o.level,
        what.replace('`', "'")
    ))
}

/// The summary's state counts and the outcome by the policy (`fail_on`).
pub fn decide(r: &mut CiRun) {
    let mut states: BTreeMap<String, u64> = BTreeMap::new();
    for f in &r.findings {
        *states.entry(f.state.clone()).or_default() += 1;
    }
    r.summary.states = states;
    r.summary.findings = r
        .findings
        .iter()
        .filter(|f| f.state != ci_state::FIXED)
        .count() as u64;
    r.summary.regressions = CiRegressionCounts {
        replayed: r.regressions.len() as u64,
        as_expected: r.regressions.iter().filter(|g| g.as_expected).count() as u64,
    };
    let on = |s: &str| r.outcome.fail_on.iter().any(|f| f == s);
    let mut by = Vec::new();
    for f in &r.findings {
        if on(&f.state) {
            by.push(format!("{} finding {}", f.state, f.title));
        }
    }
    if on("regression") {
        for g in r.regressions.iter().filter(|g| !g.as_expected) {
            by.push(format!("regression {} not as expected", g.name));
        }
    }
    if r.outcome.exit < 2 {
        r.outcome.exit = if by.is_empty() { 0 } else { 1 };
    }
    r.outcome.failed_by = by;
}

/// What the first runs drew, and the property table's reach.
fn coverage(doc: &Exploration, dir: &Path) -> (CiCoverage, u64) {
    let mut c = CiCoverage::default();
    let mut runs = 0;
    if let Ok(hydo_results::Results::Exploration {
        events: Some(ev), ..
    }) = hydo_results::read_dir(dir)
    {
        for r in ev.runs() {
            runs += 1;
            if r.role.as_deref() == Some(hydo_results::role::FIRST) {
                key::count_faults(&r.schedule, &mut c.faults, &mut c.services);
            }
        }
    }
    if let Some(t) = &doc.properties {
        let declared = t.properties.iter().filter(|p| p.declared).count() as u64;
        let not: Vec<String> = t
            .properties
            .iter()
            .filter(|p| p.status == hydo_results::status::NOT_EXERCISED)
            .map(|p| p.id.clone())
            .collect();
        c.properties = Some(CiProperties {
            declared,
            reached: t
                .properties
                .iter()
                .filter(|p| p.passes + p.fails > 0)
                .count() as u64,
            not_exercised: not,
        });
    }
    (c, runs)
}

/// The base commit's checkout (a detached `git worktree`), removed when dropped.
struct BaseCheckout {
    repo: PathBuf,
    path: PathBuf,
}

impl Drop for BaseCheckout {
    fn drop(&mut self) {
        let _ = git(
            &self.repo,
            &[
                "worktree",
                "remove",
                "--force",
                &self.path.display().to_string(),
            ],
        );
        let _ = git(&self.repo, &["worktree", "prune"]);
    }
}

fn checkout_base(
    project: &Path,
    commit: &str,
    out: &Path,
) -> Result<(BaseCheckout, PathBuf), String> {
    let top = git(project, &["rev-parse", "--show-toplevel"])
        .ok_or_else(|| format!("{} is not in a git repository", project.display()))?;
    let top = PathBuf::from(top);
    let rel = abspath(project)
        .strip_prefix(abspath(&top))
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let path = out.join("base-src");
    if path.exists() {
        let _ = git(
            &top,
            &["worktree", "remove", "--force", &path.display().to_string()],
        );
        let _ = std::fs::remove_dir_all(&path);
        let _ = git(&top, &["worktree", "prune"]);
    }
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(&top)
        .args(["worktree", "add", "--detach", "--force"])
        .arg(&path)
        .arg(commit)
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("git worktree add: {e}"))?;
    if !st.success() {
        return Err(format!(
            "cannot check out the base commit {commit} (git worktree add failed; a shallow \
             clone needs the base commit fetched: fetch-depth 0, or git fetch origin <base>)"
        ));
    }
    let proj = path.join(rel);
    Ok((BaseCheckout { repo: top, path }, proj))
}

pub fn cmd_run(ctx: &Ctx, argv: &[String]) -> i32 {
    DIE_CODE.store(2, Ordering::Relaxed);
    hydo_explore::DIE_CODE.store(2, Ordering::Relaxed);
    let t0 = Instant::now();
    let mut args = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{}", super::HELP);
        return 0;
    }
    let project = abspath(Path::new(
        &take(&mut args, "--project").unwrap_or_else(|| ".".into()),
    ));
    let out = abspath(Path::new(
        &take(&mut args, "--out").unwrap_or_else(|| "hydo-ci".into()),
    ));
    let config = take(&mut args, "--config");
    let mode_flag = take(&mut args, "--mode");
    let commit_flag = take(&mut args, "--commit");
    let base_flag = take(&mut args, "--base");
    let base_branch_flag = take(&mut args, "--base-branch");
    let change_flag = take(&mut args, "--change");
    let base_run = take(&mut args, "--base-run");
    let image_flag = take(&mut args, "--image");
    let base_image_flag = take(&mut args, "--base-image");
    let backend_flag = take(&mut args, "--backend");
    let seeds_flag = take(&mut args, "--seeds");
    let strategy_flag = take(&mut args, "--strategy");
    let options_flag = take_all(&mut args, "--option");
    let workers_flag = take(&mut args, "--workers");
    let fail_on_flag = take(&mut args, "--fail-on");
    let cache_flag = take(&mut args, "--cache");
    let set_flags = take_all(&mut args, "--set");
    let no_base = take_flag(&mut args, "--no-base");
    if !args.is_empty() {
        die(&format!(
            "ci run: unknown arguments {:?} (hydo ci --help)",
            args.join(" ")
        ));
    }
    if !project.is_dir() {
        die(&format!(
            "ci run: no project directory {}",
            project.display()
        ));
    }
    let cienv = env::from_process();

    // the CI settings
    let cfg_path = config
        .as_ref()
        .map(|c| abspath(Path::new(c)))
        .unwrap_or_else(|| project.join(hydo_manifest::ci::DEFAULT_PATH));
    let cfg = if cfg_path.is_file() {
        let t = std::fs::read_to_string(&cfg_path)
            .unwrap_or_else(|e| die(&format!("{}: {e}", cfg_path.display())));
        hydo_manifest::ci::parse(&t, &cfg_path.display().to_string())
            .unwrap_or_else(|e| die(&e.message))
    } else if config.is_some() {
        die(&format!("no such file: {}", cfg_path.display()))
    } else {
        CiConfig::default()
    };
    if let Err(e) = std::env::set_current_dir(&project) {
        die(&format!("{}: {e}", project.display()));
    }

    // the engine: --backend, HYDO_BACKEND, ci.yaml's engine, else the platform's CI default
    let backend = backend_flag
        .or_else(|| ctx.config.kvm.backend.clone())
        .or_else(|| cfg.engine.clone())
        .unwrap_or_else(|| match ctx.platform {
            Platform::Linux => "kvm".into(),
            Platform::Mac => "hvm".into(),
        });
    let kvm = match (ctx.platform, backend.as_str()) {
        (Platform::Linux, "kvm") => true,
        (Platform::Linux, "hvx") => false,
        (Platform::Mac, "hvm") => false,
        (p, b) => die(&format!(
            "the engine {b} does not run on {} (kvm or hvx on Linux, hvm on a Mac)",
            if p == Platform::Mac { "a Mac" } else { "Linux" }
        )),
    };
    let ctx = Ctx {
        platform: ctx.platform,
        root: ctx.root.clone(),
        config: ctx.config.clone(),
        kvm,
        // `hydo ci run` has chosen the engine above (flag, HYDO_BACKEND, ci.yaml or the
        // platform's CI default), so a replay never switches to a recorded one.
        backend_chosen: true,
        cargo_installed: ctx.cargo_installed,
        checkout: ctx.checkout.clone(),
    };

    // the mode and the budget
    let trigger = cienv.trigger.clone().unwrap_or_else(|| {
        if base_flag.is_some() {
            "pull_request".into()
        } else {
            "manual".into()
        }
    });
    let mode = mode_flag.unwrap_or_else(|| {
        match trigger.as_str() {
            "pull_request" => "pr",
            "schedule" => "nightly",
            _ => "push",
        }
        .into()
    });
    if !matches!(mode.as_str(), "pr" | "push" | "nightly") {
        die(&format!("--mode {mode}: expected pr, push or nightly"));
    }
    let b = cfg.budget(&mode);
    let plan = Plan {
        seeds: seeds_flag
            .or(b.seeds.clone())
            .unwrap_or_else(|| "1-50".into()),
        strategy: strategy_flag
            .or(b.strategy.clone())
            .unwrap_or_else(|| "process".into()),
        options: if options_flag.is_empty() {
            b.options.clone()
        } else {
            options_flag
        },
        workers: workers_flag
            .map(|w| w.parse().unwrap_or_else(|_| die(&format!("--workers {w}"))))
            .or(b.workers),
    };
    if hydo_manifest::ci::parse_seeds(&plan.seeds).is_none() {
        die(&format!(
            "--seeds {}: expected a range like 1-200",
            plan.seeds
        ));
    }
    let fail_on: Vec<String> = match fail_on_flag {
        Some(f) => f
            .split(',')
            .map(|w| {
                FailOn::parse(w.trim())
                    .unwrap_or_else(|| die(&format!("--fail-on: {w:?} is not a state")))
                    .word()
                    .to_string()
            })
            .collect(),
        None => cfg.fail_on().iter().map(|f| f.word().to_string()).collect(),
    };
    let mut sets: BTreeMap<String, String> = cfg.set.clone();
    for s in &set_flags {
        let (k, v) = s
            .split_once('=')
            .unwrap_or_else(|| die(&format!("--set {s}: expected KEY=VALUE")));
        sets.insert(k.into(), v.into());
    }
    let sets: Vec<(String, String)> = sets.into_iter().collect();
    let cache = cache_flag
        .map(|c| abspath(Path::new(&c)))
        .unwrap_or_else(|| ctx.config.cli.cache.home.join("ci"));

    // the commits
    let commit = commit_flag
        .or(cienv.commit.clone())
        .or_else(|| git(&project, &["rev-parse", "HEAD"]));
    let branch = cienv
        .branch
        .clone()
        .or_else(|| git(&project, &["rev-parse", "--abbrev-ref", "HEAD"]))
        .filter(|b| b != "HEAD");
    let base_branch = base_branch_flag.or(cienv.base_branch.clone());
    let resolve = |r: &str| {
        git(
            &project,
            &["rev-parse", "--verify", &format!("{r}^{{commit}}")],
        )
    };
    let base_commit: Option<String> = if no_base {
        None
    } else if let Some(r) = &base_flag {
        Some(resolve(r).unwrap_or_else(|| die(&format!("--base {r}: not a commit here"))))
    } else if mode == "pr" {
        cienv.base_commit.clone().or_else(|| {
            let b = base_branch.as_deref()?;
            let tip = resolve(&format!("origin/{b}")).or_else(|| resolve(b))?;
            git(&project, &["merge-base", "HEAD", &tip])
        })
    } else {
        None
    };
    let _ = std::fs::create_dir_all(&out);
    println!(
        "hydo ci: project {} at {}, mode {mode}, engine {}, base {}",
        project.display(),
        commit.as_deref().unwrap_or("(no commit)"),
        ctx.backend_name(),
        base_commit.as_deref().unwrap_or("none")
    );

    // this commit's image and its exploration
    let manifest = manifest_of(&project, &cfg);
    let head = match &image_flag {
        Some(p) => given(p),
        None => ensure_image(&ctx, &project, &cfg, &sets, &cache, "this commit")
            .unwrap_or_else(|e| die(&e)),
    };
    let explore_dir = out.join("explore");
    let doc =
        explore(&ctx, &head.path, &plan, &explore_dir, "this commit").unwrap_or_else(|e| die(&e));
    let (mut found, nondet, route_class) = findings_of(&doc);
    let (cov, nruns) = coverage(&doc, &explore_dir);

    // the base: its image, and its findings
    let mut base = CiBase {
        branch: base_branch.clone(),
        commit: base_commit.clone(),
        findings_from: "none".into(),
        ..CiBase::default()
    };
    let mut _checkout = None;
    let base_img: Option<Built> = if no_base {
        base.missing = Some("--no-base".into());
        None
    } else if let Some(p) = &base_image_flag {
        Some(given(p))
    } else if let Some(c) = &base_commit {
        match checkout_base(&project, c, &out).and_then(|(co, proj)| {
            let r = ensure_image(&ctx, &proj, &cfg, &sets, &cache, "the base");
            _checkout = Some(co);
            r
        }) {
            Ok(b) => Some(b),
            Err(e) => {
                println!("hydo ci: no base image: {e}");
                base.missing = Some(e);
                None
            }
        }
    } else {
        base.missing = Some(if mode == "pr" {
            "no base commit (give --base, or run in a pull request)".into()
        } else {
            format!("a {mode} run has no base unless --base is given")
        });
        None
    };
    drop(_checkout.take());
    let mut base_findings: Vec<CiFinding> = Vec::new();
    let mut base_explore = None;
    if let Some(f) = &base_run {
        let r = super::read_ci_run(Path::new(f)).unwrap_or_else(|e| die(&e));
        base.ci_run = Some(r.id.clone());
        if base.commit.is_none() {
            base.commit = r.commit.clone();
        }
        base_findings = r
            .findings
            .into_iter()
            .filter(|f| f.state != ci_state::FIXED)
            .collect();
        base.findings_from = "base-run".into();
        base.missing = None;
    } else if let Some(bi) = &base_img {
        base.missing = None;
        base.findings_from = "explored".into();
        if bi.ci.sha256.is_some() && bi.ci.sha256 == head.ci.sha256 {
            println!("hydo ci: the base's image is this commit's (the same bytes): its findings are the same");
            base_findings = found.clone();
        } else {
            let d = out.join("base-explore");
            let bdoc = explore(&ctx, &bi.path, &plan, &d, "the base").unwrap_or_else(|e| die(&e));
            base_findings = findings_of(&bdoc).0;
            base_explore = Some("base-explore/exploration.json".to_string());
        }
    }
    base.image = base_img.as_ref().map(|b| b.ci.clone());

    // every finding decided by replay
    let mut rp = Replayer {
        ctx: &ctx,
        dir: out.join("replays"),
        out: &out,
        n: 0,
    };
    let base_by_key: BTreeMap<String, CiFinding> = base_findings
        .iter()
        .map(|f| (f.key.clone(), f.clone()))
        .collect();
    let known = base.findings_from != "none";
    for f in &mut found {
        let on_base = base_by_key.get(&f.key);
        // the routes (minimal schedules) to replay on the base: all of a key the base does not
        // have; of a key it has, those it did not find (two causes with one message are one
        // key, and replay keeps each route honest: docs/design/ci.md section 4.2)
        let mut routes: Vec<String> = f
            .schedules
            .iter()
            .filter(|s| on_base.is_none_or(|b| !b.schedules.contains(s)))
            .cloned()
            .collect();
        // a route of a fault class the base's finding does not have first: it is the one this
        // change is likely to explain (a stable sort keeps the exploration's order otherwise)
        routes.sort_by_key(|s| {
            let c = route_class.get(s).map(String::as_str).unwrap_or("");
            on_base.is_some_and(|b| b.classes.iter().any(|x| x == c))
        });
        let Some(bi) = base_img.as_ref().filter(|_| !routes.is_empty()) else {
            let (state, proof) = match (on_base, known) {
                (Some(_), _) => (ci_state::PRESENT, format!("base-{}", base.findings_from)),
                (None, true) => (ci_state::NEW, "base-run".to_string()),
                (None, false) => (ci_state::NEW, "no-base".to_string()),
            };
            f.state = state.into();
            f.proof = Some(proof);
            continue;
        };
        let (mut fails, mut refused, mut no_result) = (false, 0, 0);
        let mut passed: Option<(String, PathBuf, &str)> = None;
        let mut other: Option<(String, PathBuf)> = None;
        for s in routes.iter().take(3) {
            let (r, keys, bdir) = rp.run(bi, "base", s);
            if r.refused.is_some() {
                refused += 1;
            } else if r.verdict.is_none() {
                no_result += 1;
            } else if keys.contains(&f.key) {
                fails = true;
            } else if keys.is_empty() {
                passed = Some((s.clone(), bdir, "base-replay-ok"));
            } else if other.is_none() {
                other = Some((s.clone(), bdir));
            }
            f.replays.push(r);
            if passed.is_some() {
                break;
            }
        }
        let tried = routes.len().min(3);
        // a key the base has: its exploration found it (a replay of this route there may also
        // have failed the same way)
        let present_proof = if fails {
            "base-replay-fails".to_string()
        } else {
            format!("base-{}", base.findings_from)
        };
        let (state, proof) = if let Some((s, bdir, proof)) = passed {
            // a route the base does not fail on: this change introduced it
            f.minimal = s.clone();
            let (h, _, hdir) = rp.run(&head, "head", &s);
            f.replays.push(h);
            let into = rp.dir.join(format!("diff-{:03}", rp.n));
            f.diff = diff_line(&bdir, &hdir, &into);
            (ci_state::NEW, proof)
        } else if let (Some((s, bdir)), None) = (other.clone(), on_base) {
            // the base fails this schedule too, with another finding: the change changed how
            // it fails, it did not make it fail
            f.minimal = s.clone();
            let (h, _, hdir) = rp.run(&head, "head", &s);
            f.replays.push(h);
            let into = rp.dir.join(format!("diff-{:03}", rp.n));
            f.diff = diff_line(&bdir, &hdir, &into);
            (ci_state::CHANGED, "base-replay-other")
        } else if on_base.is_some() {
            (ci_state::PRESENT, present_proof.as_str())
        } else if fails {
            (ci_state::PRE_EXISTING, "base-replay-fails")
        } else if refused == tried {
            // the base lacks what the schedule names (a new service, a new fault point)
            (ci_state::NEW, "base-refused")
        } else {
            let _ = no_result;
            (ci_state::UNVERIFIED, "base-replay-no-result")
        };
        f.state = state.into();
        f.proof = Some(proof.into());
    }
    let head_keys: BTreeSet<String> = found.iter().map(|f| f.key.clone()).collect();
    let mut gone = Vec::new();
    for b in base_findings.iter().filter(|b| !head_keys.contains(&b.key)) {
        let mut g = b.clone();
        g.replays.clear();
        g.diff = None;
        let mut scheds = g.schedules.clone();
        if scheds.is_empty() {
            scheds.push(g.minimal.clone());
        }
        let (mut same, mut other, mut ok, mut not_run) = (false, false, 0, 0);
        for s in &scheds {
            let (r, keys, _) = rp.run(&head, "head", s);
            if r.verdict.is_none() {
                not_run += 1;
            } else if keys.contains(&g.key) {
                same = true;
            } else if keys.is_empty() {
                ok += 1;
            } else {
                other = true;
            }
            g.replays.push(r);
        }
        let paired = found.iter().any(|h| {
            h.state == ci_state::CHANGED
                && g.replays.iter().any(|r| {
                    r.key.as_deref() == Some(h.key.as_str()) && h.schedules.contains(&r.schedule)
                })
        });
        if other && paired {
            // the same pair of verdicts is already reported from this change's side
            continue;
        }
        let (state, proof) = if same {
            (ci_state::PRESENT, "head-replay-fails")
        } else if other {
            (ci_state::CHANGED, "head-replay-other")
        } else if not_run == 0 && ok > 0 {
            (ci_state::FIXED, "head-replay-ok")
        } else {
            (ci_state::UNVERIFIED, "head-not-replayed")
        };
        g.state = state.into();
        g.proof = Some(proof.into());
        gone.push(g);
    }
    // a new finding failing on a schedule a fixed one named: probably one finding, renamed
    for f in found.iter_mut().filter(|f| f.state == ci_state::NEW) {
        if let Some(g) = gone.iter().find(|g| {
            g.state == ci_state::FIXED && g.schedules.iter().any(|s| f.schedules.contains(s))
        }) {
            f.renamed_from = Some(g.title.clone());
        }
    }
    found.extend(gone);

    // the saved regressions
    let regressions = runs::replay_regressions(&ctx, &head.path, &out.join("regressions"));

    let mut r = CiRun {
        id: format!(
            "{}:{}",
            cienv.system,
            cienv
                .run_id
                .clone()
                .unwrap_or_else(|| crate::ctx::strftime("%Y%m%dT%H%M%S"))
        ),
        ci: CiSystem {
            name: cienv.system.clone(),
            run_id: cienv.run_id.clone(),
            run_url: cienv.run_url.clone(),
        },
        project: CiProject {
            name: project_name(&manifest),
            dir: git(&project, &["rev-parse", "--show-prefix"])
                .map(|p| p.trim_end_matches('/').to_string())
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| ".".into()),
            manifest: manifest
                .strip_prefix(&project)
                .unwrap_or(&manifest)
                .display()
                .to_string(),
            manifest_sha256: sha_of(&manifest),
        },
        trigger,
        commit,
        branch,
        change: change_flag.or(cienv.change.clone()),
        base: if no_base { None } else { Some(base) },
        hydo: CiHydo {
            version: hydo_image::hydo_version().to_string(),
            commit: hydo_image::hydo_commit().map(str::to_string),
            epoch: hydo_image::EPOCH,
        },
        engine: doc.engine.clone(),
        image: head.ci.clone(),
        budget: CiBudget {
            mode,
            seeds: plan.seeds.clone(),
            strategy: plan.strategy.clone(),
            options: plan.options.clone(),
        },
        summary: CiSummary {
            runs: nruns,
            failures: doc.summary.failures,
            vacuous_passes: doc.summary.vacuous_passes.unwrap_or(0),
            replays: rp.n,
            ..CiSummary::default()
        },
        coverage: cov,
        findings: found,
        regressions,
        nondeterminism: nondet,
        outcome: CiOutcome {
            exit: 0,
            fail_on,
            failed_by: Vec::new(),
        },
        artifacts: CiArtifacts {
            exploration: Some("explore/exploration.json".into()),
            base_exploration: base_explore,
            report: "report.md".into(),
            junit: "junit.xml".into(),
        },
        ..CiRun::default()
    };
    r.summary.wall_s = (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    decide(&mut r);
    let md = report::markdown(&r, &sets);
    let w = |n: &str, t: &str| {
        std::fs::write(out.join(n), t).unwrap_or_else(|e| die(&format!("{n}: {e}")))
    };
    w("report.md", &md);
    w("junit.xml", &report::junit(&r));
    hydo_results::write_document(&out.join("ci-run.json"), &r)
        .unwrap_or_else(|e| die(&format!("ci-run.json: {e}")));
    println!("\n{md}");
    println!(
        "hydo ci: wrote {}, report.md and junit.xml (exit status {})",
        out.join("ci-run.json").display(),
        r.outcome.exit
    );
    r.outcome.exit as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(name: &str) -> Exploration {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schema/examples/results/v1")
            .join(name)
            .join("exploration.json");
        match hydo_results::reader::document(&std::fs::read_to_string(p).unwrap()).unwrap() {
            hydo_results::reader::Record::Exploration(d) => *d,
            _ => panic!("not an exploration"),
        }
    }

    /// Two fault classes that end in the same check's verdict are one finding with two routes
    /// (docs/design/ci.md section 4.2): the key comes from the property id, the classes and the
    /// minimal schedules are its attributes.
    #[test]
    fn an_explorations_failures_group_by_finding_key() {
        let (f, nondet, classes) = findings_of(&example("mac-redis-exploration"));
        assert!(nondet.is_empty());
        assert_eq!(f.len(), 1, "{f:#?}");
        let x = &f[0];
        assert_eq!(
            x.key,
            key::key("invariant", "VIOLATION [redis.acked_writes_survive]")
        );
        assert_eq!(
            x.title,
            "VIOLATION [redis.acked_writes_survive] key k:# lost"
        );
        assert_eq!(x.classes, ["lying-disk", "unsafe-knob"]);
        assert_eq!(
            x.schedules,
            [
                "disk=lying:3 dseed=eac0b527c5ac67d9 | run C=20",
                "disk=cut dseed=1f16055b6d41c6d9 | run C=9 R=no"
            ]
        );
        assert_eq!(x.minimal, x.schedules[0]);
        assert_eq!(classes[&x.schedules[1]], "unsafe-knob");
    }

    /// The policy decides the exit status: new findings and failed regressions fail by default,
    /// findings already on the base do not.
    #[test]
    fn the_outcome_follows_fail_on() {
        let f = |state: &str| CiFinding {
            key: format!("f1:invariant:{state}"),
            title: state.into(),
            state: state.into(),
            ..CiFinding::default()
        };
        let mut r = CiRun {
            findings: vec![f("present"), f("pre-existing"), f("fixed"), f("changed")],
            outcome: CiOutcome {
                fail_on: vec!["new".into(), "reappeared".into(), "regression".into()],
                ..CiOutcome::default()
            },
            ..CiRun::default()
        };
        decide(&mut r);
        assert_eq!((r.outcome.exit, r.summary.findings), (0, 3));
        assert_eq!(r.summary.states["fixed"], 1);
        r.regressions.push(hydo_results::CiRegression {
            name: "lost-write".into(),
            as_expected: false,
            ..Default::default()
        });
        decide(&mut r);
        assert_eq!(r.outcome.exit, 1);
        assert_eq!(
            r.outcome.failed_by,
            ["regression lost-write not as expected"]
        );
        r.regressions.clear();
        r.findings.push(f("new"));
        decide(&mut r);
        assert_eq!(r.outcome.failed_by, ["new finding new"]);
        r.outcome.fail_on.push("present".into());
        decide(&mut r);
        assert_eq!(r.outcome.failed_by.len(), 2);
    }
}
