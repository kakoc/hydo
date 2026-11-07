//! `hydo run`, `hydo repro`, `hydo repro --regressions`, `hydo save`: runs from a fresh boot
//! through the engine interface (hydo-engine: hvx on Linux, hvm on the Mac), the history checks
//! (hydo-history), the error logs of a run (hydo-errors), saved regressions (hydo-composer), and
//! the files a run leaves (run.json, engine.log, the consoles). One behaviour on both platforms
//! since M107; only the engine and the image's file name differ.

use crate::ctx::{
    abspath, die, glob_simple, pjoin, read_text, strftime, take, Ctx, Platform, OUT, PROJECT_OUT,
};
use crate::hvxenv;
use crate::pyjson::{head, py_str, tail};
use hydo_control::agent::AgentEngine;
use hydo_control::drive::Policy;
use hydo_engine::hvm::Hvm;
use hydo_engine::hvx::Hvx;
use hydo_engine::image::{boot_files, embedded_compose, embedded_manifest, sha256_file};
use hydo_engine::protocol::verdict_kind;
use hydo_engine::{BootSpec, ControlledEngine, CpuLevel, RunResult};
use hydo_manifest::Guest;
use hydo_schedule::Schedule;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------- images and engine options

/// The image file names of this platform: (what `hydo build` writes, a positional image's suffix).
fn image_pattern(ctx: &Ctx) -> (&'static str, &'static str) {
    match ctx.platform {
        Platform::Linux => ("-disk.img", ".img"),
        Platform::Mac => ("-arm64.cpio.gz", ".cpio.gz"),
    }
}

/// `--image IMG`, or a leading positional image, or the one image `hydo build` wrote in ./hydo-out
/// (then ./.hydo, where a project's hydo/hydo.yaml builds).
pub fn find_image(ctx: &Ctx, args: &mut Vec<String>) -> PathBuf {
    let (built, suffix) = image_pattern(ctx);
    let mut img = take(args, "--image");
    if img.is_none() && args.first().is_some_and(|a| a.ends_with(suffix)) {
        img = Some(args.remove(0));
    }
    let img = match img {
        Some(i) => i,
        None => {
            let mut imgs = glob_simple(Path::new(OUT), "", built);
            if imgs.is_empty() {
                imgs = glob_simple(Path::new(PROJECT_OUT), "", built);
            }
            if imgs.len() != 1 {
                die(&format!(
                    "no image given and {} *{} in ./{} or ./{} (run `hydo build hydo.yaml` here, or pass --image)",
                    imgs.len(),
                    built,
                    OUT,
                    PROJECT_OUT
                ));
            }
            imgs[0].display().to_string()
        }
    };
    if !Path::new(&img).is_file() {
        die(&format!("no such image: {img}"));
    }
    abspath(Path::new(&img))
}

/// The `guest:` section of the manifest an image carries (defaults when it cannot be read: the
/// Python CLIs fell back the same way when the image had no manifest).
pub fn image_guest(image: &Path) -> Guest {
    let Ok(text) = embedded_manifest(image) else {
        return Guest::from_value(&Value::Null);
    };
    // manifest_space.load(image_path=...): the manifest with the plan meta the image carries,
    // expanded and validated as hydo-build did when it built the image
    let meta: Option<Value> = embedded_compose(image).and_then(|t| serde_json::from_str(&t).ok());
    match hydo_manifest::from_embedded(&text, meta.as_ref()) {
        Ok(m) => Guest::from_value(&m.data),
        Err(_) => Guest::from_value(&Value::Null),
    }
}

/// The engine options the explorer passes for this image (explore.py's engine_args_for).
pub fn engine_args(image: &Path, ram: Option<u64>, cpu: Option<String>, vcpus: u64) -> Vec<String> {
    let (ram, cpu) = match (ram, cpu) {
        (Some(r), Some(c)) => (r, c),
        (r, c) => {
            let g = image_guest(image);
            (r.unwrap_or(g.ram_mib as u64), c.unwrap_or(g.cpu))
        }
    };
    let mut a = Vec::new();
    if ram != 1024 {
        a.push("--ram-mib".to_string());
        a.push(ram.to_string());
    }
    if vcpus > 1 {
        a.push("--vcpus".to_string());
        a.push(vcpus.to_string());
    }
    if cpu == "v3" {
        a.push("--cpu".to_string());
        a.push("v3".to_string());
    }
    a
}

/// Engine options (as argv words) -> a BootSpec.
fn spec_from_args(
    eargs: &[String],
    image: &Path,
    kernel: PathBuf,
    initrd: Option<PathBuf>,
    outdir: &Path,
) -> BootSpec {
    let mut spec = BootSpec {
        image: image.to_path_buf(),
        ram_mib: 1024,
        vcpus: 1,
        cpu: CpuLevel::V2,
        kernel,
        initrd,
        outdir: outdir.to_path_buf(),
        snapshots: hydo_engine::SnapshotStore::Files,
    };
    let mut i = 0;
    while i + 1 < eargs.len() {
        match eargs[i].as_str() {
            "--ram-mib" => spec.ram_mib = eargs[i + 1].parse().unwrap_or(1024),
            "--vcpus" => spec.vcpus = eargs[i + 1].parse().unwrap_or(1),
            "--cpu" => spec.cpu = CpuLevel::parse(&eargs[i + 1]).unwrap_or_default(),
            _ => {}
        }
        i += 2;
    }
    spec
}

