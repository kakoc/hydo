//! `hydo-bench rebaseline` (Milestone 164): every recorded example run of one platform, checked
//! or re-recorded with one command. On the machine it runs on, for every image of the list
//! (`hydo/onboard/recorded.json`, [`crate::recorded`]) of that machine's platform:
//!
//! 1. stage its inputs (`hydo/onboard/rebaseline/stage.sh prepare <stage>`, into the work
//!    folder) and build it twice from the tree (`stage.sh build`, which runs the prefix's
//!    `hydo build`; the prefix is this tree's programs over a release's guest files,
//!    `stage.sh prefix`); every output file of the two builds must be byte-identical;
//! 2. run every recorded schedule twice, each from a fresh boot (`hydo run`), at most `--jobs`
//!    VMs at a time and a `big` image's runs alone; a run is cached in the work folder under the
//!    image's sha256, the schedule, the repetition and the programs and module that ran it, so an
//!    interrupted rebaseline resumes where it stopped;
//! 3. write a report (`<work>/report.md`, also printed): per run the recorded and the new values,
//!    and first of all every verdict change, every run whose two repetitions differ and every
//!    build that is not byte-reproducible.
//!
//! `--check` (the default) writes nothing into the list; `--write` writes the new image hashes,
//! fingerprints and verdict lines into it (a run whose verdict changed only with
//! `--accept-verdicts`). Exit status: 0 everything as recorded (or, with `--write`, written);
//! 1 a verdict change, a nondeterministic run or build, or a step that failed; 2 a usage error;
//! 3 only values moved and `--check` did not write them.
//!
//! On a shared machine: every VM goes through the prefix's `libexec/hvx-run` (the shared module
//! lock, `~/bin/hvx-run` on the bare-metal machine); no VM starts while `HYDO_QUIET_FILE` exists
//! (at most 6 hours, then the run fails); on Linux a run waits until the machine has
//! `MemAvailable` for it (6 GiB for a big image, 3 GiB otherwise).

use crate::recorded::{self, Image};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The options (`hydo-bench rebaseline --help` in [`crate::cli::USAGE`]).
#[derive(Clone, Debug)]
pub struct Opts {
    pub root: PathBuf,
    pub list: PathBuf,
    pub work: PathBuf,
    pub inputs: PathBuf,
    pub release: Option<PathBuf>,
    pub platform: String,
    pub only: Vec<String>,
    pub write: bool,
    pub accept_verdicts: bool,
    pub jobs: usize,
    pub changed_only: bool,
    pub reuse_builds: bool,
    pub no_build: bool,
    /// Build and compare the images only (no runs).
    pub build_only: bool,
    pub timeout: u64,
    pub quiet_file: Option<PathBuf>,
    pub extra_env: Vec<(String, String)>,
}

/// The platform this machine runs by default: `hvm` on a Mac, `hvx` elsewhere.
pub fn default_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "hvm"
    } else {
        "hvx"
    }
}

/// What one build of an image gave.
#[derive(Clone, Debug, Default)]
struct Built {
    /// The image file (the disk image, or on the Mac the initramfs).
    image: PathBuf,
    sha256: String,
    /// Every output file of the second build is the first's.
    reproducible: bool,
    /// The files that differ (or why the comparison could not be made).
    differ: Vec<String>,
    error: Option<String>,
    secs: f64,
}

/// What one run gave.
#[derive(Clone, Debug, Default)]
struct Ran {
    verdict: String,
    fp: String,
    ctl: Option<String>,
    wfp: Option<String>,
    secs: f64,
    cached: bool,
    error: Option<String>,
    dir: PathBuf,
}

impl Ran {
    fn to_json(&self) -> Value {
        json!({"verdict": self.verdict, "fp": self.fp, "ctl": self.ctl, "wfp": self.wfp,
               "secs": self.secs})
    }
    fn from_json(v: &Value) -> Option<Ran> {
        Some(Ran {
            verdict: v["verdict"].as_str()?.to_string(),
            fp: v["fp"].as_str()?.to_string(),
            ctl: v["ctl"].as_str().map(str::to_string),
            wfp: v["wfp"].as_str().map(str::to_string),
            secs: v["secs"].as_f64().unwrap_or(0.0),
            cached: true,
            error: None,
            dir: PathBuf::new(),
        })
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha_text(t: &str) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(t.as_bytes()))
}

