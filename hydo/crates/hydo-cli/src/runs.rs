//! `hydo run`, `hydo repro`, `hydo repro --regressions`, `hydo save`: runs from a fresh boot
//! through the engine interface (hydo-engine: hvx on Linux, hvm on the Mac), the history checks
//! (hydo-history), the error logs of a run (hydo-errors), saved regressions (hydo-composer), and
//! the files a run leaves (run.json, engine.log, the consoles). One behaviour on both platforms
//! since M107; only the engine and the image's file name differ.

use crate::ctx::{
    abspath, die, glob_simple, pjoin, read_text, strftime, take, Ctx, Platform, OUT, PROJECT_OUT,
};
use crate::hvxenv;
use crate::pyjson::{dumps, head, py_str, tail};
use hydo_engine::hvm::{kernel_for, Hvm};
use hydo_engine::hvx::Hvx;
use hydo_engine::image::{embedded_compose, embedded_manifest, sha256_file};
use hydo_engine::protocol::verdict_kind;
use hydo_engine::{BootSpec, CpuLevel, Engine, RunResult};
use hydo_manifest::Guest;
use hydo_schedule::Schedule;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

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

/// The kernel for an image (hvm_backend.kernel_for); a refusal ends the command with `hydo: `.
pub fn mac_kernel_for(ctx: &Ctx, image: &Path, quiet: bool) -> PathBuf {
    let k = kernel_for(image, None, &mac_kernel(ctx)).unwrap_or_else(|msg| die(&msg));
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
fn run_engine(
    ctx: &Ctx,
    image: &Path,
    sched: &str,
    outdir: &Path,
    eargs: &[String],
    quiet: bool,
) -> (Option<RunResult>, String) {
    let _ = std::fs::create_dir_all(outdir);
    let s = schedule(sched);
    let run = match ctx.platform {
        Platform::Linux => {
            let engine = hvxenv::engine(ctx);
            if !crate::ctx::access(&engine, libc::X_OK) {
                die(&format!(
                    "{} is not built (run install.sh again)",
                    engine.display()
                ));
            }
            let hvx = Hvx {
                engine,
                hvx_run: Some(ctx.libexec().join("hvx-run")),
                snap_dir: hvxenv::snap_dir(),
                extra_env: Vec::new(),
                env: hvxenv::tool_env(ctx),
            };
            let spec = spec_from_args(
                eargs,
                image,
                hvxenv::bzimage(ctx),
                Some(hvxenv::initrd(ctx)),
                &abspath(outdir),
            );
            hvx.one(&spec, &s)
        }
        Platform::Mac => {
            let kernel = mac_kernel_for(ctx, image, quiet);
            let spec = spec_from_args(eargs, image, kernel, None, outdir);
            Hvm::for_release(&ctx.root).one(&spec, &s)
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
    match run_engine(ctx, image, sched, outdir, eargs, quiet) {
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

/// History checks on the host (hydo/HISTORIES.md): writes one.checks.log and one.checks.txt and
/// folds the combined verdict into res (the guest's own as guest_verdict).
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
    let _ = hydo_history::write_checks_files(outdir, "one", &r);
    let gv = res.get("verdict").cloned().unwrap_or(Value::Null);
    res.insert("guest_verdict".into(), gv);
    res.insert("verdict".into(), json!(r.verdict));
    if !quiet {
        println!(
            "hydo: history checks: {}   (explanations in {})",
            verdict_text(&r),
            outdir.join("one.checks.txt").display()
        );
    }
    res
}

/// Error logs as findings for one run (the hydo-errors crate): prints the kinds, writes
/// one.errors.json, folds the verdict into res.
fn errors_one(
    outdir: &Path,
    sched: &str,
    res: Map<String, Value>,
    known: Option<&str>,
) -> Map<String, Value> {
    hydo_errors::for_run_dir(outdir, sched, res, known, &mut |l| println!("{l}"))
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
    let res = run_one(ctx, &image, &sched, &outdir, &eargs, false);
    let res = host_checks(&outdir, res, false);
    let res = errors_one(&outdir, &sched, res, known.as_deref());
    let mut rec = Map::new();
    rec.insert("image".into(), json!(image.display().to_string()));
    rec.insert(
        "image_sha256".into(),
        json!(sha256_file(&image).unwrap_or_else(|e| die(&format!("{}: {e}", image.display())))),
    );
    rec.insert("schedule".into(), json!(sched));
    rec.insert("engine_args".into(), json!(eargs));
    for (k, v) in res {
        rec.insert(k, v);
    }
    let _ = std::fs::write(outdir.join("run.json"), dumps(&Value::Object(rec), 1));
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
}

fn load_json(p: &Path) -> Value {
    let t = std::fs::read_to_string(p).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())));
    serde_json::from_str(&t).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())))
}