/// Parse a schedule at the engine seam. A string the typed parser refuses is not run.
fn schedule(text: &str) -> Schedule {
    Schedule::parse(text).unwrap_or_else(|e| die(&format!("bad schedule {text:?}: {e}")))
}

// ---------------------------------------------------------------- one run

/// The Mac's default guest kernel.
pub fn mac_kernel(ctx: &Ctx) -> PathBuf {
    ctx.p("hv/hvm/explore/out/Image-explore")
}

/// The kernel for an image: the one its record names (Milestone 155, by sha256 among this hydo's
/// kernels), else the one `/etc/hydo/kernel` names (M99), else the default; a refusal ends the
/// command with `hydo: `.
pub fn mac_kernel_for(ctx: &Ctx, image: &Path, quiet: bool) -> PathBuf {
    let b = boot_files(image, &mac_kernel(ctx), None, None, &[ctx.guest()])
        .unwrap_or_else(|msg| die(&msg));
    if let Some(n) = &b.note {
        eprintln!("hydo: {n}");
    }
    let k = b.kernel;
    if !quiet && k != mac_kernel(ctx) {
        let rel = k.strip_prefix(&ctx.root).unwrap_or(&k);
        println!(
            "hydo: guest kernel {} (recorded in the image)",
            rel.display()
        );
    }
    k
}

/// One schedule from a fresh boot on this platform's engine. Writes `engine.log` (everything the
/// engine printed) into outdir. Returns the RESULT, or None (the engine's output is returned too).
/// Every image is an agent image (`/etc/hydo/control`, M126; a shell-init image is refused since
/// M145) and runs under the host controller: `policy` is the schedule's, or recorded transcripts
/// typed again (`hydo repro --transcript`).
fn run_engine(
    ctx: &Ctx,
    image: &Path,
    sched: &str,
    outdir: &Path,
    eargs: &[String],
    quiet: bool,
    policy: &Policy,
) -> (Option<RunResult>, String) {
    let _ = std::fs::create_dir_all(outdir);
    let s = schedule(sched);
    // the engine and the boot spec of this platform, then the run under the host controller
    let one = |eng: Arc<dyn ControlledEngine + Send + Sync>, spec: &BootSpec| {
        let a = AgentEngine::for_image(eng, image).unwrap_or_else(|e| die(&format!("hydo: {e}")));
        if !quiet {
            println!(
                "hydo: an agent image: the host controller drives the run (/etc/hydo/control)"
            );
        }
        a.with_control(&ctx.config.control)
            .one_with(spec, &s, policy)
    };
    let run = match ctx.platform {
        Platform::Linux if ctx.kvm => {
            // the KVM build of the image's recorded kernel, and the image's initramfs
            let dirs = [ctx.guest(), ctx.p("build"), ctx.p("hv/kvm/linux/out")];
            let b = hydo_engine::kvm::boot_files(image, &dirs, ctx.config.kvm.kernel.as_deref())
                .unwrap_or_else(|msg| die(&msg));
            if let Some(n) = &b.note {
                eprintln!("hydo: {n}");
            }
            let spec = spec_from_args(eargs, image, b.kernel, Some(b.initrd), &abspath(outdir));
            if spec.vcpus != 1 || spec.cpu != CpuLevel::V2 {
                die("the KVM engine runs one vCPU at the v2 CPU level (--vcpus and guest.cpu: v3 need hvx)");
            }
            let k = hydo_engine::kvm::Kvm::for_release(&ctx.root, ctx.config.env.clone());
            one(Arc::new(k), &spec)
        }
        Platform::Linux => {
            let hvx = Hvx {
                engine: hvxenv::engine(ctx),
                hvx_run: Some(ctx.libexec().join("hvx-run")),
                extra_env: Vec::new(),
                env: hvxenv::tool_env(ctx),
                config: ctx.config.env.clone(),
            };
            if let Some(p) = hvx.missing_program() {
                die(&format!(
                    "{} is not there (reinstall the release, or build it: cargo build --release -p hydo-hvx)",
                    p.display()
                ));
            }
            // the kernel and initramfs the image's record names (Milestone 155)
            let b = boot_files(
                image,
                &hvxenv::bzimage(ctx),
                Some(&hvxenv::initrd(ctx)),
                None,
                &[ctx.p("build")],
            )
            .unwrap_or_else(|msg| die(&msg));
            if let Some(n) = &b.note {
                eprintln!("hydo: {n}");
            }
            let spec = spec_from_args(eargs, image, b.kernel, b.initrd, &abspath(outdir));
            one(Arc::new(hvx), &spec)
        }
        Platform::Mac => {
            let kernel = mac_kernel_for(ctx, image, quiet);
            let spec = spec_from_args(eargs, image, kernel, None, outdir);
            one(Arc::new(Hvm::for_release(&ctx.root, ctx.config.env.clone())), &spec)
        }
    }
    .unwrap_or_else(|e| die(&format!("cannot start the engine: {e}")));
    let _ = std::fs::write(outdir.join("engine.log"), &run.output);
    if !quiet {
        // the spin observer's notes (hvm: a run ended at a counter loop and ran again)
        for line in run.output.lines().filter(|l| l.starts_with("hvmx: one")) {
            println!("{line}");
        }
        if let Some(r) = &run.result {
            println!("{}", r.line);
        }
    }
    (run.result, run.output)
}

