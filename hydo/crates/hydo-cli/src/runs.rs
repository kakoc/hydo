//! `hydo run`, `hydo repro`, `hydo repro --regressions`, `hydo save`: runs from a fresh boot
//! through the engine interface (hydo-engine), the history checks and error logs of a run (still
//! Python, through the bridge), and the files the Python CLIs wrote (run.json, consoles).

use crate::bridge;
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

/// Linux: `--image IMG`, or a leading positional `*.img`, or the one `*-disk.img` in ./hydo-out
/// (then ./.hydo).
pub fn find_image_linux(args: &mut Vec<String>) -> PathBuf {
    let mut img = take(args, "--image");
    if img.is_none() && args.first().is_some_and(|a| a.ends_with(".img")) {
        img = Some(args.remove(0));
    }
    let img = match img {
        Some(i) => i,
        None => {
            let mut imgs = glob_simple(Path::new(OUT), "", "-disk.img");
            if imgs.is_empty() {
                imgs = glob_simple(Path::new(PROJECT_OUT), "", "-disk.img");
            }
            if imgs.len() != 1 {
                die(&format!(
                    "no image given and {} *-disk.img in ./{} or ./{} (run `hydo build hydo.yaml` here, or pass --image)",
                    imgs.len(),
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

/// Mac: `--image IMG`, or the one `*-arm64.cpio.gz` in ./hydo-out.
pub fn default_image_mac(args: &mut Vec<String>) -> PathBuf {
    if let Some(img) = take(args, "--image") {
        return abspath(Path::new(&img));
    }
    let imgs = glob_simple(Path::new(OUT), "", "-arm64.cpio.gz");
    if imgs.len() != 1 {
        die(&format!(
            "no image given and {} images in ./{} (run `hydo build hydo.yaml` here, or pass --image)",
            imgs.len(),
            OUT
        ));
    }
    abspath(&imgs[0])
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

/// The engine options explore.py would pass for this image (engine_args_for).
pub fn linux_engine_args(
    image: &Path,
    ram: Option<u64>,
    cpu: Option<String>,
    vcpus: u64,
) -> Vec<String> {
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

// ---------------------------------------------------------------- Linux: one run

/// One schedule from a fresh boot (hvx_explore one). Writes engine.log; dies without a RESULT.
pub fn run_one_linux(
    ctx: &Ctx,
    image: &Path,
    sched: &str,
    outdir: &Path,
    eargs: &[String],
    quiet: bool,
) -> Map<String, Value> {
    let _ = std::fs::create_dir_all(outdir);
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
    let run = hvx
        .one(&spec, &schedule(sched))
        .unwrap_or_else(|e| die(&format!("cannot start the engine: {e}")));
    let _ = std::fs::write(outdir.join("engine.log"), &run.output);
    match run.result {
        Some(r) => {
            if !quiet {
                println!("{}", r.line);
            }
            result_map(&r)
        }
        None => {
            eprint!("{}", tail(&run.output, 3000));
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

/// Write one.checks.log and one.checks.txt from a run_checks answer.
fn write_checks(outdir: &Path, r: &Value) {
    let recs: String = r["records"]
        .as_array()
        .map(|a| a.iter().map(|x| format!("{}\n", py_str(x))).collect())
        .unwrap_or_default();
    let _ = std::fs::write(outdir.join("one.checks.log"), recs);
    let _ = std::fs::write(
        outdir.join("one.checks.txt"),
        format!("{}\n{}\n", py_str(&r["report"]), py_str(&r["verdict"])),
    );
}

/// History checks on the host (hydo/HISTORIES.md), Linux: folds the verdict into res.
fn host_checks_linux(ctx: &Ctx, outdir: &Path, mut res: Map<String, Value>) -> Map<String, Value> {
    let srcs = console_sources(outdir);
    if !srcs.iter().any(|(_, t)| t.contains("HYDO-CHECK ")) {
        return res;
    }
    let verdict = res
        .get("verdict")
        .and_then(Value::as_str)
        .map(str::to_string);
    let r = bridge::run_checks(ctx, &srcs, verdict.as_deref());
    write_checks(outdir, &r);
    let gv = res.get("verdict").cloned().unwrap_or(Value::Null);
    res.insert("guest_verdict".into(), gv);
    res.insert("verdict".into(), r["verdict"].clone());
    println!(
        "hydo: history checks: {}   (explanations in {})",
        py_str(&r["verdict"]),
        outdir.join("one.checks.txt").display()
    );
    res
}

fn errors_one(
    ctx: &Ctx,
    outdir: &Path,
    sched: &str,
    res: Map<String, Value>,
    known: Option<&str>,
) -> Map<String, Value> {
    match bridge::errors_for_run_dir(ctx, outdir, sched, &Value::Object(res.clone()), known) {
        Value::Object(m) => m,
        _ => res,
    }
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

pub fn cmd_run_linux(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    let outdir = take(&mut args, "--out");
    let known = take(&mut args, "--known-errors");
    let image = find_image_linux(&mut args);
    if args.len() != 1 {
        die("usage: hydo run [IMG] \"<schedule>\" [--image IMG] [--out DIR]   e.g. hydo run \"disk=cut | run C=12 R=everysec\"");
    }
    let sched = args[0].clone();
    let outdir = outdir.map(PathBuf::from).unwrap_or_else(new_run_dir);
    let eargs = linux_engine_args(&image, None, None, 1);
    let res = run_one_linux(ctx, &image, &sched, &outdir, &eargs, false);
    let res = host_checks_linux(ctx, &outdir, res);
    let res = errors_one(ctx, &outdir, &sched, res, known.as_deref());
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
    let eargs = linux_engine_args(&image, Some(ram), Some(cpu), vcpus);
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

pub fn cmd_repro_linux(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if let Some(i) = args.iter().position(|a| a == "--regressions") {
        args.remove(i);
        return cmd_regressions_linux(ctx, args);
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
            let res = run_one_linux(ctx, &it.image, &it.sched, &dir, &it.eargs, true);
            let res = host_checks_linux(ctx, &dir, res);
            let res = errors_one(ctx, &dir, &it.sched, res, None);
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
    cmd_run_linux(ctx, argv)
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
fn regression_precheck(ctx: &Ctx, r: &Value, meta: &Value, bad: &mut usize) -> bool {
    let name = py_str(&r["name"]);
    if meta.get("stable_compose").is_some_and(truthy) {
        let st = bridge::regression_status(ctx, r, meta);
        for w in st["warnings"].as_array().into_iter().flatten() {
            println!("hydo: warning: {}: {}", name, py_str(w));
        }
        if truthy(&st["error"]) {
            *bad += 1;
            println!(
                "FAIL  {}  expect {}  not run: {}\n      {}",
                name,
                py_str(&r["expect"]),
                py_str(&st["error"]),
                py_str(&r["schedule"])
            );
            return false;
        }
    } else if let Some(w) = bridge::regression_warning(ctx, r, meta) {
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

fn cmd_regressions_linux(ctx: &Ctx, mut args: Vec<String>) -> i32 {
    let image = find_image_linux(&mut args);
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
    let eargs = linux_engine_args(&image, None, None, 1);
    let mut bad = 0usize;
    for (k, r) in regs.iter().enumerate() {
        if !regression_precheck(ctx, r, &meta, &mut bad) {
            continue;
        }
        let name = py_str(&r["name"]);
        let sched = py_str(&r["schedule"]);
        let res = run_one_linux(
            ctx,
            &image,
            &sched,
            &base.join(format!("{k:03}-{name}")),
            &eargs,
            true,
        );
        let (fp, v) = (py_str(&res["fp"]), py_str(&res["verdict"]));
        let ok = regression_ok(&py_str(&r["expect"]), &v, &fp);
        bad += !ok as usize;
        println!(
            "{:<5} {}  expect {}  fp={}  {}\n      {}",
            if ok { "PASS" } else { "FAIL" },
            name,
            py_str(&r["expect"]),
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

// ---------------------------------------------------------------- Mac: one run

pub fn mac_kernel(ctx: &Ctx) -> PathBuf {
    ctx.p("hv/hvm/explore/out/Image-explore")
}

/// The kernel for an image (hvm_backend.kernel_for); exits like its SystemExit when refused.
pub fn mac_kernel_for(ctx: &Ctx, image: &Path, quiet: bool) -> PathBuf {
    let k = kernel_for(image, None, &mac_kernel(ctx)).unwrap_or_else(|msg| {
        eprintln!("{msg}");
        std::process::exit(1)
    });
    if !quiet && k != mac_kernel(ctx) {
        let rel = k.strip_prefix(&ctx.root).unwrap_or(&k);
        println!(
            "hydo: guest kernel {} (recorded in the image)",
            rel.display()
        );
    }
    k
}

/// One schedule from a fresh boot on hvmx; returns the engine's output text (the Mac CLI's run_once).
pub fn run_once_mac(ctx: &Ctx, image: &Path, sched: &str, outdir: &Path, quiet: bool) -> String {
    let _ = std::fs::create_dir_all(outdir);
    let ram = image_guest(image).ram_mib;
    let kernel = mac_kernel_for(ctx, image, quiet);
    let hvm = Hvm::for_release(&ctx.root);
    let mut spec = spec_from_args(&[], image, kernel, None, outdir);
    spec.ram_mib = ram;
    let run = hvm
        .one(&spec, &schedule(sched))
        .unwrap_or_else(|e| die(&format!("cannot start the engine: {e}")));
    for line in run.output.lines() {
        if !quiet && (line.starts_with("RESULT") || line.starts_with("hvmx: one")) {
            println!("{line}");
        }
    }
    run.output
}

/// The first RESULT line's (fp, verdict) by the Mac CLI's regex `RESULT \S+ fp=(\S+) .*?verdict=(.*)`.
fn mac_result(text: &str) -> Option<(String, String)> {
    text.lines().find_map(|l| {
        let rest = l.strip_prefix("RESULT ")?;
        let (_, rest) = rest.split_once(' ')?;
        let rest = rest.strip_prefix("fp=")?;
        let (fp, rest) = rest.split_once(' ')?;
        if fp.is_empty() || fp.contains(char::is_whitespace) {
            return None;
        }
        let i = rest.find("verdict=")?;
        Some((fp.to_string(), rest[i + 8..].to_string()))
    })
}

/// History checks on the host, Mac: returns the combined verdict, or None without checks.
fn host_checks_mac(ctx: &Ctx, outdir: &Path, text: &str, quiet: bool) -> Option<String> {
    let (_, verdict) = mac_result(text)?;
    let srcs = console_sources(outdir);
    if !srcs.iter().any(|(_, t)| t.contains("HYDO-CHECK ")) {
        return None;
    }
    let r = bridge::run_checks(ctx, &srcs, Some(&verdict));
    write_checks(outdir, &r);
    if !quiet {
        println!(
            "hydo: history checks: {}   (explanations in {})",
            py_str(&r["verdict"]),
            outdir.join("one.checks.txt").display()
        );
    }
    Some(py_str(&r["verdict"])).filter(|v| !v.is_empty() && v != "None")
}

pub fn cmd_run_mac(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    let image = default_image_mac(&mut args);
    if args.len() != 1 {
        die("usage: hydo run \"<schedule>\" [--image IMG]   e.g. hydo run \"disk=cut | run C=12\"");
    }
    let sched = args[0].clone();
    let outdir = Path::new(OUT)
        .join("runs")
        .join(format!("run-{}", std::process::id()));
    let text = run_once_mac(ctx, &image, &sched, &outdir, false);
    host_checks_mac(ctx, &outdir, &text, false);
    let mut cons: Vec<String> = std::fs::read_dir(&outdir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .map(|n| pjoin(&outdir.display().to_string(), &n))
                .collect()
        })
        .unwrap_or_default();
    cons.sort();
    if !cons.is_empty() {
        println!("hydo: console(s): {}", cons.join(" "));
    }
    if !text.lines().any(|l| l.starts_with("RESULT")) {
        eprint!("{}", tail(&text, 3000));
        die("the run produced no RESULT line (output above)");
    }
    let res = match mac_result(&text) {
        Some((fp, v)) => json!({"fp": fp, "verdict": v}),
        None => json!({"verdict": ""}),
    };
    bridge::errors_for_run_dir(ctx, &outdir, &sched, &res, None);
    0
}

pub fn cmd_repro_mac(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if let Some(i) = args.iter().position(|a| a == "--regressions") {
        args.remove(i);
        return cmd_regressions_mac(ctx, args);
    }
    cmd_run_mac(ctx, &args)
}

fn cmd_regressions_mac(ctx: &Ctx, mut args: Vec<String>) -> i32 {
    let image = default_image_mac(&mut args);
    let meta: Value = hydo_engine::image::embedded_file(&image, "etc/hydo/compose.json")
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
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
    let base = Path::new(OUT).join(format!("regressions-{}", strftime("%Y%m%d-%H%M%S")));
    let mut bad = 0usize;
    for (k, r) in regs.iter().enumerate() {
        if !regression_precheck(ctx, r, &meta, &mut bad) {
            continue;
        }
        let name = py_str(&r["name"]);
        let sched = py_str(&r["schedule"]);
        let dir = base.join(format!("{k:03}-{name}"));
        let text = run_once_mac(ctx, &image, &sched, &dir, true);
        let (fp, v) =
            mac_result(&text).unwrap_or_else(|| ("-".into(), "ERROR no RESULT line".into()));
        let v = host_checks_mac(ctx, &dir, &text, true).unwrap_or(v);
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

pub fn cmd_save(ctx: &Ctx, argv: &[String]) -> i32 {
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
        let text = bridge::saved_regression_text(ctx, x);
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

/// Platform dispatch for run/repro.
pub fn cmd_run(ctx: &Ctx, argv: &[String]) -> i32 {
    match ctx.platform {
        Platform::Linux => cmd_run_linux(ctx, argv),
        Platform::Mac => cmd_run_mac(ctx, argv),
    }
}

pub fn cmd_repro(ctx: &Ctx, argv: &[String]) -> i32 {
    match ctx.platform {
        Platform::Linux => cmd_repro_linux(ctx, argv),
        Platform::Mac => cmd_repro_mac(ctx, argv),
    }
}
