//! `hydo versions` (Milestone 221): in which versions of the system a finding is present.
//!
//! A finding (a run directory, an exploration's findings, or a minimal schedule) is replayed on
//! a list of the system's versions, oldest first. A version is one of:
//!
//! - a manifest parameter's value: `hydo build MANIFEST --set KEY=<version>` (`--param`);
//! - a git revision of the project's repository: a detached checkout of it, its `--build`
//!   command, then `hydo build` of its manifest (`--git`);
//! - a ready image (`--images`).
//!
//! Each version's image comes from an image cache by a key over its inputs (`vk1-<hex>`, the
//! CI's input key lines plus the build command, or the commit for `--git`), so a second search
//! over the same versions builds nothing. Builds and replays run as child `hydo build` and `hydo
//! run` processes: a version that does not build, or a schedule that a version refuses, is that
//! version's outcome (`cannot-run`) and never ends the search.
//!
//! One replay per version is enough: hydo replays a schedule exactly, so a version's verdict on
//! one engine and image is a fact, not a sample. The search replays the newest version and the
//! oldest, then bisects every range between two decided versions whose outcomes differ. It
//! assumes one change point (passes ... reproduces, or reproduces ... passes for a fix) and says
//! when the replays contradict it. `--all` replays every version.
//!
//! The answer is `versions.json` (`hydo_results::VersionsDoc`, one `Versions` per finding), a
//! table on stdout, and a line in `hydo report` of the run or exploration it came from.
//!
//! Tests: the scripted engine (`HYDO_SCRIPTED_ENGINE`, `hydo_explore::scripted`) answers the
//! replays, with the version's image in `HYDO_SCRIPTED_IMAGE`, and builds the images (`<program>
//! build <manifest> <out dir> KEY=VALUE...`), so the whole command runs without a VM
//! (tests/versions.rs).

use crate::ci::{inputkey, key};
use crate::ctx::{abspath, die, take, Ctx, Platform};
use crate::runs::RunOpts;
use hydo_results::{version_outcome as vo, VersionOutcome, Versions, VersionsDoc};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const HELP: &str = r#"hydo versions -- which versions of the system have a finding (hydo guide workflow)

    hydo versions <run dir | explore dir | "<schedule>"> VERSIONS [options]

  replays a finding's minimal schedule on versions of the system, oldest first, and says for
  each: reproduced (the same check failed the same way), passes, fails otherwise, or cannot run
  (the version does not build, or the schedule names something the version lacks). It replays
  the newest and the oldest version first, then bisects for the first version that reproduces
  and the last that passes, assuming one change point, and says when the replays contradict it.
  One replay per version is enough: hydo replays exactly, so each verdict is exact for this
  engine and image.

  VERSIONS (one of; oldest first):
    --param KEY=V1,V2,...     each version is `hydo build MANIFEST --set KEY=<version>`
    --git REV,REV,... | A..B  each version is a checkout of a git revision (A..B: A, then every
                              first-parent commit after it up to B), its --build command, and
                              `hydo build` of its MANIFEST
    --images NAME=IMG,...     each version is a ready image (repeatable)

  Options:
    --manifest FILE     the manifest (default hydo/hydo.yaml; with --git, relative to --project)
    --project DIR       --git: the project's directory in its repository (default .)
    --build CMD         run before each `hydo build`, with HYDO_VERSION set to the version: in the
                        manifest's directory (--param) or in the checkout's project directory (--git)
    --set KEY=VALUE     a parameter for every version's build (repeatable)
    --image IMG         with a schedule: the image the finding was found on (one replay there
                        names the finding); without it, the first failing end names it
    --found-on VERSION  the version the finding was found on: replayed first
    --key KEY           the finding key to look for (f1:<kind>:<hex>, as ci-run.json has it)
    --finding N         an exploration's N-th finding only (default: each finding)
    --all               replay every version instead of searching
    --out DIR           where the replays and versions.json go (default <dir>/versions for a
                        run or explore dir, hydo-out/versions for a schedule)
    --cache DIR         the image cache (default $HYDO_HOME/versions)
    --vcpus N, --turn-margin N   as for hydo run (default: the recorded run's)

  `hydo report <dir>` prints the answer with the run's or exploration's report.
  The exit status is 0 when the search finished, whatever it found.
"#;

// ------------------------------------------------------------------ the search (pure)

/// A version's side of the change point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Reproduced.
    Bad,
    /// Passes.
    Good,
    /// Replayed, neither side (fails otherwise, cannot run).
    Undecided,
}

/// The next version to replay, given what is known (None: unknown), or None when the search is
/// done. The newest version first (stepping older past undecided ones), then the oldest
/// (stepping newer), then the middle of each range between two decided versions whose sides
/// differ. `all`: every version, newest first.
pub fn next(states: &[Option<Side>], all: bool) -> Option<usize> {
    let n = states.len();
    if all {
        return (0..n).rev().find(|&i| states[i].is_none());
    }
    let decided = |s: &Option<Side>| matches!(s, Some(Side::Bad | Side::Good));
    let mut hi = None;
    for i in (0..n).rev() {
        match states[i] {
            None => return Some(i),
            Some(Side::Undecided) => continue,
            Some(_) => {
                hi = Some(i);
                break;
            }
        }
    }
    let hi = hi?;
    for (i, s) in states.iter().enumerate().take(hi) {
        match s {
            None => return Some(i),
            Some(Side::Undecided) => continue,
            Some(_) => break,
        }
    }
    let d: Vec<usize> = (0..n).filter(|&i| decided(&states[i])).collect();
    for w in d.windows(2) {
        let (a, b) = (w[0], w[1]);
        if states[a] == states[b] {
            continue;
        }
        let mid = (a + b) / 2;
        let best = (a + 1..b)
            .filter(|&i| states[i].is_none())
            .min_by_key(|&i| (i.abs_diff(mid), i));
        if best.is_some() {
            return best;
        }
    }
    None
}

/// What the replays say, by version index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub first_bad: Option<usize>,
    pub last_good: Option<usize>,
    pub last_bad: Option<usize>,
    pub fixed_in: Option<usize>,
    pub still_present: Option<bool>,
    /// More than one change between consecutive decided versions.
    pub contradiction: bool,
}