/// One schedule from a fresh boot; dies without a RESULT (the engine's output tail on stderr).
pub fn run_one(
    ctx: &Ctx,
    image: &Path,
    sched: &str,
    outdir: &Path,
    eargs: &[String],
    quiet: bool,
) -> Map<String, Value> {
    run_one_with(ctx, image, sched, outdir, eargs, quiet, &Policy::Schedule)
}

/// [`run_one`] under `policy`.
fn run_one_with(
    ctx: &Ctx,
    image: &Path,
    sched: &str,
    outdir: &Path,
    eargs: &[String],
    quiet: bool,
    policy: &Policy,
) -> Map<String, Value> {
    match run_engine(ctx, image, sched, outdir, eargs, quiet, policy) {
        (Some(r), _) => result_map(&r),
        (None, output) => {
            eprint!("{}", tail(&output, 3000));
            die(&format!(
                "the run produced no RESULT line (output above; full log in {})",
                outdir.join("engine.log").display()
            ))
        }
    }
}

fn result_map(r: &RunResult) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("fp".into(), json!(r.fp));
    m.insert("wfp".into(), json!(r.wfp));
    m.insert("secs".into(), json!(r.secs));
    m.insert("verdict".into(), json!(r.verdict));
    m.insert("line".into(), json!(r.line));
    if let Some(c) = r.extra("ctl") {
        m.insert("ctl".into(), json!(c));
    }
    m
}

/// The consoles of a run (one.log, one.recovery.log) as (label, text).
fn console_sources(outdir: &Path) -> Vec<(String, String)> {
    let mut srcs = Vec::new();
    for suffix in ["", ".recovery"] {
        let p = outdir.join(format!("one{suffix}.log"));
        if p.exists() {
            srcs.push((format!("one{suffix}"), read_text(&p).unwrap_or_default()));
        }
    }
    srcs
}

/// The history checks of a run's consoles (hydo-history). An input the checkers cannot survive
/// stops the command with status 1, as the Python checker's exception did.
fn history_checks(srcs: &[(String, String)], verdict: Option<&str>) -> hydo_history::Checks {
    match hydo_history::run_checks(srcs, verdict, hydo_history::DEFAULT_BUDGET, None) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("hydo: the history checks stopped: {e}");
            std::process::exit(1)
        }
    }
}

/// The combined verdict as the Python CLI printed it ("None" without checks).
fn verdict_text(r: &hydo_history::Checks) -> String {
    r.verdict.clone().unwrap_or_else(|| "None".into())
}

/// History checks on the host (hydo/HISTORIES.md): keeps the checks' findings and records in res
/// (result.json reports them) and folds the combined verdict into res (the guest's own as
/// guest_verdict).
fn host_checks(outdir: &Path, mut res: Map<String, Value>, quiet: bool) -> Map<String, Value> {
    let srcs = console_sources(outdir);
    if !srcs.iter().any(|(_, t)| t.contains("HYDO-CHECK ")) {
        return res;
    }
    let verdict = res
        .get("verdict")
        .and_then(Value::as_str)
        .map(str::to_string);
    let r = history_checks(&srcs, verdict.as_deref());
    hydo_explore::results::keep_checks(&mut res, &r);
    let gv = res.get("verdict").cloned().unwrap_or(Value::Null);
    res.insert("guest_verdict".into(), gv);
    res.insert("verdict".into(), json!(r.verdict));
    if !quiet {
        println!(
            "hydo: history checks: {}   (explanations in {})",
            verdict_text(&r),
            outdir.join(hydo_results::RUN_DOC).display()
        );
    }
    res
}

/// The known-errors list a run's error kinds were compared with (result.json `error_logs`).
const ERRLOG_KNOWN: &str = "_errlog_known";

/// Error logs as findings for one run (the hydo-errors crate): prints the kinds, keeps the
/// analysis in res (result.json reports it), folds the verdict into res.
fn errors_one(
    outdir: &Path,
    sched: &str,
    res: Map<String, Value>,
    known: Option<&str>,
) -> Map<String, Value> {
    let (mut res, an) =
        hydo_errors::for_run_dir(outdir, sched, res, known, &mut |l| println!("{l}"));
    if let Some((an, k)) = an {
        res.insert(hydo_explore::results::ERRLOG.into(), an.to_json());
        res.insert(
            ERRLOG_KNOWN.into(),
            json!({"known_file": k.path, "known_sha256": k.sha256, "unknown": k.unknown}),
        );
    }
    res
}