fn sha_file(p: &Path) -> String {
    hydo_engine::image::sha256_file(p).unwrap_or_default()
}

/// The image's name in file paths (`hvx/redis` -> `hvx-redis`).
fn slug(id: &str) -> String {
    id.replace('/', "-")
}

/// Today's date (UTC) as `YYYY-MM-DD`, for the `recorded` field.
fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86400)
        .unwrap_or(0) as i64;
    // Howard Hinnant's civil_from_days
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

impl Opts {
    fn stage_script(&self) -> PathBuf {
        self.root
            .join("hydo")
            .join("onboard")
            .join("rebaseline")
            .join("stage.sh")
    }
    fn prefix(&self) -> PathBuf {
        self.work.join("prefix")
    }
    fn hydo(&self) -> PathBuf {
        self.prefix().join("bin").join("hydo")
    }
    fn log(&self, name: &str) -> PathBuf {
        self.work.join("logs").join(name)
    }

    /// `stage.sh <args>` with the staging environment, its output into `logs/<log>`.
    fn stage(&self, args: &[String], log: &str) -> Result<(), String> {
        let lp = self.log(log);
        let f = std::fs::File::create(&lp).map_err(|e| format!("{}: {e}", lp.display()))?;
        let f2 = f.try_clone().map_err(|e| e.to_string())?;
        let mut c = Command::new("bash");
        c.arg(self.stage_script())
            .args(args)
            .env("HYDO_ROOT", &self.root)
            .env("HYDO_REBASE_WORK", &self.work)
            .env("HYDO_REBASE_INPUTS", &self.inputs)
            .env("HYDO_REBASE_PLATFORM", &self.platform)
            .stdin(Stdio::null())
            .stdout(f)
            .stderr(f2);
        if let Some(r) = &self.release {
            c.env("HYDO_REBASE_RELEASE", r);
        }
        for (k, v) in &self.extra_env {
            c.env(k, v);
        }
        let st = c.status().map_err(|e| format!("stage.sh: {e}"))?;
        if st.success() {
            Ok(())
        } else {
            let tail = std::fs::read_to_string(&lp).unwrap_or_default();
            let tail: Vec<&str> = tail.lines().rev().take(8).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            Err(format!(
                "stage.sh {} failed ({}; {}):\n    {}",
                args.join(" "),
                st,
                lp.display(),
                tail.join("\n    ")
            ))
        }
    }
}

/// The image file among a build's outputs: `*-disk.img` (x86_64) or `*-arm64.cpio.gz` (Mac).
fn image_in(dir: &Path, platform: &str) -> Option<PathBuf> {
    let suffix = if platform == "hvm" {
        "-arm64.cpio.gz"
    } else {
        "-disk.img"
    };
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(suffix))
        })
        .collect();
    v.sort();
    if v.len() == 1 {
        v.pop()
    } else {
        None
    }
}

/// The regular files of a build's output folder (the image, its disks and build record) with
/// their sha256.
fn outputs(dir: &Path) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_file() && !name.ends_with(".log") && !name.starts_with('.') {
                m.insert(name, sha_file(&p));
            }
        }
    }
    m
}