pub fn answer(states: &[Option<Side>]) -> Answer {
    let d: Vec<(usize, Side)> = states
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match s {
            Some(x @ (Side::Bad | Side::Good)) => Some((i, *x)),
            _ => None,
        })
        .collect();
    let changes = d.windows(2).filter(|w| w[0].1 != w[1].1).count();
    let first_bad = d.iter().position(|(_, s)| *s == Side::Bad);
    let last_bad = d.iter().rposition(|(_, s)| *s == Side::Bad);
    Answer {
        first_bad: first_bad.map(|k| d[k].0),
        last_good: first_bad
            .filter(|&k| k > 0 && d[k - 1].1 == Side::Good)
            .map(|k| d[k - 1].0),
        last_bad: last_bad.map(|k| d[k].0),
        fixed_in: last_bad.filter(|&k| k + 1 < d.len()).map(|k| d[k + 1].0),
        still_present: match states.last() {
            Some(Some(Side::Bad)) => Some(true),
            Some(Some(Side::Good)) => Some(false),
            _ => None,
        },
        contradiction: changes > 1,
    }
}

/// The answer in one sentence (labels by index; `n` versions).
pub fn sentence(a: &Answer, states: &[Option<Side>], labels: &[String]) -> String {
    let n = labels.len();
    let undecided_between = |lo: usize, hi: usize| -> Vec<String> {
        (lo + 1..hi)
            .filter(|&i| states[i] != Some(Side::Bad) && states[i] != Some(Side::Good))
            .map(|i| labels[i].clone())
            .collect()
    };
    let mut parts = Vec::new();
    if a.contradiction {
        let seq: Vec<String> = states
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                Some(Side::Bad) => Some(format!("{} reproduces", labels[i])),
                Some(Side::Good) => Some(format!("{} passes", labels[i])),
                _ => None,
            })
            .collect();
        parts.push(format!(
            "the replays contradict a single change point ({})",
            seq.join(", ")
        ));
    }
    let Some(fb) = a.first_bad else {
        let decided = states
            .iter()
            .filter(|s| matches!(s, Some(Side::Good)))
            .count();
        return if decided == 0 {
            "no version gave an answer: every version replayed failed otherwise or could not run"
                .to_string()
        } else {
            format!("not reproduced on any version replayed ({decided} of {n} pass)")
        };
    };
    match a.last_good {
        Some(lg) => {
            let mut s = format!("introduced in {} ({} passes", labels[fb], labels[lg]);
            let u = undecided_between(lg, fb);
            if !u.is_empty() {
                s.push_str(&format!("; {} in between not decided", u.join(", ")));
            }
            s.push(')');
            parts.push(s);
        }
        None if fb == 0 => parts.push(format!(
            "present in {}, the oldest version tried",
            labels[fb]
        )),
        None => parts.push(format!(
            "present in {}, the oldest version decided",
            labels[fb]
        )),
    }
    if let (Some(lb), Some(fx)) = (a.last_bad, a.fixed_in) {
        let mut s = format!("fixed in {} ({} reproduces", labels[fx], labels[lb]);
        let u = undecided_between(lb, fx);
        if !u.is_empty() {
            s.push_str(&format!("; {} in between not decided", u.join(", ")));
        }
        s.push(')');
        parts.push(s);
    }
    match a.still_present {
        Some(true) => parts.push(format!(
            "still present in {}, the newest version tried",
            labels[n - 1]
        )),
        Some(false) if a.fixed_in.is_none() || a.fixed_in != Some(n - 1) => parts.push(format!(
            "not present in {}, the newest version tried",
            labels[n - 1]
        )),
        Some(false) => {}
        None => parts.push(format!(
            "the newest version, {}, gave no answer",
            labels[n - 1]
        )),
    }
    parts.join("; ")
}

// ------------------------------------------------------------------ findings

/// A recorded run of the finding: a version whose image has these bytes is answered by it.
#[derive(Clone, Debug, Default)]
struct Recorded {
    image_sha: String,
    backend: Option<String>,
    verdict: String,
    fp: String,
    keys: Vec<String>,
}

/// One finding to place among the versions.
#[derive(Clone, Debug, Default)]
struct Finding {
    schedule: String,
    key: Option<String>,
    title: Option<String>,
    found_on: Option<String>,
    class: Option<String>,
    recorded: Option<Recorded>,
    /// Every (key, title) the recorded run gave, for `--key`.
    alt: Vec<(String, String)>,
}

/// Every finding key of a run: its verdict's, and each new error kind's.
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

/// The finding of a run directory: its verdict's key, else its first new error kind.
fn finding_of_run(run: &hydo_results::Run, dir: &Path) -> Finding {
    let keys = run_keys(run);
    let mut alt: Vec<(String, String)> = key::of_verdict(&run.verdict.text, &run.findings)
        .map(|x| vec![(x.2, x.1)])
        .unwrap_or_default();
    for f in run.findings_of(hydo_results::kind::ERROR_LOG) {
        if f.status == hydo_results::status::NEW {
            alt.push((
                key::key(hydo_results::kind::ERROR_LOG, &f.id),
                format!("new error kind {}: {}", f.id, f.message),
            ));
        }
    }
    Finding {
        schedule: run.schedule.clone(),
        key: alt.first().map(|x| x.0.clone()),
        title: alt.first().map(|x| x.1.clone()),
        alt,
        found_on: Some(dir.display().to_string()),
        class: run.class.clone(),
        recorded: run.image.as_ref().and_then(|i| {
            Some(Recorded {
                image_sha: i.sha256.clone()?,
                backend: run.engine.as_ref().and_then(|e| e.backend.clone()),
                verdict: run.verdict.text.clone(),
                fp: run.fingerprints.fp.clone(),
                keys: keys.clone(),
            })
        }),
    }
}

/// The findings of an exploration: one per minimal schedule that reproduced (its first
/// confirmation's verdict), and one per new error kind with a schedule.
fn findings_of_exploration(doc: &hydo_results::Exploration, dir: &Path) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let backend = doc.engine.backend.clone();
    for s in &doc.shrinks {
        let fps: Vec<&String> = s
            .confirmations
            .iter()
            .filter_map(|c| c.fp.as_ref())
            .collect();
        if !s.identical && fps.len() > 1 {
            continue; // not deterministic: a hydo or environment problem, never a finding
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
        let Some((_, title, k)) = key::of_verdict(&verdict, &fails) else {
            continue;
        };
        if out.iter().any(|f| f.key.as_deref() == Some(&k)) {
            continue;
        }
        out.push(Finding {
            schedule: s.minimal.clone(),
            key: Some(k.clone()),
            title: Some(title),
            found_on: Some(dir.display().to_string()),
            class: Some(s.class.clone()),
            recorded: match (&doc.image.sha256, fps.first()) {
                (Some(sha), Some(fp)) => Some(Recorded {
                    image_sha: sha.clone(),
                    backend: backend.clone(),
                    verdict: verdict.clone(),
                    fp: (*fp).clone(),
                    keys: vec![k],
                }),
                _ => None,
            },
            alt: Vec::new(),
        });
    }
    if let Some(ek) = &doc.error_kinds {
        for k in ek.kinds.iter().filter(|k| k.new) {
            let schedule = k
                .shrink
                .as_ref()
                .map(|s| s.minimal.clone())
                .or_else(|| k.first_new.as_ref().map(|f| f.schedule.clone()))
                .unwrap_or_else(|| k.first_schedule.clone());
            out.push(Finding {
                schedule,
                key: Some(key::key(hydo_results::kind::ERROR_LOG, &k.signature)),
                title: Some(format!("[{}] {}", k.service, k.template)),
                found_on: Some(dir.display().to_string()),
                class: Some("error-log".into()),
                recorded: None,
                alt: Vec::new(),
            });
        }
    }
    out
}