/// One run's result.json (the results format): the run, its image and engine, its findings and
/// property table, its files.
fn run_document(
    outdir: &Path,
    image: &Path,
    image_sha: Option<String>,
    sched: &str,
    eargs: &[String],
    res: &Map<String, Value>,
    backend: &str,
) -> hydo_results::Run {
    let mut consoles = Vec::new();
    for suffix in ["", ".recovery"] {
        let rel = format!("one{suffix}.log");
        if let Ok(t) = read_text(&outdir.join(&rel)) {
            consoles.push((rel, t));
        }
    }
    let mut run = hydo_explore::results::run_of("one", sched, res, &consoles);
    run.tag = None;
    run.hydo_version = Some(env!("CARGO_PKG_VERSION").into());
    run.epoch = hydo_explore::results::this_epoch();
    let p = image.display().to_string();
    run.image = Some(hydo_explore::results::image_of(&p, image, image_sha));
    let mut engine = hydo_results::Engine {
        backend: Some(backend.into()),
        args: Some(eargs.to_vec()),
        ..hydo_results::Engine::default()
    };
    let mut i = 0;
    while i + 1 < eargs.len() {
        match eargs[i].as_str() {
            "--ram-mib" => engine.ram_mib = eargs[i + 1].parse().ok(),
            "--vcpus" => engine.vcpus = eargs[i + 1].parse().ok(),
            "--cpu" => engine.cpu = Some(eargs[i + 1].clone()),
            _ => {}
        }
        i += 2;
    }
    if backend == "hvx" {
        engine.module_srcversion = std::fs::read_to_string("/sys/module/hvx/srcversion")
            .ok()
            .map(|s| s.trim().to_string());
    }
    run.engine = Some(engine);
    let texts: Vec<String> = consoles.iter().map(|c| c.1.clone()).collect();
    run.properties =
        hydo_explore::results::property_table(&[hydo_explore::results::merged_text(&texts, res)]);
    if let (Some(el), Some(k)) = (run.error_logs.as_mut(), res.get(ERRLOG_KNOWN)) {
        let g = |f: &str| k.get(f).and_then(Value::as_str).map(str::to_string);
        el.known_file = g("known_file");
        el.known_sha256 = g("known_sha256");
        el.unknown = g("unknown");
    }
    run.result_line = res.get("line").and_then(Value::as_str).map(str::to_string);
    // an agent image's run: the control transcripts of its conversations (M126)
    run.control = hydo_control::agent::controls_in(outdir, "", "");
    run.artifacts
        .get_or_insert_with(Default::default)
        .engine_log = Some("engine.log".into());
    run
}

fn write_run_document(outdir: &Path, run: &hydo_results::Run) {
    let _ = hydo_results::write_document(&outdir.join(hydo_results::RUN_DOC), run);
}