fn repro_list(ctx: &Ctx, d: &Path) -> Vec<ReproItem> {
    let rj = d.join("run.json");
    if rj.is_file() {
        let r = load_json(&rj);
        let image = py_str(&r["image"]);
        if let Some(sha) = r["image_sha256"].as_str().filter(|s| !s.is_empty()) {
            if Path::new(&image).is_file()
                && sha256_file(Path::new(&image)).ok().as_deref() != Some(sha)
            {
                println!("hydo: warning: {image} changed since the run (sha256 differs); fingerprints will differ");
            }
        }
        let eargs = r["engine_args"]
            .as_array()
            .map(|a| a.iter().map(py_str).collect())
            .unwrap_or_default();
        return vec![ReproItem {
            label: "run".into(),
            image: PathBuf::from(image),
            sched: py_str(&r["schedule"]),
            fp: py_str(&r["fp"]),
            eargs,
        }];
    }
    let rep = d.join("report.json");
    if !rep.is_file() {
        die(&format!(
            "{} has neither run.json (hydo run) nor report.json (hydo explore)",
            d.display()
        ));
    }
    let j = load_json(&rep);
    let s = &j["summary"];
    let mut image = PathBuf::from(py_str(&s["image"]));
    if !image.is_absolute() {
        image = ctx.guest().join(image);
    }
    let ram = s.get("ram_mib").and_then(Value::as_u64).unwrap_or(1024);
    let cpu = s
        .get("cpu")
        .and_then(Value::as_str)
        .unwrap_or("v2")
        .to_string();
    let vcpus = s.get("vcpus").and_then(Value::as_u64).unwrap_or(1);
    let eargs = engine_args(&image, Some(ram), Some(cpu), vcpus);
    let mut out = Vec::new();
    for f in j["failures"].as_array().into_iter().flatten() {
        out.push(ReproItem {
            label: format!("seed {}", py_str(&f["seed"])),
            image: image.clone(),
            sched: py_str(&f["schedule"]),
            fp: py_str(&f["fp"]),
            eargs: eargs.clone(),
        });
    }
    for x in j
        .get("shrunk")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        out.push(ReproItem {
            label: format!("minimal {}", head(&py_str(&x["signature"]), 40)),
            image: image.clone(),
            sched: py_str(&x["minimal"]),
            fp: py_str(&x["fp_a"]),
            eargs: eargs.clone(),
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
        let mut same = 0;
        for (k, it) in items.iter().enumerate() {
            let dir = base.join(format!("{k:03}"));
            let res = run_one(ctx, &it.image, &it.sched, &dir, &it.eargs, true);
            let res = host_checks(&dir, res, false);
            let res = errors_one(&dir, &it.sched, res, None);
            let fp = py_str(&res["fp"]);
            let ok = fp == it.fp;
            same += ok as usize;
            println!(
                "{:<12} {}  fp={} (recorded {})  {}\n             {}",
                if ok { "REPRODUCED" } else { "DIFFERENT" },
                it.label,
                fp,
                it.fp,
                it.sched,
                head(&py_str(&res["verdict"]), 160)
            );
        }
        println!(
            "hydo: {} of {} reproduced with the recorded fingerprint (consoles under {})",
            same,
            items.len(),
            base.display()
        );
        return if same == items.len() { 0 } else { 1 };
    }
    cmd_run(ctx, argv)
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
        let (fp, v) = match run_engine(ctx, &image, &sched, &dir, &eargs, true).0 {
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
    if args.len() != 1 || !Path::new(&args[0]).join("report.json").is_file() {
        die("usage: hydo save <explore dir> [--to hydo/regressions]");
    }
    let j = load_json(&Path::new(&args[0]).join("report.json"));
    if let Err(e) = std::fs::create_dir_all(&to) {
        die(&format!("{to}: {e}"));
    }
    let mut n = 0;
    for x in j
        .get("shrunk")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
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
        let text = hydo_composer::saved_regression_text(x)
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