// ------------------------------------------------------------------ versions and their images

/// How the versions are made.
#[derive(Clone, Debug)]
enum Source {
    Param {
        key: String,
        manifest: PathBuf,
    },
    Git {
        top: PathBuf,
        rel: PathBuf,
        manifest: PathBuf,
    },
    Images,
}

impl Source {
    fn name(&self) -> String {
        match self {
            Source::Param { key, .. } => format!("param {key}"),
            Source::Git { .. } => "git".into(),
            Source::Images => "images".into(),
        }
    }
}

#[derive(Clone, Debug)]
struct Version {
    label: String,
    commit: Option<String>,
    /// A given image, or why the version cannot be made at all (an unknown revision).
    image: Option<PathBuf>,
    bad: Option<String>,
}

/// A version's image, once made.
#[derive(Clone, Debug)]
struct Made {
    path: PathBuf,
    sha: Option<String>,
    reused: bool,
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A short name for files: letters, digits and `._-`.
fn slug(s: &str) -> String {
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
    t.chars().take(48).collect()
}

fn image_suffix(ctx: &Ctx) -> &'static str {
    match ctx.platform {
        Platform::Linux => "-disk.img",
        Platform::Mac => "-arm64.cpio.gz",
    }
}

/// The one image `hydo build` wrote into `dir`.
fn image_in(ctx: &Ctx, dir: &Path) -> Option<PathBuf> {
    let suffix = image_suffix(ctx);
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

/// The last line of a log that says something.
fn last_line(p: &Path) -> String {
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().chars().take(300).collect())
        .unwrap_or_default()
}

/// Run `cmd` with its output into `log`; Err names the log's last line.
fn run_logged(mut cmd: Command, log: &Path, what: &str) -> Result<(), String> {
    let f = std::fs::File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
    let f2 = f
        .try_clone()
        .map_err(|e| format!("{}: {e}", log.display()))?;
    let st = cmd
        .stdout(f)
        .stderr(f2)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("{what}: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!(
            "{what} failed ({st}): {} ({})",
            last_line(log),
            log.display()
        ))
    }
}

/// Builds each version's image once, through the image cache.
struct Maker<'a> {
    ctx: &'a Ctx,
    source: Source,
    sets: Vec<(String, String)>,
    build: Option<String>,
    cache: PathBuf,
    out: PathBuf,
    made: BTreeMap<usize, Result<Made, String>>,
}