fn new_run_dir() -> PathBuf {
    let d = Path::new(OUT).join("runs").join(format!(
        "run-{}-{}",
        strftime("%Y%m%d-%H%M%S"),
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn cmd_run(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    let outdir = take(&mut args, "--out");
    let known = take(&mut args, "--known-errors");
    let image = find_image(ctx, &mut args);
    if args.len() != 1 {
        die("usage: hydo run [IMG] \"<schedule>\" [--image IMG] [--out DIR] [--known-errors FILE]   e.g. hydo run \"disk=cut | run C=12 R=everysec\"");
    }
    let sched = args[0].clone();
    let outdir = outdir.map(PathBuf::from).unwrap_or_else(new_run_dir);
    let eargs = engine_args(&image, None, None, 1);
    if let Ok(Some(r)) = hydo_engine::image::image_record(&image) {
        if r.epoch != hydo_image::EPOCH {
            println!(
                "hydo: note: {} was built at fingerprint epoch {} and this hydo is at epoch {}: \
                 fingerprints recorded with the hydo that built it are not comparable with this \
                 run's (its verdicts are)",
                image.display(),
                r.epoch,
                hydo_image::EPOCH
            );
        }
    }
    let res = run_one(ctx, &image, &sched, &outdir, &eargs, false);
    let res = host_checks(&outdir, res, false);
    let res = errors_one(&outdir, &sched, res, known.as_deref());
    let sha = sha256_file(&image).unwrap_or_else(|e| die(&format!("{}: {e}", image.display())));
    let run = run_document(
        &outdir,
        &image,
        Some(sha),
        &sched,
        &eargs,
        &res,
        ctx.backend_name(),
    );
    write_run_document(&outdir, &run);
    println!(
        "hydo: run directory {}   (hydo repro {}  runs it again and compares fp=)",
        outdir.display(),
        outdir.display()
    );
    0
}

struct ReproItem {
    label: String,
    image: PathBuf,
    sched: String,
    fp: String,
    eargs: Vec<String>,
    /// The fingerprint epoch the run was recorded at (None: before M155, epoch 1).
    epoch: Option<u64>,
    /// The recorded verdict (compared when the epochs or the engines differ).
    verdict: String,
    /// The engine that recorded it (`hvx`, `hvm`, `kvm`; None: not recorded).
    backend: Option<String>,
}

/// The recorded engine when it is not the one this hydo runs (each engine has its own
/// fingerprints, so only verdicts compare between them).
fn other_engine<'a>(recorded: Option<&'a str>, now: &str) -> Option<&'a str> {
    recorded.filter(|b| !b.is_empty() && *b != now)
}

/// The plain sentence for a run recorded by another engine, once.
fn engine_note(recorded: Option<&str>, now: &str) -> Option<String> {
    other_engine(recorded, now).map(|b| {
        format!(
            "hydo: the recorded run is from the {b} engine and this hydo runs the {now} engine: \
             fingerprints of different engines are not comparable, so the verdicts are compared \
             instead (hydo repro --backend {b} repeats it on its own engine where that engine runs)"
        )
    })
}

/// Whether a repeated run gives the recorded one's answer: the fingerprint within one epoch and
/// one engine, the verdict's class across epochs (M155) or engines. Returns (same, the label for
/// the line).
fn repro_same(it: &ReproItem, now: &str, fp: &str, verdict: &str) -> (bool, &'static str) {
    if other_engine(it.backend.as_deref(), now).is_some() {
        let ok = verdict_kind(verdict) == verdict_kind(&it.verdict);
        return (ok, if ok { "SAME VERDICT" } else { "DIFFERENT" });
    }
    match hydo_image::comparable(it.epoch) {
        hydo_image::Compare::Fingerprints => {
            let ok = fp == it.fp;
            (ok, if ok { "REPRODUCED" } else { "DIFFERENT" })
        }
        hydo_image::Compare::Verdicts { .. } => {
            let ok = verdict_kind(verdict) == verdict_kind(&it.verdict);
            (ok, if ok { "SAME VERDICT" } else { "DIFFERENT" })
        }
    }
}

/// The plain sentence for a recorded epoch other than this hydo's, once.
fn epoch_note(epoch: Option<u64>) -> Option<String> {
    match hydo_image::comparable(epoch) {
        hydo_image::Compare::Fingerprints => None,
        hydo_image::Compare::Verdicts { recorded, now } => Some(format!(
            "hydo: {}",
            hydo_image::other_epoch_note(recorded, now)
        )),
    }
}

/// An exploration's document (the results reader); a directory without one is an error.
pub fn exploration_of(d: &Path) -> hydo_results::Exploration {
    match hydo_results::read_dir(d).unwrap_or_else(|e| die(&e.0)) {
        hydo_results::Results::Exploration { doc: Some(doc), .. } => *doc,
        hydo_results::Results::Exploration { doc: None, .. } => die(&format!(
            "{} has no {} yet (the exploration has not finished)",
            d.display(),
            hydo_results::EXPLORATION_DOC
        )),
        hydo_results::Results::Run { .. } => die(&format!(
            "{} is a run directory (hydo run), not an exploration",
            d.display()
        )),
    }
}

fn repro_list(ctx: &Ctx, d: &Path) -> Vec<ReproItem> {
    let doc = match hydo_results::read_dir(d).unwrap_or_else(|e| die(&e.0)) {
        hydo_results::Results::Run { run, .. } => {
            let image = run
                .image
                .as_ref()
                .map(|i| i.path.clone())
                .unwrap_or_default();
            if let Some(sha) = run
                .image
                .as_ref()
                .and_then(|i| i.sha256.as_deref())
                .filter(|s| !s.is_empty())
            {
                if Path::new(&image).is_file()
                    && sha256_file(Path::new(&image)).ok().as_deref() != Some(sha)
                {
                    println!("hydo: warning: {image} changed since the run (sha256 differs); fingerprints will differ");
                }
            }
            let eargs = run
                .engine
                .as_ref()
                .and_then(|e| e.args.clone())
                .unwrap_or_default();
            return vec![ReproItem {
                label: "run".into(),
                image: PathBuf::from(image),
                sched: run.schedule.clone(),
                fp: run.fingerprints.fp.clone(),
                eargs,
                epoch: run.epoch,
                verdict: run.verdict.text.clone(),
                backend: run.engine.as_ref().and_then(|e| e.backend.clone()),
            }];
        }
        hydo_results::Results::Exploration { .. } => exploration_of(d),
    };
    let mut image = PathBuf::from(&doc.image.path);
    if !image.is_absolute() {
        image = ctx.guest().join(image);
    }
    let e = &doc.engine;
    let eargs = engine_args(
        &image,
        Some(e.ram_mib.unwrap_or(1024)),
        Some(e.cpu.clone().unwrap_or_else(|| "v2".into())),
        e.vcpus.unwrap_or(1),
    );
    let mut out = Vec::new();
    for f in &doc.failures {
        out.push(ReproItem {
            label: format!(
                "seed {}",
                f.run.seed.map(|s| s.to_string()).unwrap_or_default()
            ),
            image: image.clone(),
            sched: f.run.schedule.clone(),
            fp: f.run.fingerprints.fp.clone(),
            eargs: eargs.clone(),
            epoch: doc.epoch,
            verdict: f.run.verdict.text.clone(),
            backend: e.backend.clone(),
        });
    }
    for x in &doc.shrinks {
        out.push(ReproItem {
            label: format!("minimal {}", head(&x.signature, 40)),
            image: image.clone(),
            sched: x.minimal.clone(),
            fp: x
                .confirmations
                .first()
                .and_then(|c| c.fp.clone())
                .unwrap_or_else(|| "None".into()),
            eargs: eargs.clone(),
            epoch: doc.epoch,
            verdict: x
                .confirmations
                .first()
                .and_then(|c| c.verdict.clone())
                .unwrap_or_default(),
            backend: e.backend.clone(),
        });
    }
    out
}

pub fn cmd_repro(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if let Some(i) = args.iter().position(|a| a == "--regressions") {
        args.remove(i);
        return cmd_regressions(ctx, args);
    }
    if let Some(t) = take(&mut args, "--transcript") {
        return cmd_transcript(ctx, &t, args);
    }
    if args.first().is_some_and(|a| Path::new(a).is_dir()) {
        let d = PathBuf::from(args.remove(0));
        let items = repro_list(ctx, &d);
        if items.is_empty() {
            println!(
                "hydo: {} recorded no failures; nothing to reproduce",
                d.display()
            );
            return 0;
        }
        let base = d.join(format!("repro-{}", strftime("%Y%m%d-%H%M%S")));
        let now = ctx.backend_name();
        let by_verdict = if let Some(n) = engine_note(items[0].backend.as_deref(), now) {
            println!("{n}");
            true
        } else if let Some(n) = epoch_note(items[0].epoch) {
            println!("{n}");
            true
        } else {
            false
        };
        let mut same = 0;
        for (k, it) in items.iter().enumerate() {
            let dir = base.join(format!("{k:03}"));
            let res = run_one(ctx, &it.image, &it.sched, &dir, &it.eargs, true);
            let res = host_checks(&dir, res, false);
            let res = errors_one(&dir, &it.sched, res, None);
            let run = run_document(
                &dir,
                &it.image,
                None,
                &it.sched,
                &it.eargs,
                &res,
                ctx.backend_name(),
            );
            write_run_document(&dir, &run);
            let fp = py_str(&res["fp"]);
            let (ok, word) = repro_same(it, now, &fp, &py_str(&res["verdict"]));
            same += ok as usize;
            println!(
                "{:<12} {}  fp={} (recorded {})  {}\n             {}",
                word,
                it.label,
                fp,
                it.fp,
                it.sched,
                head(&py_str(&res["verdict"]), 160)
            );
        }
        println!(
            "hydo: {} of {} reproduced with the recorded {} (consoles under {})",
            same,
            items.len(),
            if by_verdict { "verdict" } else { "fingerprint" },
            base.display()
        );
        return if same == items.len() { 0 } else { 1 };
    }
    cmd_run(ctx, argv)
}

// ---------------------------------------------------------------- transcripts (agent images, M126)

/// The recorded run next to a transcript.
#[derive(Default)]
struct Recorded {
    image: Option<PathBuf>,
    eargs: Option<Vec<String>>,
    fp: Option<String>,
    ctl: Option<String>,
    /// The fingerprint epoch it was recorded at (None: before M155, epoch 1).
    epoch: Option<u64>,
    verdict: Option<String>,
    /// The engine that recorded it (a run directory's result.json; None: not known).
    backend: Option<String>,
}

/// The recorded run next to a transcript: from a run directory's result.json, or from the
/// exploration whose `consoles/` holds it (its events.jsonl; the epoch from its start record).
fn recorded_of(t: &Path) -> Recorded {
    let dir = t.parent().unwrap_or(Path::new("."));
    if let Ok(hydo_results::Results::Run { run, .. }) = hydo_results::read_dir(dir) {
        return Recorded {
            image: run.image.as_ref().map(|i| PathBuf::from(&i.path)),
            eargs: run.engine.as_ref().and_then(|e| e.args.clone()),
            fp: Some(run.fingerprints.fp.clone()),
            ctl: run.fingerprints.ctl.clone(),
            epoch: run.epoch,
            verdict: Some(run.verdict.text.clone()),
            backend: run.engine.as_ref().and_then(|e| e.backend.clone()),
        };
    }
    let name = t
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(tag) = name.strip_suffix(".control.jsonl") else {
        return Recorded::default();
    };
    let ev = dir.parent().unwrap_or(Path::new(".")).join("events.jsonl");
    let text = std::fs::read_to_string(ev).unwrap_or_default();
    let mut epoch = None;
    for l in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        if v["type"] == "start" {
            epoch = v["epoch"].as_u64();
        }
        if v["type"] == "run" && v["tag"] == tag {
            let fps = &v["fingerprints"];
            return Recorded {
                fp: fps["fp"].as_str().map(str::to_string),
                ctl: fps["ctl"].as_str().map(str::to_string),
                epoch,
                verdict: v["verdict"]["text"].as_str().map(str::to_string),
                ..Recorded::default()
            };
        }
    }
    Recorded::default()
}