/// Build one image twice; the first build stays (`img/<slug>/b1`), the second's files are
/// compared and deleted.
fn build(o: &Opts, img: &Image) -> Built {
    let t0 = Instant::now();
    let base = o.work.join("img").join(slug(&img.id));
    let b1 = base.join("b1");
    let done = base.join("b1.done");
    if (o.reuse_builds || o.no_build) && done.is_file() {
        if let Some(i) = image_in(&b1, &img.platform) {
            let r = std::fs::read_to_string(&done).unwrap_or_default();
            let differ: Vec<String> = r
                .lines()
                .filter_map(|l| l.strip_prefix("differs "))
                .map(str::to_string)
                .collect();
            return Built {
                sha256: sha_file(&i),
                image: i,
                reproducible: differ.is_empty(),
                differ,
                error: None,
                secs: 0.0,
            };
        }
    }
    if o.no_build {
        return Built {
            error: Some(format!(
                "--no-build: no finished build in {}",
                base.display()
            )),
            ..Built::default()
        };
    }
    let _ = std::fs::remove_file(&done);
    let dir = o.work.join("tree").join(&img.dir);
    let inputs = o.inputs.display().to_string();
    let args: Vec<String> = img
        .args
        .iter()
        .map(|a| a.replace("{inputs}", &inputs))
        .collect();
    let mut sums = Vec::new();
    for k in ["b1", "b2"] {
        let out = base.join(k);
        let mut a = vec![
            "build".to_string(),
            if img.keep_owners { "keep" } else { "normal" }.to_string(),
            dir.display().to_string(),
            img.manifest.clone(),
            out.display().to_string(),
        ];
        a.extend(args.iter().cloned());
        if let Err(e) = o.stage(&a, &format!("build-{}-{k}.log", slug(&img.id))) {
            return Built {
                error: Some(e),
                secs: t0.elapsed().as_secs_f64(),
                ..Built::default()
            };
        }
        sums.push(outputs(&out));
    }
    let Some(image) = image_in(&b1, &img.platform) else {
        return Built {
            error: Some(format!("no single image in {}", b1.display())),
            secs: t0.elapsed().as_secs_f64(),
            ..Built::default()
        };
    };
    let mut differ = Vec::new();
    for (name, s1) in &sums[0] {
        match sums[1].get(name) {
            Some(s2) if s2 == s1 => {}
            Some(_) => differ.push(name.clone()),
            None => differ.push(format!("{name} (only in the first build)")),
        }
    }
    for name in sums[1].keys() {
        if !sums[0].contains_key(name) {
            differ.push(format!("{name} (only in the second build)"));
        }
    }
    if differ.is_empty() {
        let _ = std::fs::remove_dir_all(base.join("b2"));
    }
    let mut stamp = String::new();
    for d in &differ {
        let _ = writeln!(stamp, "differs {d}");
    }
    let _ = std::fs::write(&done, stamp);
    Built {
        sha256: sha_file(&image),
        image,
        reproducible: differ.is_empty(),
        differ,
        error: None,
        secs: t0.elapsed().as_secs_f64(),
    }
}

/// What ran a run, for the cache key: the prefix's programs and, on Linux, the loaded module.
fn engine_id(o: &Opts) -> String {
    let mut parts = Vec::new();
    for p in ["hydo", "hydo-hvx", "hydo-hvm", "hydo-kvm"] {
        let f = o.prefix().join("bin").join(p);
        if f.is_file() {
            parts.push(format!("{p}={}", sha_file(&f)));
        }
    }
    if let Ok(v) = std::fs::read_to_string("/sys/module/hvx/srcversion") {
        parts.push(format!("hvx.ko={}", v.trim()));
    }
    for (k, v) in &o.extra_env {
        parts.push(format!("{k}={v}"));
    }
    parts.join(" ")
}

fn mem_available_mib() -> Option<u64> {
    let t = std::fs::read_to_string("/proc/meminfo").ok()?;
    let l = t.lines().find(|l| l.starts_with("MemAvailable:"))?;
    l.split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()
        .map(|k| k / 1024)
}