impl Maker<'_> {
    fn arch(&self) -> &'static str {
        match self.ctx.platform {
            Platform::Linux => "x86_64",
            Platform::Mac => "arm64",
        }
    }

    fn hydo_id() -> String {
        format!(
            "{} {}",
            hydo_image::hydo_version(),
            hydo_image::hydo_commit().unwrap_or("-")
        )
    }

    fn image(&mut self, i: usize, v: &Version) -> Result<Made, String> {
        if let Some(m) = self.made.get(&i) {
            return m.clone();
        }
        let m = self.make(v);
        self.made.insert(i, m.clone());
        m
    }

    fn make(&self, v: &Version) -> Result<Made, String> {
        if let Some(b) = &v.bad {
            return Err(b.clone());
        }
        if let Some(p) = &v.image {
            if !p.is_file() {
                return Err(format!("no such image: {}", p.display()));
            }
            return Ok(Made {
                path: p.clone(),
                sha: hydo_engine::image::sha256_file(p).ok(),
                reused: false,
            });
        }
        let logs = self.out.join("builds");
        let _ = std::fs::create_dir_all(&logs);
        match &self.source {
            Source::Images => Err("no image".into()),
            Source::Param { key: k, manifest } => {
                let dir = manifest.parent().unwrap_or(Path::new(".")).to_path_buf();
                let mut sets = self.sets.clone();
                sets.push((k.clone(), v.label.clone()));
                if let Some(c) = &self.build {
                    let mut cmd = Command::new("sh");
                    cmd.args(["-c", c])
                        .current_dir(&dir)
                        .env("HYDO_VERSION", &v.label);
                    run_logged(
                        cmd,
                        &logs.join(format!("{}.build-cmd.log", slug(&v.label))),
                        "the build command",
                    )?;
                }
                let opts = hydo_manifest::LoadOptions {
                    arch: self.arch().into(),
                    sets: sets.clone(),
                    onboard_dir: self.ctx.onboard(),
                    ..Default::default()
                };
                let loaded = hydo_manifest::load(manifest, &opts)
                    .map_err(|e| format!("{}: {e}", manifest.display()))?;
                let mut srcs = Vec::new();
                inputkey::src_paths(&loaded.manifest.data, &mut srcs);
                let none = BTreeMap::new();
                let (mut lines, _) = inputkey::input_key(&inputkey::Inputs {
                    hydo: Self::hydo_id(),
                    arch: self.arch(),
                    sets: &sets,
                    manifest,
                    project: &dir,
                    srcs: &srcs,
                    inputs: &none,
                    built_here: false,
                });
                if let Some(c) = &self.build {
                    lines.push(format!("build {c}"));
                }
                self.cached_or_built(&lines, &v.label, manifest, &dir, &sets)
            }
            Source::Git {
                top, rel, manifest, ..
            } => {
                let commit = v.commit.clone().unwrap_or_default();
                let mut lines = vec![
                    format!("hydo {}", Self::hydo_id()),
                    format!("arch {}", self.arch()),
                    format!("commit {commit}"),
                    format!("project {}", rel.display()),
                    format!("manifest {}", manifest.display()),
                ];
                for (k, val) in &self.sets {
                    lines.push(format!("set {k}={val}"));
                }
                if let Some(c) = &self.build {
                    lines.push(format!("build {c}"));
                }
                if let Some(m) = self.cache_hit(&lines) {
                    return Ok(m);
                }
                // a detached checkout of the commit, removed afterwards
                let wt = self.out.join("src").join(slug(&v.label));
                let wts = wt.display().to_string();
                if wt.exists() {
                    let _ = git(top, &["worktree", "remove", "--force", &wts]);
                    let _ = std::fs::remove_dir_all(&wt);
                    let _ = git(top, &["worktree", "prune"]);
                }
                let _ = std::fs::create_dir_all(wt.parent().unwrap_or(Path::new(".")));
                let mut cmd = Command::new("git");
                cmd.arg("-C")
                    .arg(top)
                    .args(["worktree", "add", "--detach", "--force"])
                    .arg(&wt)
                    .arg(&commit);
                run_logged(
                    cmd,
                    &logs.join(format!("{}.checkout.log", slug(&v.label))),
                    "git worktree add",
                )?;
                let proj = wt.join(rel);
                let r = (|| {
                    if let Some(c) = &self.build {
                        let mut cmd = Command::new("sh");
                        cmd.args(["-c", c])
                            .current_dir(&proj)
                            .env("HYDO_VERSION", &v.label);
                        run_logged(
                            cmd,
                            &logs.join(format!("{}.build-cmd.log", slug(&v.label))),
                            "the build command",
                        )?;
                    }
                    let m = proj.join(manifest);
                    if !m.is_file() {
                        return Err(format!("no {} at {commit}", manifest.display()));
                    }
                    let mdir = m.parent().unwrap_or(&proj).to_path_buf();
                    self.cached_or_built(&lines, &v.label, &m, &mdir, &self.sets)
                })();
                let _ = git(top, &["worktree", "remove", "--force", &wts]);
                let _ = git(top, &["worktree", "prune"]);
                r
            }
        }
    }

    fn key_of(lines: &[String]) -> String {
        let mut l = lines.to_vec();
        l.sort();
        l.dedup();
        let mut h = Sha256::new();
        for x in &l {
            h.update(x.as_bytes());
            h.update(b"\n");
        }
        format!("vk1-{}", &hex(&h.finalize())[..32])
    }

    fn cache_hit(&self, lines: &[String]) -> Option<Made> {
        let dir = self.cache.join("images").join(Self::key_of(lines));
        let p = image_in(self.ctx, &dir)?;
        Some(Made {
            sha: hydo_engine::image::sha256_file(&p).ok(),
            path: p,
            reused: true,
        })
    }

    /// The image of `manifest` with `sets`: from the cache by the key of `lines`, or built.
    fn cached_or_built(
        &self,
        lines: &[String],
        label: &str,
        manifest: &Path,
        cwd: &Path,
        sets: &[(String, String)],
    ) -> Result<Made, String> {
        if let Some(m) = self.cache_hit(lines) {
            println!("hydo versions: {label}: the image from the image cache");
            return Ok(m);
        }
        let k = Self::key_of(lines);
        let images = self.cache.join("images");
        let dir = images.join(&k);
        let tmp = images.join(format!(".{k}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let log = self
            .out
            .join("builds")
            .join(format!("{}.build.log", slug(label)));
        let kv: Vec<String> = sets.iter().map(|(a, b)| format!("{a}={b}")).collect();
        let cmd = match &self.ctx.config.pool.scripted_engine {
            // the tests: the scripted engine program builds a stand-in image
            Some(p) => {
                let mut c = Command::new(p);
                c.arg("build").arg(manifest).arg(&tmp).args(&kv);
                c
            }
            None => {
                let mut c = Command::new(
                    std::env::current_exe().map_err(|e| format!("this program: {e}"))?,
                );
                c.arg("build").arg(manifest).arg("--out").arg(&tmp);
                for s in &kv {
                    c.arg("--set").arg(s);
                }
                c
            }
        };
        let mut cmd = cmd;
        cmd.current_dir(cwd);
        println!(
            "hydo versions: {label}: hydo build {} {}",
            manifest.display(),
            kv.iter()
                .map(|s| format!("--set {s}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        if let Err(e) = run_logged(cmd, &log, "hydo build") {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        let _ = std::fs::write(tmp.join("input-key.txt"), lines.join("\n") + "\n");
        // the build's intermediates (root trees, file systems before they are joined; on the
        // Mac every other output file is part of the image), as the re-baseline's stage.sh
        if let Ok(rd) = std::fs::read_dir(&tmp) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                let linux_only = n.ends_with("-ext4.img")
                    || n.ends_with("-data.img")
                    || n.ends_with("-oci-tmp")
                    || n.ends_with("-root.closures");
                if n.ends_with("-root") || (linux_only && self.ctx.platform == Platform::Linux) {
                    let p = e.path();
                    if p.is_dir() {
                        // a root tree holds read-only copies (Nix store paths)
                        let _ = Command::new("chmod")
                            .args(["-R", "u+w"])
                            .arg(&p)
                            .stderr(Stdio::null())
                            .status();
                        let _ = std::fs::remove_dir_all(&p);
                    } else {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        }
        if std::fs::rename(&tmp, &dir).is_err() {
            let _ = std::fs::remove_dir_all(&tmp); // another search put the same image there
        }
        let p = image_in(self.ctx, &dir)
            .ok_or_else(|| format!("hydo build wrote no image ({})", log.display()))?;
        Ok(Made {
            sha: hydo_engine::image::sha256_file(&p).ok(),
            path: p,
            reused: false,
        })
    }
}

// ------------------------------------------------------------------ replays

/// One replay's result.
#[derive(Clone, Debug, Default)]
struct Replay {
    verdict: Option<String>,
    fp: Option<String>,
    keys: Vec<String>,
    /// Why there is no verdict.
    why: Option<String>,
}

fn replay(ctx: &Ctx, opts: &RunOpts, image: &Path, sched: &str, dir: &Path) -> Replay {
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::create_dir_all(dir);
    if let Some(p) = &ctx.config.pool.scripted_engine {
        // the tests: the scripted engine's `one` mode, told the image
        let o = Command::new(p)
            .arg("one")
            .arg(dir)
            .arg(sched)
            .env("HYDO_SCRIPTED_IMAGE", image)
            .stderr(Stdio::inherit())
            .output();
        let text = o
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        return match text
            .lines()
            .find_map(hydo_engine::protocol::RunResult::parse)
        {
            Some(r) => Replay {
                keys: key::of_verdict(&r.verdict, &[])
                    .map(|x| vec![x.2])
                    .unwrap_or_default(),
                verdict: Some(r.verdict),
                fp: Some(r.fp),
                why: None,
            },
            None => Replay {
                why: Some("the engine gave no result".into()),
                ..Replay::default()
            },
        };
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            return Replay {
                why: Some(format!("this program: {e}")),
                ..Replay::default()
            }
        }
    };
    let mut c = Command::new(exe);
    c.arg("run")
        .arg(image)
        .arg(sched)
        .arg("--out")
        .arg(dir)
        .arg("--no-diagnose");
    if ctx.platform == Platform::Linux {
        c.arg("--backend").arg(ctx.backend_name());
    }
    if let Some(n) = opts.vcpus {
        c.arg("--vcpus").arg(n.to_string());
    }
    if let Some(m) = opts.turn_margin {
        c.arg("--turn-margin").arg(m.to_string());
    }
    // `<run dir>.log` (not with_extension: a version such as 3.6.15 has dots)
    let mut log = dir.as_os_str().to_owned();
    log.push(".log");
    let log = PathBuf::from(log);
    let rc = run_logged(c, &log, "hydo run");
    match hydo_results::read_dir(dir) {
        Ok(hydo_results::Results::Run { run, .. }) => Replay {
            keys: run_keys(&run),
            verdict: Some(run.verdict.text.clone()),
            fp: Some(run.fingerprints.fp.clone()),
            why: None,
        },
        _ => {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let refused = text
                .lines()
                .find(|l| l.contains("not run (no VM was started)"))
                .map(|l| l.trim_start_matches("hydo: ").to_string());
            Replay {
                why: Some(refused.unwrap_or_else(|| match rc {
                    Err(e) => e,
                    Ok(()) => format!("the run wrote no result.json ({})", log.display()),
                })),
                ..Replay::default()
            }
        }
    }
}

/// A replay's outcome for a finding key.
fn classify(r: &Replay, k: Option<&str>) -> (&'static str, Option<String>) {
    let Some(v) = &r.verdict else {
        return (vo::CANNOT_RUN, r.why.clone());
    };
    if let Some(k) = k {
        if r.keys.iter().any(|x| x == k) {
            return (vo::REPRODUCED, None);
        }
    }
    if hydo_engine::protocol::verdict_kind(v) == "ok" {
        let other: Vec<&String> = r.keys.iter().filter(|x| Some(x.as_str()) != k).collect();
        let why = (!other.is_empty()).then(|| {
            format!(
                "passed; other findings: {}",
                other
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        return (vo::PASSES, why);
    }
    (vo::FAILS_OTHERWISE, None)
}

fn side_of(outcome: &str) -> Side {
    match outcome {
        vo::REPRODUCED => Side::Bad,
        vo::PASSES => Side::Good,
        _ => Side::Undecided,
    }
}

// ------------------------------------------------------------------ one finding

struct Search<'a, 'b> {
    ctx: &'a Ctx,
    opts: RunOpts,
    maker: &'b mut Maker<'a>,
    versions: &'b [Version],
    all: bool,
    found_on: Option<usize>,
    out: PathBuf,
}

impl Search<'_, '_> {
    fn one(&mut self, n: usize, f: &Finding) -> Versions {
        let labels: Vec<String> = self.versions.iter().map(|v| v.label.clone()).collect();
        let mut rows: Vec<VersionOutcome> = self
            .versions
            .iter()
            .map(|v| VersionOutcome {
                version: v.label.clone(),
                outcome: vo::NOT_REPLAYED.into(),
                commit: v.commit.clone(),
                ..Default::default()
            })
            .collect();
        let mut states: Vec<Option<Side>> = vec![None; self.versions.len()];
        let mut key = f.key.clone();
        let mut title = f.title.clone();
        let mut found_on = f.found_on.clone();
        let mut replays = 0u64;
        let mut first = self.found_on;
        while let Some(i) = first.take().or_else(|| next(&states, self.all)) {
            let v = &self.versions[i];
            let row = &mut rows[i];
            let made = self.maker.image(i, v);
            let r = match made {
                Err(e) => Replay {
                    why: Some(e),
                    ..Replay::default()
                },
                Ok(m) => {
                    row.image_sha256 = m.sha.clone();
                    row.image_reused = Some(m.reused);
                    let rec = f.recorded.as_ref().filter(|r| {
                        m.sha.as_deref() == Some(r.image_sha.as_str())
                            && r.backend
                                .as_deref()
                                .is_none_or(|b| b == self.ctx.backend_name())
                    });
                    match rec {
                        Some(r) => {
                            println!(
                                "hydo versions: {}: the finding's own recorded run (the same image)",
                                v.label
                            );
                            row.recorded = Some(true);
                            Replay {
                                verdict: Some(r.verdict.clone()),
                                fp: Some(r.fp.clone()),
                                keys: r.keys.clone(),
                                why: None,
                            }
                        }
                        None => {
                            replays += 1;
                            let d = self
                                .out
                                .join("runs")
                                .join(format!("{n:02}-{replays:02}-{}", slug(&v.label)));
                            println!(
                                "hydo versions: {}: replay {replays}: {}",
                                v.label, f.schedule
                            );
                            let r = replay(self.ctx, &self.opts, &m.path, &f.schedule, &d);
                            row.replay = Some(replays);
                            row.dir = Some(
                                d.strip_prefix(&self.out)
                                    .unwrap_or(&d)
                                    .display()
                                    .to_string(),
                            );
                            r
                        }
                    }
                }
            };
            // without a key, the first failing replay names the finding (the newest end first)
            if key.is_none() {
                if let Some(v) = r.verdict.as_deref() {
                    if let Some((_, t, k)) = key::of_verdict(v, &[]) {
                        key = Some(k);
                        title = Some(t);
                        found_on = Some(format!("version {}", labels[i]));
                    }
                }
            }
            let (o, why) = classify(&r, key.as_deref());
            println!(
                "hydo versions: {}: {o}{}",
                labels[i],
                r.verdict
                    .as_ref()
                    .map(|v| format!(" ({v})"))
                    .or_else(|| why.as_ref().map(|w| format!(" ({w})")))
                    .unwrap_or_default()
            );
            row.outcome = o.into();
            row.fingerprint = r.fp.clone();
            row.verdict = r.verdict.clone();
            // the key that matched, else the run's first
            row.key = if o == vo::REPRODUCED {
                key.clone()
            } else {
                r.keys.first().cloned()
            };
            row.why = why;
            states[i] = Some(side_of(o));
        }
        let a = answer(&states);
        let lab = |i: Option<usize>| i.map(|i| labels[i].clone());
        let mut notes = vec![
            "one replay per version: hydo replays a schedule exactly, so each verdict is exact for \
             this engine and image"
                .to_string(),
        ];
        let flaky = f.class.as_deref() == Some("test-failed")
            || self.opts.vcpus.is_some_and(|n| n > 1)
            || f.schedule
                .split_whitespace()
                .any(|t| t.starts_with("q=") || t.starts_with("pct="));
        if flaky {
            notes.push(
                "a flaky test's verdict is exact per engine and image too, but another engine, \
                 vCPU count or turn length (q=) can give another verdict"
                    .into(),
            );
        }
        if rows.iter().any(|r| r.outcome == vo::FAILS_OTHERWISE) {
            notes.push(
                "a version that fails otherwise may hide the finding behind another failure; \
                 the search leaves it undecided"
                    .into(),
            );
        }
        if key.is_none() {
            notes.push("no replay failed, so the finding has no key to compare".into());
        }
        if let Some(i) = self.found_on {
            if rows[i].outcome != vo::REPRODUCED {
                notes.push(format!(
                    "the finding does not reproduce on {}, the version it was found on, as built \
                     here: its verdict depends on the image's exact bytes (the guest's timing), so \
                     the other versions' outcomes say little about it",
                    labels[i]
                ));
            }
        }
        Versions {
            schedule: f.schedule.clone(),
            key,
            title,
            found_on,
            source: self.maker.source.name(),
            first_bad: lab(a.first_bad),
            last_good: lab(a.last_good),
            last_bad: lab(a.last_bad),
            fixed_in: lab(a.fixed_in),
            still_present_in_newest: a.still_present,
            contradiction: a.contradiction.then(|| sentence(&a, &states, &labels)),
            replays,
            summary: sentence(&a, &states, &labels),
            notes,
            versions: rows,
        }
    }
}

// ------------------------------------------------------------------ text

/// The table and the answer of one finding, for people.
pub fn text(v: &Versions) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "finding:  {}\n",
        v.title.as_deref().unwrap_or("-")
    ));
    if let Some(k) = &v.key {
        s.push_str(&format!("key:      {k}\n"));
    }
    s.push_str(&format!("schedule: {}\n", v.schedule));
    s.push_str(&format!("versions: {} (oldest first)\n\n", v.source));
    let w = v
        .versions
        .iter()
        .map(|r| r.version.chars().count())
        .max()
        .unwrap_or(7)
        .max(7);
    s.push_str(&format!(
        "  {:<w$}  {:<15}  {:<16}  {:<6}  verdict\n",
        "version", "outcome", "fingerprint", "replay"
    ));
    for r in &v.versions {
        let how = match (r.recorded, r.replay) {
            (Some(true), _) => "record".to_string(),
            (_, Some(n)) => n.to_string(),
            _ => "-".to_string(),
        };
        let detail = r
            .verdict
            .clone()
            .or_else(|| r.why.clone())
            .unwrap_or_default();
        let detail: String = detail.chars().take(100).collect();
        let line = format!(
            "  {:<w$}  {:<15}  {:<16}  {:<6}  {}",
            r.version,
            r.outcome,
            r.fingerprint.as_deref().unwrap_or("-"),
            how,
            detail
        );
        s.push_str(line.trim_end());
        s.push('\n');
    }
    let n = v.versions.len();
    s.push_str(&format!(
        "\n{} ({} replays for {n} versions)\n",
        v.summary, v.replays
    ));
    for x in &v.notes {
        s.push_str(&format!("note: {x}\n"));
    }
    s
}

/// The lines `hydo report` adds for a versions document: Markdown for an exploration's report,
/// plain lines for a run's.
pub fn report_lines(doc: &VersionsDoc, file: &Path, md: bool) -> String {
    let mut s = String::new();
    if md {
        s.push_str(&format!(
            "\n## Versions\n\n`hydo versions` ({}):\n\n",
            file.display()
        ));
        for v in &doc.findings {
            s.push_str(&format!(
                "- `{}`: {}\n",
                v.title.as_deref().unwrap_or(&v.schedule),
                v.summary
            ));
        }
    } else {
        for v in &doc.findings {
            s.push_str(&format!(
                "versions: {} ({}; {})\n",
                v.summary,
                v.source,
                file.display()
            ));
        }
    }
    s
}

// ------------------------------------------------------------------ the command

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

fn kv(s: &str, flag: &str) -> (String, String) {
    match s.split_once('=') {
        Some((k, v)) if !k.is_empty() => (k.to_string(), v.to_string()),
        _ => die(&format!("{flag} {s}: expected KEY=VALUE")),
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

/// The versions of `--git`: a list of revisions, or `A..B` (A, then every first-parent commit
/// after it up to B).
fn git_versions(top: &Path, spec: &str) -> Vec<Version> {
    let resolve = |r: &str| {
        git(
            top,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{r}^{{commit}}"),
            ],
        )
    };
    let one = |label: &str| {
        let c = resolve(label);
        Version {
            label: label.to_string(),
            bad: c.is_none().then(|| format!("no such revision: {label}")),
            commit: c,
            image: None,
        }
    };
    if let Some((a, b)) = spec.split_once("..") {
        if resolve(a).is_none() || resolve(b).is_none() {
            die(&format!(
                "--git {spec}: {a} or {b} is not a revision of {}",
                top.display()
            ));
        }
        let list = git(
            top,
            &[
                "rev-list",
                "--first-parent",
                "--reverse",
                &format!("{a}..{b}"),
            ],
        )
        .unwrap_or_default();
        let mut v = vec![one(a)];
        for c in list.lines() {
            // B as given, else the commit's tag, else its short hash
            let label = if resolve(b).as_deref() == Some(c) {
                b.to_string()
            } else {
                git(top, &["describe", "--tags", "--exact-match", c])
                    .or_else(|| git(top, &["rev-parse", "--short=12", c]))
                    .unwrap_or_else(|| c.into())
            };
            v.push(Version {
                label,
                commit: Some(c.to_string()),
                image: None,
                bad: None,
            });
        }
        v
    } else {
        split_list(spec).iter().map(|r| one(r)).collect()
    }
}

pub fn cmd_versions(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return 0;
    }
    let param = take(&mut args, "--param");
    let git_spec = take(&mut args, "--git");
    let images: Vec<String> = take_all(&mut args, "--images");
    let manifest_flag = take(&mut args, "--manifest");
    let project_flag = take(&mut args, "--project");
    let build = take(&mut args, "--build");
    let sets: Vec<(String, String)> = take_all(&mut args, "--set")
        .iter()
        .map(|s| kv(s, "--set"))
        .collect();
    let ref_image = take(&mut args, "--image");
    let found_on_flag = take(&mut args, "--found-on");
    let key_flag = take(&mut args, "--key");
    let finding_flag = take(&mut args, "--finding");
    let all = take_flag(&mut args, "--all");
    let out_flag = take(&mut args, "--out");
    let cache_flag = take(&mut args, "--cache");
    let mut opts = RunOpts::take(ctx, &mut args);
    if args.len() != 1 {
        die("usage: hydo versions <run dir | explore dir | \"<schedule>\"> --param KEY=V1,V2,... | --git REVS | --images NAME=IMG,... [options] (hydo versions --help)");
    }
    if let Some(a) = args.iter().find(|a| a.starts_with("--")) {
        die(&format!(
            "hydo versions: unknown option {a} (hydo versions --help)"
        ));
    }
    let given = args[0].clone();
    let sources = [param.is_some(), git_spec.is_some(), !images.is_empty()]
        .iter()
        .filter(|x| **x)
        .count();
    if sources != 1 {
        die("hydo versions: name the versions with one of --param, --git or --images");
    }

    // the finding(s)
    let gp = Path::new(&given);
    let (mut findings, from, default_out): (Vec<Finding>, String, PathBuf) = if gp.is_dir() {
        match hydo_results::read_dir(gp) {
            Ok(hydo_results::Results::Run { run, .. }) => {
                if opts.vcpus.is_none() {
                    if let Some(a) = run.engine.as_ref().and_then(|e| e.args.as_ref()) {
                        opts = RunOpts::of_eargs(a);
                    }
                }
                (
                    vec![finding_of_run(&run, gp)],
                    given.clone(),
                    gp.join("versions"),
                )
            }
            Ok(hydo_results::Results::Exploration { doc: Some(d), .. }) => (
                findings_of_exploration(&d, gp),
                given.clone(),
                gp.join("versions"),
            ),
            Ok(_) => die(&format!("{given}: the exploration has not finished")),
            Err(e) => die(&format!("{given}: {e}")),
        }
    } else {
        (
            vec![Finding {
                schedule: given.clone(),
                ..Finding::default()
            }],
            "schedule".into(),
            PathBuf::from(crate::ctx::OUT).join("versions"),
        )
    };
    if let Some(n) = finding_flag {
        let i: usize = n
            .parse()
            .ok()
            .filter(|i| *i >= 1 && *i <= findings.len())
            .unwrap_or_else(|| {
                die(&format!(
                    "--finding {n}: the exploration has {} findings (1..{})",
                    findings.len(),
                    findings.len()
                ))
            });
        findings = vec![findings.remove(i - 1)];
    }
    if let Some(k) = &key_flag {
        // an exploration's finding with that key, or one of a run's keys (a run that failed and
        // also had new error kinds), or a schedule's
        findings.retain(|f| {
            f.key.as_deref().is_none_or(|x| x == k) || f.alt.iter().any(|(a, _)| a == k)
        });
        for f in &mut findings {
            if let Some((_, t)) = f.alt.iter().find(|(a, _)| a == k) {
                f.title = Some(t.clone());
            }
            f.key = Some(k.clone());
        }
    }
    if findings.is_empty() {
        die(&format!("{given}: no finding to place among versions"));
    }
    let out = abspath(&out_flag.map(PathBuf::from).unwrap_or(default_out));
    let _ = std::fs::create_dir_all(&out);

    // the versions
    let (source, versions): (Source, Vec<Version>) = if let Some(p) = &param {
        let (k, list) = kv(p, "--param");
        let manifest = abspath(Path::new(
            manifest_flag.as_deref().unwrap_or("hydo/hydo.yaml"),
        ));
        if !manifest.is_file() {
            die(&format!(
                "--param: no manifest {} (--manifest FILE)",
                manifest.display()
            ));
        }
        let v = split_list(&list)
            .into_iter()
            .map(|label| Version {
                label,
                commit: None,
                image: None,
                bad: None,
            })
            .collect();
        (Source::Param { key: k, manifest }, v)
    } else if let Some(spec) = &git_spec {
        let project = abspath(Path::new(project_flag.as_deref().unwrap_or(".")));
        let top = git(&project, &["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                die(&format!(
                    "--git: {} is not in a git repository",
                    project.display()
                ))
            });
        let rel = abspath(&project)
            .strip_prefix(abspath(&top))
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let manifest = PathBuf::from(manifest_flag.as_deref().unwrap_or("hydo/hydo.yaml"));
        let v = git_versions(&top, spec);
        (Source::Git { top, rel, manifest }, v)
    } else {
        let v = images
            .iter()
            .flat_map(|s| split_list(s))
            .map(|s| {
                let (label, path) = match s.split_once('=') {
                    Some((l, p)) => (l.to_string(), p.to_string()),
                    None => (
                        Path::new(&s)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| s.clone()),
                        s.clone(),
                    ),
                };
                Version {
                    label,
                    commit: None,
                    image: Some(abspath(Path::new(&path))),
                    bad: None,
                }
            })
            .collect();
        (Source::Images, v)
    };
    if versions.is_empty() {
        die("hydo versions: no versions given");
    }
    let found_on = found_on_flag.as_ref().map(|l| {
        versions
            .iter()
            .position(|v| &v.label == l)
            .unwrap_or_else(|| die(&format!("--found-on {l}: not one of the versions")))
    });
    let cache = cache_flag
        .map(|c| abspath(Path::new(&c)))
        .unwrap_or_else(|| ctx.config.cli.cache.home.join("versions"));
    let mut maker = Maker {
        ctx,
        source,
        sets,
        build,
        cache,
        out: out.clone(),
        made: BTreeMap::new(),
    };

    // a schedule with --image: one replay there names the finding
    if let Some(img) = &ref_image {
        let img = abspath(Path::new(img));
        let f = &mut findings[0];
        if f.key.is_none() {
            println!(
                "hydo versions: the finding's image: replay of {}",
                f.schedule
            );
            let d = out.join("runs").join("00-found-on");
            let r = replay(ctx, &opts, &img, &f.schedule, &d);
            match r
                .verdict
                .as_deref()
                .and_then(|v| key::of_verdict(v, &[]).map(|x| (v, x)))
            {
                Some((v, (_, t, k))) => {
                    f.key = Some(k.clone());
                    f.title = Some(t);
                    f.found_on = Some(img.display().to_string());
                    f.recorded = hydo_engine::image::sha256_file(&img)
                        .ok()
                        .map(|sha| Recorded {
                            image_sha: sha,
                            backend: Some(ctx.backend_name().into()),
                            verdict: v.to_string(),
                            fp: r.fp.clone().unwrap_or_default(),
                            keys: r.keys.clone(),
                        });
                }
                None => die(&format!(
                    "the schedule does not fail on {}: {}",
                    img.display(),
                    r.verdict.or(r.why).unwrap_or_default()
                )),
            }
        }
    }

    let mut doc = VersionsDoc {
        hydo_version: Some(hydo_image::hydo_version().to_string()),
        epoch: Some(hydo_image::EPOCH),
        engine: hydo_results::Engine {
            backend: Some(ctx.backend_name().into()),
            vcpus: opts.vcpus.map(u64::from),
            ..Default::default()
        },
        from,
        ..Default::default()
    };
    let mut texts = Vec::new();
    for (n, f) in findings.iter().enumerate() {
        let mut s = Search {
            ctx,
            opts: opts.clone(),
            maker: &mut maker,
            versions: &versions,
            all,
            found_on,
            out: out.clone(),
        };
        let v = s.one(n + 1, f);
        texts.push(text(&v));
        doc.findings.push(v);
    }
    let p = out.join(hydo_results::VERSIONS_DOC);
    if let Err(e) = hydo_results::write_document(&p, &doc) {
        die(&format!("{}: {e}", p.display()));
    }
    println!();
    for t in &texts {
        println!("{t}");
    }
    println!("hydo versions: {}", p.display());
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use Side::{Bad, Good, Undecided};

    /// Run the search over a pattern of sides; returns the replay order and the states.
    fn run(pattern: &[Side], all: bool) -> (Vec<usize>, Vec<Option<Side>>) {
        let mut st = vec![None; pattern.len()];
        let mut order = Vec::new();
        while let Some(i) = next(&st, all) {
            assert!(st[i].is_none(), "replayed {i} twice");
            order.push(i);
            st[i] = Some(pattern[i]);
        }
        (order, st)
    }

    fn labels(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("v{i}")).collect()
    }

    #[test]
    fn the_newest_and_the_oldest_first_then_bisection() {
        // introduced at index 5 of 8 (v6)
        let p = [Good, Good, Good, Good, Good, Bad, Bad, Bad];
        let (order, st) = run(&p, false);
        assert_eq!(&order[..2], &[7, 0]);
        assert!(order.len() <= 2 + 3, "{order:?}");
        let a = answer(&st);
        assert_eq!((a.first_bad, a.last_good), (Some(5), Some(4)));
        assert_eq!(a.still_present, Some(true));
        assert!(!a.contradiction);
        assert_eq!(
            sentence(&a, &st, &labels(8)),
            "introduced in v6 (v5 passes); still present in v8, the newest version tried"
        );
    }

    #[test]
    fn present_in_every_version_needs_two_replays() {
        let p = [Bad; 6];
        let (order, st) = run(&p, false);
        assert_eq!(order, [5, 0]);
        let a = answer(&st);
        assert_eq!((a.first_bad, a.last_good), (Some(0), None));
        assert_eq!(
            sentence(&a, &st, &labels(6)),
            "present in v1, the oldest version tried; still present in v6, the newest version tried"
        );
    }

    #[test]
    fn a_fix_is_found_the_same_way() {
        let p = [Bad, Bad, Good, Good, Good];
        let (_, st) = run(&p, false);
        let a = answer(&st);
        assert_eq!((a.last_bad, a.fixed_in), (Some(1), Some(2)));
        assert_eq!(a.still_present, Some(false));
        assert_eq!(
            sentence(&a, &st, &labels(5)),
            "present in v1, the oldest version tried; fixed in v3 (v2 reproduces); not present in \
             v5, the newest version tried"
        );
    }

    #[test]
    fn undecided_versions_are_stepped_over() {
        // v8 cannot run, v5 cannot run; introduced at v6
        let p = [Good, Good, Good, Good, Undecided, Bad, Bad, Undecided];
        let (order, st) = run(&p, false);
        assert_eq!(&order[..3], &[7, 6, 0]);
        let a = answer(&st);
        assert_eq!((a.first_bad, a.last_good), (Some(5), Some(3)));
        assert_eq!(a.still_present, None);
        assert_eq!(
            sentence(&a, &st, &labels(8)),
            "introduced in v6 (v4 passes; v5 in between not decided); the newest version, v8, gave \
             no answer"
        );
    }

    #[test]
    fn contradictions_are_named() {
        let p = [Good, Bad, Good, Bad];
        let (order, st) = run(&p, true);
        assert_eq!(order, [3, 2, 1, 0]);
        let a = answer(&st);
        assert!(a.contradiction);
        let s = sentence(&a, &st, &labels(4));
        assert!(
            s.starts_with(
                "the replays contradict a single change point (v1 passes, v2 reproduces, v3 \
                 passes, v4 reproduces)"
            ),
            "{s}"
        );
        // the search sees one too when the version the finding was found on reproduces between
        // two ends that pass: introduced and fixed again
        let mut st = vec![None, Some(Bad), None, None, None];
        let pat = [Good, Bad, Good, Good, Good];
        while let Some(i) = next(&st, false) {
            st[i] = Some(pat[i]);
        }
        let a = answer(&st);
        assert!(a.contradiction, "{st:?}");
        assert_eq!((a.first_bad, a.fixed_in), (Some(1), Some(2)));
    }

    #[test]
    fn nothing_decided_and_nothing_reproduced() {
        let p = [Undecided, Undecided];
        let (order, st) = run(&p, false);
        assert_eq!(order, [1, 0]);
        assert!(sentence(&answer(&st), &st, &labels(2)).starts_with("no version gave an answer"));
        let p = [Good, Good, Good];
        let (order, st) = run(&p, false);
        assert_eq!(order, [2, 0]);
        assert_eq!(
            sentence(&answer(&st), &st, &labels(3)),
            "not reproduced on any version replayed (2 of 3 pass)"
        );
    }

    #[test]
    fn classification_compares_finding_keys() {
        let k = key::of_verdict("HYDO-ORACLE: VIOLATION [a.b] lost=3", &[])
            .unwrap()
            .2;
        let r = |v: &str| Replay {
            keys: key::of_verdict(v, &[])
                .map(|x| vec![x.2])
                .unwrap_or_default(),
            verdict: Some(v.into()),
            fp: Some("f".into()),
            why: None,
        };
        assert_eq!(
            classify(&r("HYDO-ORACLE: VIOLATION [a.b] lost=7"), Some(&k)).0,
            vo::REPRODUCED
        );
        assert_eq!(classify(&r("HYDO-ORACLE: OK held"), Some(&k)).0, vo::PASSES);
        assert_eq!(
            classify(&r("HYDO-ORACLE: VIOLATION [c.d] x"), Some(&k)).0,
            vo::FAILS_OTHERWISE
        );
        let none = Replay {
            why: Some("refused".into()),
            ..Replay::default()
        };
        assert_eq!(
            classify(&none, Some(&k)),
            (vo::CANNOT_RUN, Some("refused".into()))
        );
    }
}