/// `hydo repro --transcript <control.jsonl> [--image IMG] [--out DIR]`: type a recorded run's
/// commands again at their recorded stops, without the controller's policy (the recovery boot's
/// transcript `control.recovery.jsonl` next to it, when there is one), and compare the fingerprint
/// and the control hash with the recorded run's. The boot to S1 runs by its policy (it is the
/// same for every run of an image).
fn cmd_transcript(ctx: &Ctx, file: &str, mut args: Vec<String>) -> i32 {
    let path = PathBuf::from(file);
    let read = |p: &Path| {
        let text =
            std::fs::read_to_string(p).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())));
        hydo_control::Transcript::parse(&text)
            .unwrap_or_else(|e| die(&format!("{}: {e}", p.display())))
    };
    let run = read(&path);
    if run.header.conversation != "run" {
        die(&format!(
            "{file}: a transcript of the {} conversation; give the run's (control.jsonl)",
            run.header.conversation
        ));
    }
    let rec_path = PathBuf::from(file.replace("control.jsonl", "control.recovery.jsonl"));
    let recovery = (rec_path != path && rec_path.is_file()).then(|| Box::new(read(&rec_path)));
    let sched = run
        .header
        .schedule
        .clone()
        .unwrap_or_else(|| die(&format!("{file}: the transcript names no schedule")));
    let rec = recorded_of(&path);
    if let Some(b) = other_engine(rec.backend.as_deref(), ctx.backend_name()) {
        // the transcript's stops are counted in its engine's clock: another engine cannot type it
        die(&format!(
            "{file}: a transcript recorded by the {b} engine; its stops are that engine's, so \
             it is typed again only by it (hydo repro --backend {b} --transcript {file})"
        ));
    }
    let (rec_image, rec_eargs, rec_fp, rec_ctl) = (rec.image, rec.eargs, rec.fp, rec.ctl);
    let outdir = take(&mut args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(new_run_dir);
    let image = if args.iter().any(|a| a == "--image") || !args.is_empty() || rec_image.is_none() {
        find_image(ctx, &mut args)
    } else {
        rec_image.clone().expect("an image")
    };
    if !args.is_empty() {
        die("usage: hydo repro --transcript <control.jsonl> [--image IMG] [--out DIR]");
    }
    let eargs = rec_eargs.unwrap_or_else(|| engine_args(&image, None, None, 1));
    // the recorded commands, stops and reports (a replay's header names its own controller)
    let want: Vec<_> = std::iter::once(run.records.clone())
        .chain(recovery.iter().map(|t| t.records.clone()))
        .collect();
    let rec_ctl = rec_ctl.unwrap_or_else(|| hydo_control::drive::ctl_of(&run, recovery.as_deref()));
    let policy = Policy::Replay {
        run: Box::new(run),
        recovery,
    };
    let res = run_one_with(ctx, &image, &sched, &outdir, &eargs, false, &policy);
    let res = host_checks(&outdir, res, false);
    let run_doc = run_document(
        &outdir,
        &image,
        sha256_file(&image).ok(),
        &sched,
        &eargs,
        &res,
        ctx.backend_name(),
    );
    write_run_document(&outdir, &run_doc);
    let fp = py_str(&res["fp"]);
    let got: Vec<_> = ["control.jsonl", "control.recovery.jsonl"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(outdir.join(f)).ok())
        .filter_map(|t| hydo_control::Transcript::parse(&t).ok())
        .map(|t| t.records)
        .collect();
    let same_records = got == want;
    println!(
        "hydo: transcript replay: fp={fp}{}, ctl={} (the recorded run's {rec_ctl}, which names its policy), {}",
        rec_fp
            .as_ref()
            .map(|f| format!(" (recorded {f})"))
            .unwrap_or_default(),
        res.get("ctl").map(py_str).unwrap_or_default(),
        if same_records {
            "the same commands at the same stops"
        } else {
            "OTHER commands or stops than recorded"
        }
    );
    // another fingerprint epoch (M155): the fingerprint and the transcript's stops are not
    // comparable, the verdict is
    let (ok, word) = match hydo_image::comparable(rec.epoch) {
        hydo_image::Compare::Fingerprints => {
            let same_fp = rec_fp.as_ref().is_none_or(|f| *f == fp);
            let ok = same_fp && same_records;
            (ok, if ok { "REPRODUCED" } else { "DIFFERENT" })
        }
        hydo_image::Compare::Verdicts { recorded, now } => {
            println!("hydo: {}", hydo_image::other_epoch_note(recorded, now));
            let ok = rec
                .verdict
                .as_deref()
                .is_none_or(|v| verdict_kind(v) == verdict_kind(&py_str(&res["verdict"])));
            (ok, if ok { "SAME VERDICT" } else { "DIFFERENT" })
        }
    };
    println!(
        "{}  {}\n             {}",
        word,
        sched,
        head(&py_str(&res["verdict"]), 160)
    );
    println!("hydo: run directory {}", outdir.display());
    if ok {
        0
    } else {
        1
    }
}