/// Wait while the quiet file exists and (Linux) until the memory is there; false after the
/// limit (6 hours).
fn wait_to_start(o: &Opts, big: bool) -> bool {
    let t0 = Instant::now();
    let limit = Duration::from_secs(6 * 3600);
    let need = if big { 6144 } else { 3072 };
    let mut said = false;
    loop {
        let quiet = o.quiet_file.as_ref().is_some_and(|q| q.exists());
        let low = mem_available_mib().is_some_and(|m| m < need);
        if !quiet && !low {
            return true;
        }
        if t0.elapsed() > limit {
            return false;
        }
        if !said {
            eprintln!(
                "rebaseline: waiting: {}",
                if quiet {
                    "the quiet file exists".to_string()
                } else {
                    format!("less than {need} MiB available")
                }
            );
            said = true;
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

/// One `hydo run` from a fresh boot into `dir`.
fn run_vm(o: &Opts, image: &Path, schedule: &str, dir: &Path) -> Ran {
    let t0 = Instant::now();
    let _ = std::fs::remove_dir_all(dir);
    if let Some(p) = dir.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let lp = dir.with_extension("log");
    let fail = |e: String| Ran {
        error: Some(e),
        secs: t0.elapsed().as_secs_f64(),
        dir: dir.to_path_buf(),
        ..Ran::default()
    };
    let Ok(f) = std::fs::File::create(&lp) else {
        return fail(format!("{}: cannot write", lp.display()));
    };
    let Ok(f2) = f.try_clone() else {
        return fail("log".into());
    };
    let mut c = Command::new(o.hydo());
    c.arg("run");
    if o.platform == "kvm" {
        c.args(["--backend", "kvm"]);
    }
    c.arg(image)
        .arg(schedule)
        .arg("--out")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(f)
        .stderr(f2);
    if let Some(q) = &o.quiet_file {
        c.env("HYDO_QUIET_FILE", q);
    }
    for (k, v) in &o.extra_env {
        c.env(k, v);
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return fail(format!("{}: {e}", o.hydo().display())),
    };
    let deadline = Instant::now() + Duration::from_secs(o.timeout);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(500)),
            Err(e) => return fail(format!("wait: {e}")),
        }
    };
    let Some(status) = status else {
        return fail(format!("no result after {} s (killed)", o.timeout));
    };
    match hydo_results::read_dir(dir) {
        Ok(hydo_results::Results::Run { run, .. }) => Ran {
            verdict: recorded::verdict_line(&run.verdict.text, run.verdict.guest.as_deref()),
            fp: run.fingerprints.fp.clone(),
            ctl: run.fingerprints.ctl.clone(),
            wfp: run.fingerprints.wfp.clone(),
            secs: t0.elapsed().as_secs_f64(),
            cached: false,
            error: None,
            dir: dir.to_path_buf(),
        },
        _ => {
            let t = std::fs::read_to_string(&lp).unwrap_or_default();
            let last = t.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
            fail(format!("hydo run gave no result ({status}): {last}"))
        }
    }
}

struct Job {
    image: usize,
    run: usize,
    rep: usize,
    big: bool,
    key: String,
    path: PathBuf,
    schedule: String,
    dir: PathBuf,
}

/// The per-image outcome the report and `--write` use.
struct Outcome {
    built: Built,
    /// (run index) -> [rep 1, rep 2]
    ran: HashMap<usize, [Option<Ran>; 2]>,
    skipped: bool,
}