// ---------------------------------------------------------------- regressions (the test composer's plan)

fn regression_ok(expect: &str, verdict: &str, fp: &str) -> bool {
    match expect {
        "ok" => verdict_kind(verdict) == "ok",
        "violation" => verdict_kind(verdict) == "violation",
        _ => expect == format!("fp={fp}"),
    }
}

fn compose_meta(image: &Path) -> Value {
    embedded_compose(image)
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}))
}

/// One regression's pre-run check (M98). Returns false when it must not run.
fn regression_precheck(r: &Value, meta: &Value, bad: &mut usize) -> bool {
    let name = py_str(&r["name"]);
    if meta.get("stable_compose").is_some_and(truthy) {
        let st = hydo_composer::regression_status(r, meta, None)
            .unwrap_or_else(|e| die(&format!("regression {name}: {e}")));
        for w in &st.warnings {
            println!("hydo: warning: {name}: {w}");
        }
        if let Some(e) = st.error.as_ref().filter(|e| !e.is_empty()) {
            *bad += 1;
            println!(
                "FAIL  {}  expect {}  not run: {}\n      {}",
                name,
                py_str(&r["expect"]),
                e,
                py_str(&r["schedule"])
            );
            return false;
        }
    } else if let Some(w) = hydo_composer::regression_warning(r, meta)
        .unwrap_or_else(|e| die(&format!("regression {name}: {e}")))
    {
        println!("hydo: warning: {w}");
    }
    true
}