/// Run the rebaseline; returns the exit status.
pub fn rebaseline(o: &Opts) -> Result<i32, String> {
    let text =
        std::fs::read_to_string(&o.list).map_err(|e| format!("{}: {e}", o.list.display()))?;
    let mut doc: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", o.list.display()))?;
    let all = recorded::images(&doc).map_err(|e| format!("{}: {e}", o.list.display()))?;
    let selected: Vec<Image> = all
        .into_iter()
        .filter(|i| i.platform == o.platform)
        .filter(|i| {
            o.only.is_empty()
                || o.only.iter().any(|w| {
                    i.id == *w
                        || i.id.split_once('/').map(|x| x.1) == Some(w.as_str())
                        || i.stage == *w
                })
        })
        .collect();
    if selected.is_empty() {
        return Err(format!(
            "no image of the platform {} in the list{}",
            o.platform,
            if o.only.is_empty() {
                String::new()
            } else {
                format!(" matches --only {}", o.only.join(","))
            }
        ));
    }
    for d in ["logs", "img", "tree", "runs", "cache"] {
        std::fs::create_dir_all(o.work.join(d))
            .map_err(|e| format!("{}: {e}", o.work.display()))?;
    }
    std::fs::create_dir_all(&o.inputs).map_err(|e| format!("{}: {e}", o.inputs.display()))?;
    let t_all = Instant::now();
    eprintln!(
        "rebaseline: {} images, {} schedules ({}), work folder {}",
        selected.len(),
        selected.iter().map(|i| i.runs.len()).sum::<usize>(),
        o.platform,
        o.work.display()
    );
    if !o.no_build {
        o.stage(&["prefix".to_string()], "prefix.log")?;
    }
    if !o.hydo().is_file() {
        return Err(format!("no {} (stage.sh prefix)", o.hydo().display()));
    }
    let engine = engine_id(o);

    // builds, one image at a time; an image's stage is prepared again whenever the image before
    // it had another stage (stages may share a staged folder: the stack's two order services
    // are both installed as its front-door), and a stage that failed is not tried again
    let t_build = Instant::now();
    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut failed: HashMap<String, String> = HashMap::new();
    let mut last = String::new();
    for img in &selected {
        let st = if o.no_build {
            Ok(())
        } else if let Some(e) = failed.get(&img.stage) {
            Err(e.clone())
        } else if img.stage == last {
            Ok(())
        } else {
            eprintln!("rebaseline: stage {}", img.stage);
            let r = o.stage(
                &["prepare".to_string(), img.stage.clone()],
                &format!("stage-{}.log", img.stage),
            );
            match &r {
                Ok(()) => last = img.stage.clone(),
                Err(e) => {
                    failed.insert(img.stage.clone(), e.clone());
                    last.clear();
                }
            }
            r
        };
        let built = match st {
            Err(e) => Built {
                error: Some(e),
                ..Built::default()
            },
            Ok(()) => {
                eprintln!("rebaseline: build {} twice", img.id);
                build(o, img)
            }
        };
        if let Some(e) = &built.error {
            eprintln!("rebaseline: {}: {e}", img.id);
        } else {
            eprintln!(
                "rebaseline: {} {} {} ({:.0} s)",
                img.id,
                &built.sha256[..16],
                if built.reproducible {
                    "(two builds identical)"
                } else {
                    "(BUILDS DIFFER)"
                },
                built.secs
            );
        }
        let skipped = o.changed_only
            && built.error.is_none()
            && recorded::same_hex(&img.sha256, &built.sha256);
        outcomes.push(Outcome {
            built,
            ran: HashMap::new(),
            skipped,
        });
    }
    let build_secs = t_build.elapsed().as_secs_f64();

    // runs
    let t_runs = Instant::now();
    let mut queue: VecDeque<Job> = VecDeque::new();
    for (ii, img) in selected.iter().enumerate() {
        let oc = &mut outcomes[ii];
        if oc.built.error.is_some() || oc.skipped || o.build_only {
            continue;
        }
        for (ri, r) in img.runs.iter().enumerate() {
            for rep in 0..2 {
                let key = sha_text(&format!(
                    "{}\n{}\n{}\n{}\n{}",
                    o.platform, oc.built.sha256, r.schedule, rep, engine
                ));
                let path = o.work.join("cache").join(format!("{key}.json"));
                if let Some(c) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|v| Ran::from_json(&v))
                {
                    oc.ran.entry(ri).or_insert([None, None])[rep] = Some(c);
                    continue;
                }
                queue.push_back(Job {
                    image: ii,
                    run: ri,
                    rep,
                    big: img.big,
                    key,
                    path,
                    schedule: r.schedule.clone(),
                    dir: o
                        .work
                        .join("runs")
                        .join(slug(&img.id))
                        .join(format!("r{ri}-{}", rep + 1)),
                });
            }
        }
    }
    let total = queue.len();
    eprintln!(
        "rebaseline: {total} runs to make ({} jobs at a time)",
        o.jobs
    );
    let (tx, rx) = mpsc::channel::<(Job, Ran)>();
    let mut running = 0usize;
    let mut big_running = false;
    let mut done = 0usize;
    loop {
        while let Some(j) = queue.front() {
            let can = running == 0 || (!big_running && !j.big && running < o.jobs.max(1));
            if !can {
                break;
            }
            if !wait_to_start(o, j.big) {
                return Err("waited 6 hours for the quiet file to go or for memory".into());
            }
            let j = queue.pop_front().expect("a job");
            running += 1;
            big_running |= j.big;
            let tx = tx.clone();
            let oo = o.clone();
            let image = outcomes[j.image].built.image.clone();
            std::thread::spawn(move || {
                let r = run_vm(&oo, &image, &j.schedule, &j.dir);
                let _ = tx.send((j, r));
            });
        }
        if running == 0 {
            break;
        }
        let (j, r) = rx.recv().map_err(|e| e.to_string())?;
        running -= 1;
        if j.big {
            big_running = false;
        }
        done += 1;
        let img = &selected[j.image];
        eprintln!(
            "rebaseline: [{done}/{total}] {} `{}` #{} {} {} ({:.0} s)",
            img.id,
            j.schedule,
            j.rep + 1,
            if r.error.is_some() { "FAILED" } else { &r.fp },
            r.error
                .as_deref()
                .unwrap_or(&r.verdict)
                .chars()
                .take(70)
                .collect::<String>(),
            r.secs
        );
        if r.error.is_none() {
            let _ = std::fs::write(&j.path, r.to_json().to_string());
            let _ = j.key;
        }
        outcomes[j.image].ran.entry(j.run).or_insert([None, None])[j.rep] = Some(r);
    }
    let run_secs = t_runs.elapsed().as_secs_f64();

    // the report, and the new values
    let mut rep = Report::default();
    let mut changed_images = 0;
    for (ii, img) in selected.iter().enumerate() {
        let oc = &outcomes[ii];
        let b = &oc.built;
        if let Some(e) = &b.error {
            rep.failures.push(format!("{}: {e}", img.id));
            continue;
        }
        if !b.reproducible {
            rep.unreproducible
                .push(format!("{}: {}", img.id, b.differ.join(", ")));
        }
        let image_moved = !recorded::same_hex(&img.sha256, &b.sha256);
        if image_moved {
            rep.new_images.push(format!(
                "| `{}` | `{}` | `{}` |",
                img.id,
                if img.sha256.is_empty() {
                    "-"
                } else {
                    &img.sha256
                },
                &b.sha256[..16]
            ));
        }
        if oc.skipped {
            rep.skipped.push(img.id.clone());
            continue;
        }
        if o.build_only {
            continue;
        }
        let mut wrote_any = false;
        for (ri, r) in img.runs.iter().enumerate() {
            let pair = oc.ran.get(&ri);
            let a = pair.and_then(|p| p[0].clone());
            let c = pair.and_then(|p| p[1].clone());
            let (Some(a), Some(c)) = (a, c) else {
                rep.failures
                    .push(format!("{} `{}`: not run", img.id, r.schedule));
                continue;
            };
            for x in [&a, &c] {
                if let Some(e) = &x.error {
                    rep.failures.push(format!(
                        "{} `{}`: {e} ({})",
                        img.id,
                        r.schedule,
                        x.dir.display()
                    ));
                }
            }
            if a.error.is_some() || c.error.is_some() {
                continue;
            }
            rep.runs += 1;
            rep.cached += [&a, &c].iter().filter(|x| x.cached).count();
            let twice = a.fp == c.fp && a.ctl == c.ctl && a.verdict == c.verdict;
            if !twice {
                rep.nondeterministic
                    .push(format!("{} `{}`: {} / {}", img.id, r.schedule, a.fp, c.fp));
            }
            let verdict_same = recorded::same_verdict(&r.verdict, &a.verdict);
            if !verdict_same {
                rep.verdicts.push(format!(
                    "{} `{}`:\n  - recorded: {}\n  - now: {}",
                    img.id, r.schedule, r.verdict, a.verdict
                ));
            } else if !recorded::same_text(&r.verdict, &a.verdict) {
                rep.texts.push(format!(
                    "{} `{}`:\n  - recorded: {}\n  - now: {}",
                    img.id, r.schedule, r.verdict, a.verdict
                ));
            }
            let fp_same = r.fp == a.fp;
            let ctl_same = r.ctl.as_ref().is_none_or(|x| a.ctl.as_ref() == Some(x));
            let wfp_same = r.wfp.as_ref().is_none_or(|x| a.wfp.as_ref() == Some(x));
            let moved = !(fp_same && ctl_same && wfp_same);
            if moved {
                rep.moved += 1;
            }
            let _ = writeln!(
                rep.table,
                "| `{}` | `{}` | {} | `{}` | `{}`{} | {}{} |",
                img.id,
                r.schedule.replace('|', "\\|"),
                a.verdict
                    .chars()
                    .take(60)
                    .collect::<String>()
                    .replace('|', "\\|"),
                r.fp,
                a.fp,
                match (&r.ctl, &a.ctl) {
                    (Some(x), Some(y)) if x != y => format!(" (ctl `{x}` -> `{y}`)"),
                    _ => String::new(),
                },
                if twice { "yes" } else { "NO" },
                if verdict_same { "" } else { ", VERDICT" }
            );
            // --write
            let writable = twice && (verdict_same || o.accept_verdicts);
            if o.write && writable {
                let rv = &mut doc["images"][img.index]["runs"][ri];
                if rv["fp"] != json!(a.fp)
                    || rv["verdict"] != json!(a.verdict)
                    || a.ctl.as_ref().is_some_and(|x| rv["ctl"] != json!(x))
                    || (rv.get("wfp").is_some() && rv["wfp"] != json!(a.wfp))
                {
                    wrote_any = true;
                }
                rv["verdict"] = json!(a.verdict);
                rv["fp"] = json!(a.fp);
                if let Some(x) = &a.ctl {
                    rv["ctl"] = json!(x);
                }
                if rv.get("wfp").is_some() {
                    rv["wfp"] = json!(a.wfp);
                }
            } else if o.write && moved {
                rep.not_written += 1;
            }
        }
        if o.write && b.reproducible {
            let iv = &mut doc["images"][img.index];
            if iv["sha256"] != json!(b.sha256) {
                wrote_any = true;
                iv["sha256"] = json!(b.sha256);
            }
            if wrote_any {
                iv["recorded"] = json!(format!("hydo-bench rebaseline --write, {}", today()));
                changed_images += 1;
            }
        }
    }
    let bad = !rep.failures.is_empty()
        || !rep.unreproducible.is_empty()
        || !rep.nondeterministic.is_empty()
        || !rep.verdicts.is_empty();
    let moved_any = rep.moved > 0 || !rep.new_images.is_empty();
    if o.write && changed_images > 0 {
        std::fs::write(&o.list, recorded::to_text(&doc))
            .map_err(|e| format!("{}: {e}", o.list.display()))?;
    }
    let text = rep.text(
        o,
        &selected,
        &engine,
        [build_secs, run_secs, t_all.elapsed().as_secs_f64()],
        changed_images,
    );
    let rp = o.work.join("report.md");
    std::fs::write(&rp, &text).map_err(|e| format!("{}: {e}", rp.display()))?;
    print!("{text}");
    println!("rebaseline: report {}", rp.display());
    Ok(if bad {
        1
    } else if moved_any && (!o.write || rep.not_written > 0) {
        3
    } else {
        0
    })
}