/// Python truthiness of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `hydo repro --regressions [IMG]`: every regression the image carries, with the host's history
/// checks applied; consoles next to the image. A run without a RESULT line fails its regression
/// and the others still run.
fn cmd_regressions(ctx: &Ctx, mut args: Vec<String>) -> i32 {
    let image = find_image(ctx, &mut args);
    let meta = compose_meta(&image);
    let regs: Vec<Value> = meta
        .get("regressions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if regs.is_empty() {
        println!(
            "hydo: {} carries no regressions (hydo/regressions/*.schedule or test.regressions in the plan)",
            image.display()
        );
        return 0;
    }
    let base = image
        .parent()
        .unwrap_or(Path::new(""))
        .join(format!("regressions-{}", strftime("%Y%m%d-%H%M%S")));
    let eargs = engine_args(&image, None, None, 1);
    let mut bad = 0usize;
    for (k, r) in regs.iter().enumerate() {
        if !regression_precheck(r, &meta, &mut bad) {
            continue;
        }
        let name = py_str(&r["name"]);
        let sched = py_str(&r["schedule"]);
        let dir = base.join(format!("{k:03}-{name}"));
        let (fp, v) = match run_engine(ctx, &image, &sched, &dir, &eargs, true, &Policy::Schedule).0
        {
            Some(res) => {
                let res = host_checks(&dir, result_map(&res), true);
                (py_str(&res["fp"]), py_str(&res["verdict"]))
            }
            None => ("-".into(), "ERROR no RESULT line".into()),
        };
        let ex = py_str(&r["expect"]);
        let ok = regression_ok(&ex, &v, &fp);
        bad += !ok as usize;
        println!(
            "{:<5} {}  expect {}  fp={}  {}\n      {}",
            if ok { "PASS" } else { "FAIL" },
            name,
            ex,
            fp,
            sched,
            head(&v, 160)
        );
    }
    println!(
        "hydo: {} of {} regressions as expected (consoles under {})",
        regs.len() - bad,
        regs.len(),
        base.display()
    );
    if bad > 0 {
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------- save

/// Python's `re.sub(r"[^A-Za-z0-9_.-]+", "-", s)`.
fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    out
}

pub fn cmd_save(_ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    let to = take(&mut args, "--to").unwrap_or_else(|| "hydo/regressions".into());
    if args.len() != 1
        || !Path::new(&args[0])
            .join(hydo_results::EXPLORATION_DOC)
            .is_file()
    {
        die("usage: hydo save <explore dir> [--to hydo/regressions]");
    }
    let doc = exploration_of(Path::new(&args[0]));
    if let Err(e) = std::fs::create_dir_all(&to) {
        die(&format!("{to}: {e}"));
    }
    let mut n = 0;
    for s in &doc.shrinks {
        // the shrink as the composer reads it (the results format keeps the field names)
        let mut x = serde_json::to_value(s).unwrap_or(Value::Null);
        if let Some(o) = x.as_object_mut() {
            o.entry("min_verdict").or_insert(Value::Null);
        }
        let name = head(
            &slug(&format!(
                "{}-{}",
                py_str(&x["class"]),
                py_str(&x["signature"])
            )),
            60,
        );
        let name = name.trim_matches('-');
        let p = pjoin(&to, &format!("{name}.schedule"));
        if Path::new(&p).exists() {
            println!("hydo: {p} exists, kept");
            continue;
        }
        let text = hydo_composer::saved_regression_text(&x)
            .unwrap_or_else(|e| die(&format!("{}: {e}", args[0])));
        if let Err(e) = std::fs::write(&p, text) {
            die(&format!("{p}: {e}"));
        }
        n += 1;
        println!("hydo: wrote {p}");
        if truthy(&x["stable_note"]) {
            println!("hydo: note: {}", py_str(&x["stable_note"]));
        }
    }
    println!("hydo: {n} regressions saved; `hydo build` stages them, `hydo repro --regressions` replays them");
    0
}