#[derive(Default)]
struct Report {
    failures: Vec<String>,
    unreproducible: Vec<String>,
    nondeterministic: Vec<String>,
    verdicts: Vec<String>,
    texts: Vec<String>,
    new_images: Vec<String>,
    skipped: Vec<String>,
    table: String,
    runs: usize,
    cached: usize,
    moved: usize,
    not_written: usize,
}

impl Report {
    fn text(
        &self,
        o: &Opts,
        images: &[Image],
        engine: &str,
        secs: [f64; 3],
        written: usize,
    ) -> String {
        let mut t = String::new();
        let _ = writeln!(t, "# hydo-bench rebaseline: {} ({})\n", o.platform, today());
        let _ = writeln!(
            t,
            "List `{}`, {} images, {} schedules; tree `{}`; work folder `{}`.  ",
            o.list.display(),
            images.len(),
            images.iter().map(|i| i.runs.len()).sum::<usize>(),
            o.root.display(),
            o.work.display()
        );
        let _ = writeln!(t, "Programs: {engine}.  ");
        let _ = writeln!(
            t,
            "Time: builds {:.0} s, runs {:.0} s, all {:.0} s; {} schedules twice ({} of the {} runs from the cache).\n",
            secs[0],
            secs[1],
            secs[2],
            self.runs,
            self.cached,
            2 * self.runs
        );
        let _ = writeln!(t, "## Summary\n");
        let line = |n: usize, none: &str, some: &str| {
            if n == 0 {
                none.to_string()
            } else {
                format!("{n} {some}")
            }
        };
        let _ = writeln!(
            t,
            "- Verdicts: {}.",
            line(self.verdicts.len(), "unchanged", "CHANGED (below)")
        );
        let _ = writeln!(
            t,
            "- Determinism: {}.",
            line(
                self.nondeterministic.len(),
                "every run twice identical",
                "runs whose two repetitions DIFFER (below)"
            )
        );
        let _ = writeln!(
            t,
            "- Builds: {}.",
            line(
                self.unreproducible.len(),
                "every image built twice with the same bytes",
                "images NOT byte-reproducible (below)"
            )
        );
        let _ = writeln!(
            t,
            "- Failures: {}.",
            line(self.failures.len(), "none", "steps FAILED (below)")
        );
        let _ = writeln!(
            t,
            "- Values: {} images with new bytes, {} runs with a new fingerprint or ctl value{}.",
            self.new_images.len(),
            self.moved,
            if o.write {
                format!(
                    "; written into the list for {written} images{}",
                    if self.not_written > 0 {
                        format!(
                            " ({} runs not written: a verdict change or a nondeterminism)",
                            self.not_written
                        )
                    } else {
                        String::new()
                    }
                )
            } else if self.moved > 0 || !self.new_images.is_empty() {
                " (--check: not written)".to_string()
            } else {
                String::new()
            }
        );
        if !self.skipped.is_empty() {
            let _ = writeln!(
                t,
                "- Not run (--changed-only, the image's bytes are the recorded ones): {}.",
                self.skipped.join(", ")
            );
        }
        let sec = |t: &mut String, h: &str, v: &[String]| {
            if !v.is_empty() {
                let _ = writeln!(t, "\n## {h}\n");
                for x in v {
                    let _ = writeln!(t, "- {x}");
                }
            }
        };
        sec(&mut t, "Verdict changes", &self.verdicts);
        sec(
            &mut t,
            "Runs whose two repetitions differ",
            &self.nondeterministic,
        );
        sec(
            &mut t,
            "Builds that are not byte-reproducible",
            &self.unreproducible,
        );
        sec(&mut t, "Failures", &self.failures);
        sec(
            &mut t,
            "Verdict lines that differ in other words (same class and property)",
            &self.texts,
        );
        if !self.new_images.is_empty() {
            let _ = writeln!(t, "\n## Images with new bytes\n");
            let _ = writeln!(t, "| image | recorded sha256 | now |\n|---|---|---|");
            for x in &self.new_images {
                let _ = writeln!(t, "{x}");
            }
        }
        if !self.table.is_empty() {
            let _ = writeln!(t, "\n## Runs\n");
            let _ = writeln!(
                t,
                "| image | schedule | verdict now | recorded fp | new fp | twice |\n|---|---|---|---|---|---|"
            );
            t.push_str(&self.table);
        }
        let _ = writeln!(
            t,
            "\n{}",
            if self.verdicts.is_empty() && self.failures.is_empty() {
                "verdicts unchanged"
            } else {
                "VERDICTS CHANGED OR RUNS FAILED"
            }
        );
        t
    }
}
