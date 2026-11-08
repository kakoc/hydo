//! The explorer's main flow (explore.py `main`, `branch_main`, `shrink`, `errlog_shrink`,
//! `write_report`): set up the fault space and the engine, run the first runs, re-run every
//! failure, shrink one failure per (class, signature) and every new error-log kind, write the
//! reports.
//!
//! The order of work is not explore.py's batches: a failure's re-runs and its shrink start as soon
//! as every earlier seed has finished (so the representative of each signature is the same first
//! failure as explore.py's), all shrinks run side by side, and a shrink round stops running its
//! candidates once the first one that keeps the failure is known. Every result is still a pure
//! function of its schedule, so the records, the shrink paths and the minimal schedules are
//! explore.py's; `tries` counts every candidate of a round, as explore.py did, so shrink budgets
//! end at the same place.
//!
//! What it writes is the results format (hydo-results): `events.jsonl` as it runs (a start
//! record, every run, the shrinks, an end record), `exploration.json` when it is done, and
//! report.md, a view of exploration.json for people ([`crate::md`]).

use crate::args::{self, Args};
use crate::compose;
use crate::errlog::{self, Known};
use crate::fc;
use crate::gen::{Backend, Ex};
use crate::md;
use crate::pool::{self, has_errlog, Host, Pool, Res};
use crate::py::{dumps_line, head, py_str, round1};
use crate::report::{self, assert_failures, same_failure, sig_key, verdict_kind};
use crate::results::{self, Events};
use crate::sched::S;
use crate::space::{grpc_targets, Space};
use hydo_engine::{hvm, hvx, BootSpec, ControlledEngine, CpuLevel, Engine, SnapshotStore};
use hydo_results::{
    role, Config, End, ErrorKinds, ErrorShrink, Exploration, ExplorationArtifacts, Failure, Image,
    Reproduction, Run, ShrinkStep, Start, Summary,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Where the explorer runs: the repository or release root (hydo/, hv/), whether it was
/// started by the `hydo` command (the reports' "by hand" lines then name `hydo run`), and the
/// configuration its program read at its start (M144: the images and the wrapper in
/// `config.paths`, the pool's quiet window and cache in `config.pool`, the engine in
/// `config.engine` and `config.env`).
#[derive(Clone, Debug)]
pub struct Env {
    pub root: PathBuf,
    pub cli: bool,
    pub config: hydo_config::Config,
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

/// Everything the flow needs, set up once.
struct Cx {
    a: Args,
    ex: Ex,
    out: PathBuf,
    env: Env,
    image: String,
    build: PathBuf,
    ram_mib: i64,
    vcpus: i64,
    cpu: String,
    hvm_kernel: Option<PathBuf>,
    /// `--backend kvm`: the KVM engine (hydo-kvm). Its schedules are drawn as the Mac engine's
    /// (the tokens both apply: no `cf=`, `pp=` or `xt*`; forced preemption points), its images are
    /// the x86_64 images hvx boots.
    kvm: bool,
    known: Option<Known>,
    /// The explorer's command line (the results' configuration).
    argv: Vec<String>,
    /// events.jsonl (opened when the run starts).
    ev: Events,
}

impl Cx {
    /// The image is an arm64 initramfs (the Mac engine), not an x86_64 disk.
    fn arm(&self) -> bool {
        self.ex.backend == Backend::Hvm && !self.kvm
    }

    fn hvm_dir(&self) -> PathBuf {
        self.env.root.join("hv").join("hvm").join("explore")
    }

    /// hvm_backend.resolve_image.
    fn hvm_image(&self) -> PathBuf {
        let p = Path::new(&self.image);
        if p.is_absolute() || p.exists() {
            abspath(p)
        } else {
            self.hvm_dir().join("out").join(&self.image)
        }
    }

    fn hvm_default_kernel(&self) -> PathBuf {
        self.hvm_dir().join("out").join("Image-explore")
    }

    fn ram_args(&self) -> Vec<String> {
        if self.ram_mib == 1024 {
            vec![]
        } else {
            vec!["--ram-mib".into(), self.ram_mib.to_string()]
        }
    }

    /// explore.py `engine_args`.
    fn engine_args(&self) -> Vec<String> {
        let mut a = self.ram_args();
        if self.vcpus != 1 {
            a.push("--vcpus".into());
            a.push(self.vcpus.to_string());
        }
        if self.cpu == "v3" && (self.ex.backend == Backend::Hvx || self.kvm) {
            a.push("--cpu".into());
            a.push("v3".into());
        }
        a
    }

    /// explore.py `by_hand`: the command line that runs one schedule: `hydo run` (M107 when
    /// started by the `hydo` command; always since M145, which deleted the engine program's plain
    /// `one` mode that the line named before).
    fn by_hand(&self, sched: &str) -> String {
        let img = if self.arm() {
            self.hvm_image()
        } else {
            self.build.join(&self.image)
        };
        format!("`hydo run --image {} \"{}\"`", img.display(), sched)
    }

    fn render(&self, s: &S) -> String {
        self.ex.render(s)
    }

    fn parse(&self, t: &str) -> S {
        self.ex.parse(t).unwrap_or_else(|e| {
            die(&format!(
                "hydo explore: cannot read the schedule {t:?}: {e}"
            ))
        })
    }

    fn compose_meta(&self) -> Option<&Value> {
        self.ex
            .space
            .as_ref()
            .and_then(|s| s.compose.as_ref())
            .filter(|m| hydo_manifest::py::truthy(m))
    }
}

fn abspath(p: &Path) -> PathBuf {
    let base = if p.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    let mut out = PathBuf::new();
    for c in base.join(p).components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The explorer's entry point: explore.py's command line; the exit status.
pub fn main_with(argv: &[String], env: &Env) -> i32 {
    let a = match args::parse(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}\nhydo explore: error: {e}", args::USAGE);
            return 2;
        }
    };
    let mut cx = setup(a, env);
    cx.argv = argv.to_vec();
    run(cx)
}

fn setup(a: Args, env: &Env) -> Cx {
    let mut known = errlog::load(a.known_errors.as_deref())
        .unwrap_or_else(|e| die(&format!("hydo explore: --known-errors: {e}")));
    if let Some(e) = &a.errors {
        known.unknown = e.clone();
    }
    if a.fc_adaptive && a.sweep.as_deref() != Some("freeze") {
        die("hydo explore: --fc-adaptive goes with --sweep freeze");
    }
    let kvm = a.backend == "kvm";
    let ex = Ex {
        backend: if a.backend == "hvm" || kvm {
            Backend::Hvm
        } else {
            Backend::Hvx
        },
        // the KVM engine has the image's disk and its tc, as hvx
        hvm_disk: kvm,
        hvm_tc: kvm,
        ..Ex::default()
    };
    let build = env.config.paths.build.clone();
    let mut cx = Cx {
        ex,
        out: PathBuf::from(&a.out),
        env: env.clone(),
        image: a.image.clone(),
        build,
        ram_mib: 1024,
        vcpus: 1,
        cpu: "v2".into(),
        hvm_kernel: a.hvm_kernel.as_ref().map(PathBuf::from),
        kvm,
        known: Some(known),
        argv: Vec::new(),
        ev: Events::none(),
        a,
    };
    let a = cx.a.clone();
    if cx.arm() {
        let img = cx.hvm_image();
        cx.ex.hvm_disk = hvm::disk_for(&img).is_some();
        // explore.py stopped here with a traceback for a file that is not an arm64 initramfs
        cx.ex.hvm_tc = hvm::image_has_tc(&img).unwrap_or_else(|e| {
            die(&format!(
                "hydo: {}: not a hydo arm64 image ({e}; no etc/hydo/manifest.sh)",
                img.display()
            ))
        });
        // the kernel the image's record names (M155; before it, /etc/hydo/kernel, M99)
        let b = hydo_engine::image::boot_files(
            &img,
            &cx.hvm_default_kernel(),
            None,
            cx.hvm_kernel.as_deref(),
            &[],
        )
        .unwrap_or_else(|e| die(&format!("hydo: {e}")));
        if let Some(n) = &b.note {
            eprintln!("hydo: {n}");
        }
        if b.kernel != cx.hvm_default_kernel() {
            eprintln!(
                "guest kernel: {} (recorded in the image)",
                b.kernel.display()
            );
        }
        cx.hvm_kernel = Some(b.kernel);
    }
    let base = Path::new(&a.image)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let named = a.manifest.is_some()
        || a.image.starts_with("explore-ob-")
        || base.starts_with("explore-ob-");
    // Every image hydo build makes carries its manifest (M136), whatever the file is called: an
    // image copied or renamed away from its explore-ob-* name keeps its own fault space instead
    // of silently getting the built-in one (whose services the image does not have).
    // An image without a manifest gets no fault space by default: the built-in one is the stack
    // example's (kafka, mysql, redis, front), and exploring it on another system's image tests
    // nothing and ends OK (the libSQL onboarding's friction item 6). Only the scripted engine of
    // the tests explores the built-in space without a manifest.
    let carried = if named || cx.env.config.pool.scripted_engine.is_some() {
        None
    } else {
        let got = if cx.arm() {
            hydo_engine::image::embedded_manifest(&cx.hvm_image()).map(|t| {
                (
                    t,
                    hydo_engine::image::embedded_file(&cx.hvm_image(), "etc/hydo/compose.json")
                        .ok(),
                )
            })
        } else {
            let img = cx.build.join(&a.image);
            hydo_engine::image::debugfs_cat(&img, "etc/hydo/hydo.yaml")
                .map(|t| (t, hydo_engine::image::embedded_compose(&img)))
        };
        match got {
            Ok(x) => Some(x),
            Err(e) => die(&no_manifest(&a.image, &e.to_string())),
        }
    };
    if named || carried.is_some() {
        let sp = if let Some(m) = &a.manifest {
            Space::from_path(Path::new(m))
        } else if let Some((text, comp)) = carried {
            Space::from_text(&text, comp.and_then(|t| serde_json::from_str(&t).ok()))
        } else if cx.arm() {
            let img = cx.hvm_image();
            let text = hydo_engine::image::embedded_manifest(&img)
                .unwrap_or_else(|_| die(&format!("no etc/hydo/hydo.yaml in {}", cx.image)));
            let comp = hydo_engine::image::embedded_file(&img, "etc/hydo/compose.json")
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok());
            Space::from_text(&text, comp)
        } else {
            let img = cx.build.join(&a.image);
            let text =
                hydo_engine::image::debugfs_cat(&img, "etc/hydo/hydo.yaml").unwrap_or_else(|_| {
                    die(&format!(
                        "manifest_space: no /etc/hydo/hydo.yaml in {} (not a hydo-build image?)",
                        img.display()
                    ))
                });
            let comp = hydo_engine::image::embedded_compose(&img)
                .and_then(|t| serde_json::from_str(&t).ok());
            Space::from_text(&text, comp)
        };
        let sp = sp.unwrap_or_else(|e| die(&e));
        let img = if cx.arm() {
            cx.hvm_image()
        } else {
            cx.build.join(&a.image)
        };
        if cx.env.config.pool.scripted_engine.is_none() {
            agent_space(&img, a.schedule.as_deref());
        }
        eprintln!("fault space: {}", dumps_line(&sp.describe()));
        sp.apply(&mut cx.ex);
    }
    let m = cx.ex.space.as_ref().map(|s| s.m.clone());
    let guest = m
        .as_ref()
        .and_then(|m| m.get("guest"))
        .filter(|g| hydo_manifest::py::truthy(g));
    // pick_ram_mib
    let (src, v) = match a.ram_mib {
        Some(r) => ("--ram-mib", json!(r)),
        None => match &m {
            Some(_) => (
                "guest.ram_mib",
                guest
                    .and_then(|g| g.get("ram_mib"))
                    .cloned()
                    .unwrap_or(json!(1024)),
            ),
            None => ("default", json!(1024)),
        },
    };
    let (lo, hi) = (256, hydo_manifest::RAM_MIB_MAX as i64);
    match v.as_i64() {
        Some(r) if (lo..=hi).contains(&r) && !v.is_boolean() => cx.ram_mib = r,
        _ => die(&format!(
            "hydo explore: {src}={}: guest RAM must be a whole number of MiB in [{lo}, {hi}] on --backend {}",
            hydo_manifest::py::repr(&v),
            a.backend
        )),
    }
    // pick_cpu
    let cpu = match &a.cpu {
        Some(c) => json!(c),
        None => guest
            .and_then(|g| g.get("cpu"))
            .cloned()
            .unwrap_or(json!("v2")),
    };
    match cpu.as_str() {
        Some(c @ ("v2" | "v3")) => cx.cpu = c.to_string(),
        _ => die(&format!(
            "hydo explore: guest CPU {}: expected v2 or v3",
            hydo_manifest::py::repr(&cpu)
        )),
    }
    if cx.cpu == "v3" && cx.ex.backend != Backend::Hvx && !cx.kvm {
        die(&format!(
            "hydo explore: guest CPU v3 (AVX) is an x86_64 option; --backend {} cannot run it",
            a.backend
        ));
    }
    if cx.cpu != "v2" {
        eprintln!("guest CPU: {}", cx.cpu);
    }
    if cx.ram_mib != 1024 {
        eprintln!("guest RAM: {} MiB", cx.ram_mib);
    }
    cx.vcpus = a.vcpus;
    if !(1..=8).contains(&a.vcpus) {
        die("hydo explore: --vcpus must be 1..8 (hvx's HVX_MAX_VCPUS)");
    }
    if a.vcpus > 1 && cx.ex.backend == Backend::Hvm {
        die("hydo explore: --vcpus > 1 needs --backend hvx (the Mac VMM has one vCPU and no PMU clock)");
    }
    if a.turns && a.vcpus < 2 {
        die(
            "hydo explore: --turns needs --vcpus N > 1 (turn tokens on a one-vCPU VM are an ERROR)",
        );
    }
    cx.ex.turns = a.turns;
    cx.ex.bursts = a.bursts;
    if a.bursts && !a.turns {
        die("hydo explore: --bursts needs --turns (and --vcpus N > 1)");
    }
    if let Some(tm) = a.turn_margin {
        if a.vcpus < 2 || !(8..=(1 << 20)).contains(&tm) {
            die("hydo explore: --turn-margin needs --vcpus N > 1 and a value in [8, 2^20]");
        }
    }
    cx.ex.new_faults = a.new_faults;
    cx.ex.compose = !a.no_compose;
    cx.ex.knob_rule_any = a.knob_rule == "any";
    cx.ex.concurrency = a.concurrency;
    // Q= and B= name a knob and BUGGIFY: an image without them would refuse every run
    // (hydo_control::names), so the options are refused here, once
    if let Some(m) = m.as_ref() {
        let has_knob = |t: &str| {
            m.get("knobs").and_then(Value::as_array).is_some_and(|ks| {
                ks.iter()
                    .any(|k| k.get("token").and_then(Value::as_str) == Some(t))
            })
        };
        if a.concurrency && !has_knob("Q") {
            die("hydo explore: --concurrency draws Q=, and this image's manifest has no knob Q to take it (the workload's own commands decide how many requests run at once)");
        }
        if a.buggify && !m.get("buggify").is_some_and(hydo_manifest::py::truthy) {
            die("hydo explore: --buggify draws B=, and this image's manifest does not say buggify: true");
        }
    }
    cx.ex.more_faults = a.more_faults;
    cx.ex.storage_faults = a.storage_faults;
    // M194: kills, pauses and partitions of the manifest's groups
    let has_groups = m
        .as_ref()
        .and_then(|m| m.get("groups"))
        .and_then(Value::as_array)
        .is_some_and(|g| !g.is_empty());
    if a.cluster_faults && !has_groups {
        die("hydo explore: --cluster-faults draws faults of the manifest's groups:, and this image's manifest has none (declare the cluster's nodes as a group: groups: [{name:, services: [...]}])");
    }
    cx.ex.cluster_faults = a.cluster_faults;
    // M139: reactions (A=) from their own stream
    if let Some(r) = &a.reactive {
        cx.ex.react_fault = r == "fault" || r == "both";
        cx.ex.react_heal = r == "heal" || r == "both";
    }
    let ids = |v: &str| -> Vec<String> {
        v.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(String::from)
            .collect()
    };
    if let Some(v) = &a.react_events {
        cx.ex.react_events = ids(v);
    } else if m.is_some() && cx.ex.react_fault {
        die("hydo explore: --reactive fault|both on a manifest needs --react-events (the assertion ids of the system's SDK records to react to; the defaults are the stack's)");
    }
    if let Some(v) = &a.heal_events {
        cx.ex.heal_events = ids(v);
    } else if m.is_some() && cx.ex.react_heal {
        die("hydo explore: --reactive heal|both on a manifest needs --heal-events (the assertion ids that mean a client request made no progress; the defaults are the stack's)");
    }
    cx.ex.freeze_call = a.freeze_call;
    cx.ex.fc_nmax = a.fc_nmax;
    cx.ex.protocol_faults = a.protocol_faults;
    if a.protocol_faults && cx.ex.pf_targets.is_empty() {
        die("hydo explore: --protocol-faults needs an image whose manifest has fakes: or proxies: (and allows `protocol` in faults:, or has no faults: list)");
    }
    cx.ex.grpc_on = a.grpc_faults;
    cx.ex.grpc_targets = if a.grpc_faults {
        grpc_targets(m.as_ref())
    } else {
        Vec::new()
    };
    if a.grpc_faults && cx.ex.grpc_targets.is_empty() {
        die("hydo explore: --grpc-faults needs an image whose manifest has a proxy with protocol: grpc (and allows `protocol` in faults:, or has no faults: list)");
    }
    if a.freeze_call && cx.ex.fc_pairs.is_empty() {
        die("hydo explore: --freeze-call needs an image whose manifest lists `freeze` in faults: and has two services that talk");
    }
    if a.more_faults && cx.ex.backend == Backend::Hvm {
        die(&format!(
            "hydo explore: --more-faults draws cf= (a power cut at the k-th disk FLUSH), which only \
             the hvx engine applies (x86_64 Linux with hydo's kernel module); --backend {} cannot \
             (hydo guide capabilities); explore without it, for example --strategy process",
            if cx.kvm { "kvm" } else { "hvm" }
        ));
    }
    if a.more_faults && a.cf_range != "auto" {
        let r: Vec<i64> = a
            .cf_range
            .split('-')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        if r.len() != 2 {
            die(&format!(
                "hydo explore: --cf-range {}: lo-hi or auto",
                a.cf_range
            ));
        }
        cx.ex.cf_range = Some((r[0], r[1]));
    }
    cx.ex.preempt = a.preempt;
    cx.ex.pct_exits = a
        .pct_exits
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    cx.ex.buggify = a.buggify;
    cx.ex.timing = a.timing;
    cx.ex.tj_pm = a
        .tj_pm
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    cx
}

/// The refusal for an image whose manifest cannot be read (and no `--manifest`): the explorer
/// would otherwise explore the built-in fault space of hydo's stack example, whose services the
/// image does not have.
pub fn no_manifest(image: &str, why: &str) -> String {
    format!(
        "hydo: {image}: cannot read the image's manifest (/etc/hydo/hydo.yaml: {why}), so there \
         is no fault space to explore; every image hydo build makes carries one: build the \
         image with hydo build, or name its manifest with --manifest <hydo.yaml>"
    )
}

/// The image of a manifest's space (M126): an agent image, whose manifest must be one the
/// controller drives. Every fault kind of the space is driven since the clock seam (M122: `T=`,
/// `guest.clock_after_cut`) and BUGGIFY (`B=`, `--buggify`) landed. A shell-init image is refused
/// (Milestone 145).
fn agent_space(img: &Path, schedule: Option<&str>) {
    if let Err(e) = hydo_engine::image::control(img) {
        die(&format!("hydo: {e}"));
    }
    let plan = match hydo_control::agent::plan_of_image(img) {
        Ok(p) => p,
        Err(e) => die(&format!(
            "hydo: {}: an agent image whose manifest the host controller cannot drive yet: {e}",
            img.display()
        )),
    };
    // `--schedule`: a token naming what the image does not have is refused before any VM
    // (hydo_control::names), as `hydo run` refuses it
    if let Some(Ok(s)) = schedule.map(hydo_schedule::Schedule::parse) {
        if let Err(e) = hydo_control::names::check(&plan, &s) {
            die(&format!("hydo explore: --schedule: {e}"));
        }
        for w in hydo_control::names::step_warnings(&plan, &s) {
            eprintln!("hydo explore: warning: --schedule: {w}");
        }
    }
    eprintln!(
        "agent image: the host controller drives {} (/etc/hydo/control)",
        img.display()
    );
}

/// Build the engine and size the pool. The snapshot store: in memory when every worker's VM and
/// its in-memory S0/S1 fit in the available memory (hvx; the pool line says which; until M144
/// `HYDO_SNAPSHOTS=files|memory` decided, for the deleted C engine).
fn make_pool(cx: &Cx) -> Pool {
    let consoles = cx.out.join("consoles");
    let mut extra = Vec::new();
    if let Some(tm) = cx.a.turn_margin {
        extra.push(format!("{}={tm}", hydo_config::registry::HVX_TURN_MARGIN));
    }
    let (engine, spec): (Arc<dyn ControlledEngine + Send + Sync>, BootSpec) = if cx.kvm {
        let e = hydo_engine::kvm::Kvm::for_release(&cx.env.root, cx.env.config.env.clone());
        let image = cx.build.join(&cx.image);
        let mut spec = BootSpec {
            image,
            ram_mib: cx.ram_mib as u32,
            vcpus: 1,
            cpu: if cx.cpu == "v3" {
                CpuLevel::V3
            } else {
                CpuLevel::V2
            },
            kernel: PathBuf::new(),
            initrd: None,
            outdir: consoles,
            snapshots: SnapshotStore::Memory,
        };
        if cx.env.config.pool.scripted_engine.is_none() {
            // the KVM build of the image's recorded kernel, and its initramfs (hydo-kvm)
            let dirs = [
                cx.build.clone(),
                cx.env.root.join("hv").join("kvm").join("linux").join("out"),
            ];
            let b = hydo_engine::kvm::boot_files(
                &spec.image,
                &dirs,
                cx.env.config.kvm.kernel.as_deref(),
            )
            .unwrap_or_else(|e| die(&format!("hydo: {e}")));
            if let Some(n) = &b.note {
                eprintln!("hydo: {n}");
            }
            spec.kernel = b.kernel;
            spec.initrd = Some(b.initrd);
        }
        (Arc::new(e), spec)
    } else if cx.ex.backend == Backend::Hvm {
        let e = hvm::Hvm {
            hvmx: hvm::program(&cx.env.root, &cx.env.config.hvm),
            env: Vec::new(),
            config: cx.env.config.env.clone(),
        };
        let spec = BootSpec {
            image: cx.hvm_image(),
            ram_mib: cx.ram_mib as u32,
            vcpus: 1,
            cpu: CpuLevel::V2,
            kernel: cx
                .hvm_kernel
                .clone()
                .unwrap_or_else(|| cx.hvm_default_kernel()),
            initrd: None,
            outdir: consoles,
            snapshots: SnapshotStore::Files,
        };
        (Arc::new(e), spec)
    } else {
        let e = hvx::Hvx {
            engine: hvx::program(&cx.env.root, &cx.env.config.engine),
            hvx_run: Some(cx.env.config.paths.hvx_run.clone()),
            extra_env: extra,
            env: hvx::wrapper_env(&cx.env.config.engine),
            config: cx.env.config.env.clone(),
        };
        let spec = BootSpec {
            image: cx.build.join(&cx.image),
            ram_mib: cx.ram_mib as u32,
            vcpus: cx.vcpus as u32,
            cpu: if cx.cpu == "v3" {
                CpuLevel::V3
            } else {
                CpuLevel::V2
            },
            kernel: cx.build.join("bzImage"),
            initrd: Some(cx.build.join("initramfs-kafka.cpio.gz")),
            outdir: consoles,
            snapshots: SnapshotStore::Files,
        };
        // the kernel and initramfs the image's record names (M155)
        let mut spec = spec;
        if cx.env.config.pool.scripted_engine.is_none() {
            let b = hydo_engine::image::boot_files(
                &spec.image,
                &spec.kernel,
                spec.initrd.as_deref(),
                None,
                &[],
            )
            .unwrap_or_else(|e| die(&format!("hydo: {e}")));
            if let Some(n) = &b.note {
                eprintln!("hydo: {n}");
            }
            spec.kernel = b.kernel;
            spec.initrd = b.initrd;
        }
        (Arc::new(e), spec)
    };
    // Every image is an agent image driven by the host controller, also one explored without a
    // manifest space (the stack's schedules, as hydo-bench draws them, M129); one it cannot drive
    // (a shell-init image, or one without an embedded manifest) is refused with the controller's
    // message instead of every worker failing to start.
    let engine: Arc<dyn Engine + Send + Sync> = if let Some(p) = &cx.env.config.pool.scripted_engine
    {
        // the explorer's own tests (crate::scripted)
        Arc::new(crate::scripted::ScriptedEngine {
            program: p.clone(),
            kind: engine.kind(),
        })
    } else {
        match hydo_control::agent::AgentEngine::for_image(engine, &spec.image) {
            Ok(a) => Arc::new(a.with_control(&cx.env.config.control)),
            Err(e) => die(&format!("hydo: {e}")),
        }
    };
    let (n, spec) = size_pool(cx, engine.as_ref(), spec);
    let host = Host {
        outdir: cx.out.clone(),
        onboard: cx.env.root.join("hydo").join("onboard"),
        known: cx.known.clone(),
        pool: cx.env.config.pool.clone(),
    };
    Pool::new(
        engine,
        spec,
        n,
        host,
        cx.image.clone(),
        cx.engine_args().join(" "),
    )
}

/// The pool size and the snapshot store (docs/PERF-RUN-COST.md section 7): a worker holds its VM
/// (guest RAM plus the image copied into kernel memory) and, in memory, S0 and S1 (twice RAM and
/// the data partition, here estimated by the image size).
fn size_pool(cx: &Cx, engine: &dyn Engine, mut spec: BootSpec) -> (usize, BootSpec) {
    let img_mib = std::fs::metadata(&spec.image)
        .map(|m| m.len() >> 20)
        .unwrap_or(0);
    let ram = cx.ram_mib as u64;
    let per_files = ram + img_mib + 256;
    let per_mem = per_files + 2 * (ram + data_partition_mib(&spec.image).unwrap_or(img_mib));
    let avail = hydo_engine::available_mib();
    let hvx = cx.ex.backend == Backend::Hvx;
    let free = free_threads();
    let n = match cx.a.workers {
        Some(n) => n,
        None if hvx => hydo_engine::machine_workers(per_mem).min(free),
        None => engine.max_workers(&spec).min(free),
    };
    let fits = avail.is_some_and(|m| m >= n as u64 * per_mem);
    let memory = hvx && fits;
    // M179: a guest may have 8 GiB; say it before the workers fail to make their VMs
    if let Err(e) = hydo_engine::guest_fits(ram) {
        die(&format!("explore: {e}"));
    }
    // what one worker holds: its VM, and on hvx S0 and S1 in memory too (the hvx engine keeps
    // them there whatever the store says)
    let per = if hvx { per_mem } else { per_files };
    if let Some(av) = avail.filter(|&m| m < n as u64 * per) {
        eprintln!(
            "warning: {n} worker{} with {ram} MiB guests need{} up to about {} MiB, and this machine has \
             {av} MiB available: a worker may fail to make its VM (--workers {} fits)",
            if n == 1 { "" } else { "s" },
            if n == 1 { "s" } else { "" },
            n as u64 * per,
            (av / per.max(1)).max(1)
        );
    }
    if memory {
        spec.snapshots = SnapshotStore::Memory;
    }
    eprintln!(
        "pool: {n} worker{}{}, snapshots {}",
        if n == 1 { "" } else { "s" },
        if cx.a.workers.is_some() {
            ""
        } else {
            " (sized from this machine)"
        },
        if memory { "in memory" } else { "in files" }
    );
    (n.max(1), spec)
}

/// The size of a hydo image's data partition (the second MBR entry; hydo-build puts the root at
/// LBA 2048 and the data partition after it), which a worker keeps twice in memory (S0, S1).
fn data_partition_mib(image: &Path) -> Option<u64> {
    use std::io::Read;
    let mut mbr = [0u8; 512];
    std::fs::File::open(image).ok()?.read_exact(&mut mbr).ok()?;
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        return None;
    }
    let e = 446 + 16;
    let sectors = u32::from_le_bytes([mbr[e + 12], mbr[e + 13], mbr[e + 14], mbr[e + 15]]) as u64;
    (sectors > 0).then_some((sectors * 512) >> 20)
}

/// The hardware threads this pool may use: the performance cores on Apple Silicon (an efficiency
/// core runs a guest at a fraction of the speed), else every hardware thread; minus what the
/// machine is already running (the 1-minute load average: the test machines are shared by
/// several sessions, and a pool sized past them slows every run down). At least 1.
fn free_threads() -> usize {
    let mut threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    if cfg!(target_os = "macos") {
        if let Some(p) = std::process::Command::new("sysctl")
            .args(["-n", "hw.perflevel0.logicalcpu"])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<usize>()
                    .ok()
            })
        {
            threads = threads.min(p.max(1));
        }
    }
    let mut load = [0f64; 3];
    // SAFETY: getloadavg writes at most 3 doubles into the array it is given.
    let n = unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
    let busy = if n >= 1 { load[0].round() as usize } else { 0 };
    threads.saturating_sub(busy).max(1)
}

fn tag_items(cx: &Cx, seeds: &[i64], points: &[S]) -> Vec<(String, String)> {
    seeds
        .iter()
        .zip(points)
        .map(|(i, s)| {
            (
                format!("seed-{i}"),
                cx.render(&cx.ex.add_point_streams(s.clone(), *i as u64)),
            )
        })
        .collect()
}

fn run(mut cx: Cx) -> i32 {
    let _ = std::fs::create_dir_all(&cx.out);
    cx.ev = Events::create(&cx.out.join(hydo_results::EVENTS));
    let t0 = Instant::now();
    let pool = make_pool(&cx);
    let image_sha = cx.image_sha_thread();
    cx.ev.write(&cx.start_record(pool.workers()));
    if let Some(sch) = cx.a.schedule.clone() {
        let res = pool.run_all(&[("manual".to_string(), sch.clone())], true);
        let r = &res["manual"].1;
        println!("{}", dumps_line(&Value::Object(results::public(r))));
        let mut run = cx.run_record("manual", &sch, r, role::MANUAL);
        run.class = Some(cx.ex.finding_class(&cx.parse(&sch)).to_string());
        cx.ev.write(&run);
        let summary = Summary {
            schedules: 1,
            failures: (run.verdict.kind != "ok") as u64,
            total_wall_s: Some(round1(t0.elapsed().as_secs_f64())),
            ..Summary::default()
        };
        cx.ev.write(&End {
            summary: summary.clone(),
            ..End::default()
        });
        let doc = cx.exploration(pool.workers(), summary, image_sha);
        let _ = hydo_results::write_document(&cx.out.join(hydo_results::EXPLORATION_DOC), &doc);
        pool.close();
        return 0;
    }
    let (seeds, items, pre_res, fc_info, labels) = match &cx.a.sweep {
        Some(sw) => {
            if let Some(sp) = &cx.ex.space {
                if let Err(e) = sp.check_sweep(sw) {
                    pool.close();
                    die(&e);
                }
            }
            let (points, items, pre, info) = if sw == "freeze" && cx.a.fc_adaptive {
                match fc::adaptive(&cx.ex, &pool, &cx.a, &cx.out, &cx.ev) {
                    Ok(o) => {
                        let mut fs = Map::new();
                        fs.insert("summary".into(), Value::Object(o.info.clone()));
                        for (k, v) in o.doc {
                            fs.insert(k, v);
                        }
                        (o.points, Some(o.items), Some(o.res), Some((o.info, fs)))
                    }
                    Err(e) => {
                        pool.close();
                        die(&e)
                    }
                }
            } else {
                let p = if sw == "freeze" {
                    fc::freeze_sweep(&cx.ex, &pool, &cx.a, &cx.out, &cx.ev)
                } else {
                    cx.ex.sweep(sw)
                };
                match p {
                    Ok(p) => (p, None, None, None),
                    Err(e) => {
                        pool.close();
                        die(&e)
                    }
                }
            };
            let seeds: Vec<i64> = (1..=points.len() as i64).collect();
            let items = items.unwrap_or_else(|| {
                tag_items(
                    &cx,
                    &seeds,
                    &points.iter().map(|p| p.1.clone()).collect::<Vec<_>>(),
                )
            });
            let labels: Vec<String> = points.iter().map(|p| p.0.clone()).collect();
            if sw == "freeze" && cx.a.sweep_dry {
                let t: String = seeds
                    .iter()
                    .zip(&items)
                    .map(|(i, (_, t))| format!("seed-{i} {t}\n"))
                    .collect();
                let _ = std::fs::write(cx.out.join("points.txt"), t);
                pool.close();
                println!("{}", dumps_line(&json!({"points": points.len()})));
                return 0;
            }
            (seeds, items, pre, info, Some(labels))
        }
        None => {
            let (lo, hi) = cx
                .a
                .seeds
                .split_once('-')
                .and_then(|(l, h)| {
                    Some((l.trim().parse::<i64>().ok()?, h.trim().parse::<i64>().ok()?))
                })
                .unwrap_or_else(|| die(&format!("hydo explore: --seeds {}: lo-hi", cx.a.seeds)));
            let seeds: Vec<i64> = (lo..=hi).collect();
            if cx.ex.more_faults && cx.ex.cf_range.is_none() {
                let r = measure_cf_range(&cx, &pool);
                match r {
                    Some(r) => {
                        eprintln!("cf= range (measured with cfn): {}-{}", r.0, r.1);
                        cx.ex.cf_range = Some(r);
                    }
                    None => {
                        pool.close();
                        die("hydo explore: --cf-range auto: no FLUSH count from the cfn probe (see worker-*.err)");
                    }
                }
            }
            let items: Vec<(String, String)> = seeds
                .iter()
                .map(|sd| {
                    let s = cx
                        .ex
                        .compose_seed(&cx.a.strategy, *sd as u64, cx.a.durable_only);
                    (format!("seed-{sd}"), cx.render(&s))
                })
                .collect();
            (seeds, items, None, None, None)
        }
    };
    explore(
        &cx,
        pool,
        t0,
        Inputs {
            seeds,
            items,
            pre_res,
            fc_info,
            labels,
            image_sha,
        },
    )
}

/// explore.py `measure_cf_range` (the probe run goes into the stream).
fn measure_cf_range(cx: &Cx, pool: &Pool) -> Option<(i64, i64)> {
    let tag = "cfn-probe";
    let text = "disk=cut dseed=0 cfn | run";
    let res = pool.run_all(&[(tag.into(), text.into())], true);
    if let Some((_, r)) = res.get(tag) {
        cx.ev.write(&cx.run_record(tag, text, r, role::PROBE));
    }
    let re = regex::Regex::new(&format!(
        r"^FLUSHCUT {} target=\d+ flushes=(\d+)",
        regex::escape(tag)
    ))
    .ok()?;
    for i in 0..pool.workers() {
        let f = cx.out.join(format!("worker-{i}.err"));
        let text = std::fs::read(&f)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        for line in text.lines() {
            if let Some(c) = re.captures(line) {
                let n: i64 = c[1].parse().unwrap_or(0);
                if n > 1 {
                    return Some((1, n));
                }
            }
        }
    }
    None
}

/// One failure's record as the flow uses it (explore.py's dict: the run's result, seed,
/// schedule, class, assert_failures).
type Rec = Map<String, Value>;

fn record(cx: &Cx, sd: i64, text: &str, r: &mut Res) -> Rec {
    let tag = format!("seed-{sd}");
    let s = cx.parse(text);
    let mut rec = Map::new();
    rec.insert("seed".into(), json!(sd));
    rec.insert("schedule".into(), json!(text));
    rec.insert("class".into(), json!(cx.ex.finding_class(&s)));
    for (k, v) in r.iter() {
        rec.insert(k.clone(), v.clone());
    }
    let af = assert_failures(&cx.out, &tag, r);
    if !af.is_empty() && verdict_kind(&py_str(&r["verdict"])) == "ok" {
        let v = json!(format!(
            "VIOLATION assertion {}",
            af.iter().map(py_str).collect::<Vec<_>>().join(",")
        ));
        rec.insert("verdict".into(), v.clone());
        r.insert("verdict".into(), v);
    }
    rec.insert("assert_failures".into(), Value::Array(af));
    rec
}

/// What one shrink chain produced.
struct Shrunk {
    key: (String, String),
    rec: Rec,
    /// Every run the chain used: its candidates up to the kept ones, and the confirmations.
    log: Vec<Run>,
    /// The minimal schedule's two confirmation runs (tag, result).
    conf: [(String, Res); 2],
}

/// A shrink's test of a candidate's (verdict, result): does it still fail the same way?
type Keep<'a> = &'a dyn Fn(&str, &Res) -> bool;

/// Which shrink a run belongs to (its records' role, the signature shrunk, the seed).
struct Chain<'a> {
    role: &'a str,
    of: String,
    seed: i64,
}

impl Cx {
    fn chain_run(&self, ch: &Chain, tag: &str, text: &str, r: &Res, role: &str) -> Run {
        let mut run = self.run_record(tag, text, r, role);
        run.seed = Some(ch.seed);
        run
    }
}

/// explore.py `shrink`. Returns (minimal, its verdict, tries) and appends the runs it used.
#[allow(clippy::too_many_arguments)]
fn shrink(
    cx: &Cx,
    pool: &Pool,
    s: S,
    verdict: &str,
    log: &mut Vec<Run>,
    tjk: Option<(i64, i64)>,
    keep: Option<Keep<'_>>,
    prefix: &str,
    ch: &Chain,
) -> (S, String, i64) {
    let mut budget = 40;
    let (mut cur, mut cur_v, mut tries, mut rnd, mut tjk) = (s, verdict.to_string(), 0i64, 0, tjk);
    if cur.tj.is_some() {
        budget *= 3;
    }
    if cur.w.is_some() {
        budget *= 2;
    }
    while tries < budget {
        let cands = cx.ex.candidates(&cur, tjk);
        if cands.is_empty() {
            break;
        }
        rnd += 1;
        let items: Vec<(String, String)> = cands
            .iter()
            .enumerate()
            .map(|(i, c)| (format!("{prefix}-{rnd}-{i}"), cx.render(c)))
            .collect();
        tries += items.len() as i64;
        // run the round's candidates side by side, but decide in candidate order and stop as soon
        // as the first one that keeps the failure is known (explore.py ran the whole round and then
        // took the first one that kept it: the same candidate)
        let mut b = pool.batch(&items, true);
        let mut got: HashMap<String, Res> = HashMap::new();
        let mut next = 0;
        let mut progressed = false;
        'round: while let Some((tag, _, r)) = b.next_done() {
            got.insert(tag, r);
            while next < items.len() {
                let Some(r) = got.get(&items[next].0) else {
                    break;
                };
                let v = py_str(&r["verdict"]);
                let kept = match keep {
                    Some(k) => k(&v, r),
                    None => same_failure(&cur_v, &v),
                };
                let mut run = cx.chain_run(ch, &items[next].0, &items[next].1, r, ch.role);
                run.shrink = Some(ShrinkStep {
                    of: ch.of.clone(),
                    round: rnd as u64,
                    kept,
                });
                log.push(run);
                if kept {
                    cur = cands[next].clone();
                    cur_v = v;
                    if let Some(Value::Array(t)) = r.get("tjk") {
                        tjk = Some((t[0].as_i64().unwrap_or(0), t[1].as_i64().unwrap_or(0)));
                    }
                    progressed = true;
                    b.cancel();
                    break 'round;
                }
                next += 1;
            }
        }
        if !progressed {
            break;
        }
    }
    (cur, cur_v, tries)
}

fn shrink_violation(cx: &Cx, pool: &Pool, k: usize, key: (String, String), f: &Rec) -> Shrunk {
    let (prefix, ma, mb) = if k == 0 {
        (
            "shrink".to_string(),
            "min-a".to_string(),
            "min-b".to_string(),
        )
    } else {
        (
            format!("shrink{}", k + 1),
            format!("min{}-a", k + 1),
            format!("min{}-b", k + 1),
        )
    };
    let ch = Chain {
        role: role::SHRINK,
        of: key.1.clone(),
        seed: f["seed"].as_i64().unwrap_or(0),
    };
    let mut log = Vec::new();
    let tjk = f
        .get("tjk")
        .and_then(Value::as_array)
        .map(|t| (t[0].as_i64().unwrap_or(0), t[1].as_i64().unwrap_or(0)));
    let (mut m, mv, n) = shrink(
        cx,
        pool,
        cx.parse(&py_str(&f["schedule"])),
        &py_str(&f["verdict"]),
        &mut log,
        tjk,
        None,
        &prefix,
        &ch,
    );
    let mut mt = cx.render(&m);
    let conf_pair = |t: &str, log: &mut Vec<Run>| {
        let r = pool.run_all(
            &[(ma.clone(), t.to_string()), (mb.clone(), t.to_string())],
            true,
        );
        for tag in [&ma, &mb] {
            log.push(cx.chain_run(&ch, tag, t, &r[tag].1, role::MINIMAL));
        }
        [
            (ma.clone(), r[&ma].1.clone()),
            (mb.clone(), r[&mb].1.clone()),
        ]
    };
    let mut stable: Option<Map<String, Value>> = None;
    let conf;
    let meta = cx.compose_meta();
    match (&m.w, meta) {
        (Some(w), Some(meta)) => match compose::stable_w(w, meta) {
            Ok(sw) => {
                let mut st_map = Map::new();
                st_map.insert("stable".into(), json!(sw));
                let mut ms = m.clone();
                ms.w = Some(sw.clone());
                let st = cx.render(&ms);
                let mut c = None;
                if compose::fits(&st) {
                    let [ca, cb] = conf_pair(&st, &mut log);
                    if same_failure(&mv, &py_str(&ca.1["verdict"])) && ca.1["fp"] == cb.1["fp"] {
                        m = ms;
                        mt = st.clone();
                        st_map.insert("stable_schedule".into(), json!(st));
                        c = Some([ca, cb]);
                    } else {
                        st_map.insert(
                            "stable_note".into(),
                            json!(format!(
                                "the resolved composition did not fail the same way ({}); the minimal schedule keeps the seed form",
                                head(&py_str(&ca.1["verdict"]), 120)
                            )),
                        );
                        st_map.insert("stable_schedule".into(), json!(st));
                    }
                } else {
                    let key = compose::key_of(&sw);
                    let mut ma2 = m.clone();
                    ma2.w = Some(format!("@{key}"));
                    st_map.insert("stable_schedule".into(), json!(cx.render(&ma2)));
                    st_map.insert(
                        "stable_note".into(),
                        json!(format!(
                            "the resolved composition is too long for a schedule line; a saved regression carries it as W=@{key} (unconfirmed)"
                        )),
                    );
                }
                conf = c.unwrap_or_else(|| conf_pair(&mt, &mut log));
                stable = Some(st_map);
            }
            Err(e) => {
                eprintln!("hydo-explore: the composition of {mt}: {e}");
                conf = conf_pair(&mt, &mut log);
            }
        },
        _ => conf = conf_pair(&mt, &mut log),
    }
    let mut rec = Map::new();
    rec.insert("from_seed".into(), f["seed"].clone());
    rec.insert("minimal".into(), json!(mt));
    rec.insert("verdict".into(), json!(mv));
    rec.insert("tries".into(), json!(n));
    rec.insert("min_class".into(), json!(cx.ex.finding_class(&m)));
    rec.insert("min_verdict".into(), conf[0].1["verdict"].clone());
    if let (Some(st), Some(meta)) = (stable, meta) {
        let w = m.w.clone().unwrap_or_default();
        rec.insert(
            "composition".into(),
            json!(compose::describe(&w, meta).unwrap_or_default()),
        );
        rec.insert(
            "commands".into(),
            Value::Array(
                compose::commands(meta)
                    .into_iter()
                    .map(|c| json!(c.0))
                    .collect(),
            ),
        );
        let sw = py_str(&st["stable"]);
        rec.insert(
            "contents".into(),
            Value::Object(compose::contents_of(&sw, meta).unwrap_or_default()),
        );
        for (k, v) in st {
            rec.insert(k, v);
        }
    }
    Shrunk {
        key,
        rec,
        log,
        conf,
    }
}

/// explore.py `errlog_shrink` for one kind.
fn shrink_errkind(cx: &Cx, pool: &Pool, x: &errlog::Aggregate) -> (String, ErrorShrink, Vec<Run>) {
    let sig = x.sig.clone();
    let mut log = Vec::new();
    let keep = |_: &str, r: &Res| {
        r.get("errors_new")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|s| py_str(s) == sig))
    };
    let ch = Chain {
        role: role::ERROR_SHRINK,
        of: sig.clone(),
        seed: x.first_new.as_ref().and_then(|f| f.0.as_i64()).unwrap_or(0),
    };
    let s0 = cx.parse(
        x.first_new
            .as_ref()
            .map(|f| f.1.as_str())
            .unwrap_or_default(),
    );
    let (m, _, n) = shrink(
        cx,
        pool,
        s0,
        "",
        &mut log,
        None,
        Some(&keep),
        &format!("errshrink-{sig}"),
        &ch,
    );
    let mt = cx.render(&m);
    let (ta, tb) = (format!("errmin-a-{sig}"), format!("errmin-b-{sig}"));
    let conf = pool.run_all(&[(ta.clone(), mt.clone()), (tb.clone(), mt.clone())], true);
    let mut confirmations = Vec::new();
    for t in [&ta, &tb] {
        let r = &conf[t].1;
        log.push(cx.chain_run(&ch, t, &mt, r, role::ERROR_MINIMAL));
        confirmations.push(Reproduction {
            tag: Some(t.clone()),
            fp: r.get("fp").filter(|v| !v.is_null()).map(py_str),
            verdict: r.get("verdict").filter(|v| !v.is_null()).map(py_str),
            present: Some(keep("", r)),
        });
    }
    let es = ErrorShrink {
        identical: conf[&ta].1.get("fp") == conf[&tb].1.get("fp"),
        repro_command: Some(cx.by_hand(&mt)),
        minimal: mt,
        tries: n as u64,
        confirmations,
    };
    (sig, es, log)
}

/// What the main flow runs: the first runs and, for sweeps, what the sweep found already.
struct Inputs {
    seeds: Vec<i64>,
    items: Vec<(String, String)>,
    pre_res: Option<HashMap<String, (String, Res)>>,
    /// The adaptive freeze sweep's counts (as summary rows) and its whole record.
    fc_info: Option<(Map<String, Value>, Map<String, Value>)>,
    labels: Option<Vec<String>>,
    image_sha: ShaThread,
}

fn repro_of(tag: &str, r: Option<&Res>) -> Reproduction {
    let g = |k: &str| {
        r.and_then(|r| r.get(k))
            .filter(|v| !v.is_null())
            .map(py_str)
    };
    Reproduction {
        tag: Some(tag.to_string()),
        fp: g("fp"),
        verdict: g("verdict"),
        present: None,
    }
}

fn explore(cx: &Cx, pool: Pool, t0: Instant, inp: Inputs) -> i32 {
    let Inputs {
        seeds,
        items,
        pre_res,
        fc_info,
        labels,
        image_sha,
    } = inp;
    let a = &cx.a;
    let nrep = a.repro.unwrap_or(if a.sweep.as_deref() == Some("freeze") {
        2
    } else {
        1
    });
    let pos: HashMap<String, usize> = items
        .iter()
        .enumerate()
        .map(|(i, (t, _))| (t.clone(), i))
        .collect();
    let mut slot: Vec<Option<(String, Res)>> = vec![None; items.len()];
    let mut fails: Vec<(Rec, Run)> = Vec::new();
    let mut reps: Vec<(String, String)> = Vec::new();
    let mut t_sweep = 0.0;
    let mut rep: HashMap<String, Res> = HashMap::new();
    let (shrunk, errors, res_all) = std::thread::scope(|sc| {
        let pool = &pool;
        let mut chains = Vec::new();
        let mut repro_batches = Vec::new();
        let ts = Instant::now();
        let mut next = 0;
        let mut batch = if pre_res.is_none() {
            Some(pool.batch(&items, false))
        } else {
            None
        };
        let mut pre_iter = pre_res
            .map(|m| m.into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter();
        loop {
            let d = match &mut batch {
                Some(b) => b.next_done(),
                None => pre_iter.next().map(|(t, (x, r))| (t, x, r)),
            };
            let Some((tag, text, r)) = d else { break };
            if let Some(&i) = pos.get(&tag) {
                slot[i] = Some((text, r));
            }
            // finalize every seed whose predecessors are all in, in seed order
            while next < slot.len() {
                let Some((text, r)) = slot[next].as_mut() else {
                    break;
                };
                let sd = seeds[next];
                let rec = record(cx, sd, text, r);
                let mut run = cx.run_record(&format!("seed-{sd}"), text, r, role::FIRST);
                run.seed = Some(sd);
                run.class = Some(py_str(&rec["class"]));
                run.point = labels.as_ref().and_then(|l| l.get(next).cloned());
                cx.ev.write(&run);
                let v = py_str(&rec["verdict"]);
                if verdict_kind(&v) != "ok" {
                    let ritems: Vec<(String, String)> = (0..nrep)
                        .map(|k| {
                            let tag = if k == 0 {
                                format!("repro-{sd}")
                            } else {
                                format!("repro{}-{sd}", k + 1)
                            };
                            (tag, py_str(&rec["schedule"]))
                        })
                        .collect();
                    repro_batches.push(pool.batch(&ritems, true));
                    let key = (py_str(&rec["class"]), sig_key(&v));
                    if !reps.contains(&key) {
                        reps.push(key.clone());
                        if !a.no_shrink && verdict_kind(&v) == "violation" {
                            let k = chains.len();
                            let f = rec.clone();
                            chains.push(sc.spawn(move || shrink_violation(cx, pool, k, key, &f)));
                        }
                    }
                    fails.push((rec, run));
                }
                next += 1;
            }
        }
        t_sweep = match &fc_info {
            Some((i, _)) => {
                i["stage1_wall_s"].as_f64().unwrap_or(0.0)
                    + i["stage2_wall_s"].as_f64().unwrap_or(0.0)
            }
            None => ts.elapsed().as_secs_f64(),
        };
        let mut all: HashMap<String, (String, Res)> = HashMap::new();
        for (i, s) in slot.iter().enumerate() {
            if let Some(x) = s {
                all.insert(items[i].0.clone(), x.clone());
            }
        }
        // error-log kinds: collected once every first run is in, shrunk side by side
        let an_runs: Vec<(Value, String, Option<errlog::Analysis>)> = seeds
            .iter()
            .map(|sd| {
                let tag = format!("seed-{sd}");
                let (text, r) = all.get(&tag).cloned().unwrap_or_default();
                // explore.py read back consoles/<tag>.errors.json; the analysis is a pure function
                // of the consoles, the schedule and the list, so it is made again here
                let an = if has_errlog(&r) {
                    cx.known.as_ref().and_then(|k| {
                        errlog::analyse(&cx.out.join("consoles").join(&tag), &text, k)
                    })
                } else {
                    None
                };
                (json!(sd), text, an)
            })
            .collect();
        let errors = if an_runs.iter().any(|x| x.2.is_some()) {
            let agg = hydo_errors::collect(&an_runs);
            let mut handles = Vec::new();
            if !a.no_shrink && a.errors_shrink > 0 {
                for x in errlog::to_shrink(&agg)
                    .into_iter()
                    .take(a.errors_shrink as usize)
                {
                    handles.push(sc.spawn(move || shrink_errkind(cx, pool, &x)));
                }
            }
            Some((agg, an_runs, handles))
        } else {
            None
        };
        for mut b in repro_batches {
            while let Some((tag, _, r)) = b.next_done() {
                rep.insert(tag, r);
            }
        }
        let shrunk: Vec<Shrunk> = chains
            .into_iter()
            .map(|h| h.join().expect("a shrink"))
            .collect();
        let errors = errors.map(|(agg, an_runs, hs)| {
            let done: Vec<(String, ErrorShrink, Vec<Run>)> = hs
                .into_iter()
                .map(|h| h.join().expect("a shrink"))
                .collect();
            (agg, an_runs, done)
        });
        (shrunk, errors, all)
    });
    // the re-runs, in seed order
    let mut failures = Vec::new();
    for (f, run) in &fails {
        let sd = f["seed"].as_i64().unwrap_or(0);
        let mut reproductions = Vec::new();
        let mut ok = true;
        for k in 0..nrep {
            let tag = if k == 0 {
                format!("repro-{sd}")
            } else {
                format!("repro{}-{sd}", k + 1)
            };
            let rr = rep.get(&tag);
            // an agent image's re-run also types the same commands (ctl=, M126); None == None otherwise
            ok = ok
                && rr.and_then(|r| r.get("fp")) == f.get("fp")
                && rr.and_then(|r| r.get("ctl")) == f.get("ctl");
            if let Some(r) = rr {
                let mut rr_run = cx.run_record(&tag, &run.schedule, r, role::RERUN);
                rr_run.seed = Some(sd);
                cx.ev.write(&rr_run);
            }
            reproductions.push(repro_of(&tag, rr));
        }
        failures.push(Failure {
            signature: sig_key(&run.verdict.text),
            reproduced: ok,
            reproductions,
            timing_only: None,
            run: run.clone(),
        });
    }
    // the shrinks' runs, chain by chain (representatives by first seed, then the error kinds)
    for s in &shrunk {
        for r in &s.log {
            cx.ev.write(r);
        }
    }
    let mut eshr: HashMap<String, ErrorShrink> = HashMap::new();
    if let Some((_, _, done)) = &errors {
        for (sig, es, log) in done {
            for r in log {
                cx.ev.write(r);
            }
            eshr.insert(sig.clone(), es.clone());
        }
    }
    let shrinks: Vec<hydo_results::Shrink> = shrunk.iter().map(|s| cx.shrink_model(s)).collect();
    for s in &shrinks {
        cx.ev.write(s);
    }
    let boot = pool.mean_boot();
    let nworkers = pool.workers();
    let cache_hits = *pool.cache_hits.lock().expect("lock");
    pool.close();
    let total = t0.elapsed().as_secs_f64();
    let secs: Vec<f64> = seeds
        .iter()
        .map(|sd| {
            res_all
                .get(&format!("seed-{sd}"))
                .and_then(|x| x.1.get("secs"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
        })
        .collect();
    let mut summary = Summary {
        schedules: seeds.len() as u64,
        sweep_wall_s: Some(round1(t_sweep)),
        runs_per_hour: Some(round1(seeds.len() as f64 / t_sweep.max(1e-9) * 3600.0)),
        mean_run_s: Some(round1(secs.iter().sum::<f64>() / secs.len().max(1) as f64)),
        worker_boot_s: Some(round1(boot)),
        total_wall_s: Some(round1(total)),
        failures: failures.len() as u64,
        reproduced: Some(failures.iter().filter(|f| f.reproduced).count() as u64),
        repro_runs: (nrep != 1).then_some(nrep as u64),
        quiet_wait_s: {
            let qw = pool::quiet_waited();
            (qw > 0.0).then(|| round1(qw))
        },
        cache_hits: (cache_hits > 0).then_some(cache_hits as u64),
        ..Summary::default()
    };
    // first runs that passed but may have checked nothing (results::vacuous)
    let vacuous = seeds
        .iter()
        .filter(|sd| {
            let tag = format!("seed-{sd}");
            res_all.get(&tag).is_some_and(|x| {
                let r = results::run_of(&tag, "", &x.1, &results::explorer_consoles(&cx.out, &tag));
                !r.warnings.is_empty()
            })
        })
        .count() as u64;
    if vacuous > 0 {
        summary.vacuous_passes = Some(vacuous);
        eprintln!(
            "hydo: warning: {vacuous} passing first runs may have checked nothing (the workload \
             acknowledged nothing, or a history was empty): their run records in events.jsonl \
             have warnings"
        );
    }
    let mut error_kinds = None;
    if let Some((agg, an_runs, _)) = &errors {
        summary.error_kinds_new = Some(agg.iter().filter(|x| x.new_runs > 0).count() as u64);
        summary.runs_with_new_error_kinds = Some(
            an_runs
                .iter()
                .filter(|x| x.2.as_ref().is_some_and(|an| !an.new.is_empty()))
                .count() as u64,
        );
        let known = cx.known.clone().expect("loaded");
        error_kinds = Some(ErrorKinds {
            known_file: known.path.clone(),
            known_sha256: known.sha256.clone(),
            unknown: known.unknown.clone(),
            kinds: agg
                .iter()
                .map(|x| x.to_model(eshr.get(&x.sig).cloned()))
                .collect(),
        });
    }
    cx.ev.write(&End {
        summary: summary.clone(),
        ..End::default()
    });
    let mut doc = cx.exploration(nworkers, summary, image_sha);
    doc.freeze_sweep = fc_info.map(|x| x.1);
    doc.failures = failures;
    doc.shrinks = shrinks;
    doc.error_kinds = error_kinds;
    // the property table and the liveness summary over the first runs
    let merged: Vec<(String, String)> = seeds
        .iter()
        .map(|sd| {
            let tag = format!("seed-{sd}");
            let r = res_all.get(&tag).map(|x| x.1.clone()).unwrap_or_default();
            let m = merged_text(&cx.out, &tag, &r);
            (tag, m)
        })
        .collect();
    doc.properties =
        results::property_table(&merged.iter().map(|x| x.1.clone()).collect::<Vec<_>>());
    let mut with: Vec<(String, String)> = merged
        .into_iter()
        .filter(|x| x.1.contains("HYDO-ASSERT"))
        .collect();
    with.sort_by(|a, b| format!("{}.log", a.0).cmp(&format!("{}.log", b.0)));
    doc.liveness = results::liveness(&with);
    let _ = hydo_results::write_document(&cx.out.join(hydo_results::EXPLORATION_DOC), &doc);
    let _ = std::fs::write(cx.out.join("report.md"), md::report(&doc));
    println!("{}", md::summary_line(&doc));
    0
}

fn merged_text(out: &Path, tag: &str, r: &Res) -> String {
    report::merged(out, tag, r)
}

/// The image's SHA-256, computed beside the runs (an image can be gigabytes).
type ShaThread = Option<std::thread::JoinHandle<Option<String>>>;

impl Cx {
    fn run_record(&self, tag: &str, text: &str, r: &Res, role: &str) -> Run {
        results::explorer_run(&self.out, tag, text, r, role)
    }

    fn image_file(&self) -> PathBuf {
        if self.arm() {
            self.hvm_image()
        } else {
            self.build.join(&self.image)
        }
    }

    fn image_sha_thread(&self) -> ShaThread {
        let p = self.image_file();
        Some(std::thread::spawn(move || {
            hydo_engine::image::sha256_file(&p).ok()
        }))
    }

    fn mode(&self) -> &'static str {
        if self.a.schedule.is_some() {
            "schedule"
        } else if self.a.sweep.is_some() {
            "sweep"
        } else {
            "seeds"
        }
    }

    fn config(&self) -> Config {
        let a = &self.a;
        let mode = self.mode();
        Config {
            strategy: a
                .sweep
                .as_ref()
                .map(|s| format!("sweep:{s}"))
                .unwrap_or_else(|| a.strategy.clone()),
            seeds: (mode == "seeds").then(|| a.seeds.clone()),
            sweep: a.sweep.clone(),
            branch_marker: None,
            variants: None,
            schedule: a.schedule.clone(),
            argv: self.argv.clone(),
        }
    }

    fn engine_info(&self, workers: usize) -> hydo_results::Engine {
        let hvm_b = self.arm();
        let guest_kernel = if hvm_b {
            hvm::image_kernel(&self.hvm_image()).map(|(_, sha)| {
                let k = self
                    .hvm_kernel
                    .clone()
                    .unwrap_or_else(|| self.hvm_default_kernel());
                format!(
                    "{} (sha256 {})",
                    k.file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    head(&sha, 16)
                )
            })
        } else {
            None
        };
        hydo_results::Engine {
            backend: Some(
                if hvm_b {
                    "hvm"
                } else if self.kvm {
                    "kvm"
                } else {
                    "hvx"
                }
                .into(),
            ),
            args: Some(self.engine_args()),
            ram_mib: Some(self.ram_mib as u64),
            vcpus: Some(self.vcpus as u64),
            cpu: Some(self.cpu.clone()),
            guest_kernel,
            module_srcversion: if hvm_b || self.kvm {
                None
            } else {
                std::fs::read_to_string("/sys/module/hvx/srcversion")
                    .ok()
                    .map(|s| s.trim().to_string())
            },
            workers: Some(workers as u64),
        }
    }

    fn image_info(&self, sha: Option<String>) -> Image {
        crate::results::image_of(&self.image, &self.image_file(), sha)
    }

    fn start_record(&self, workers: usize) -> Start {
        Start {
            hydo_version: Some(env!("CARGO_PKG_VERSION").into()),
            epoch: crate::results::this_epoch(),
            mode: self.mode().into(),
            config: self.config(),
            image: self.image_info(None),
            engine: self.engine_info(workers),
            ..Start::default()
        }
    }

    fn exploration(&self, workers: usize, summary: Summary, sha: ShaThread) -> Exploration {
        let sha = sha.and_then(|h| h.join().ok()).flatten();
        Exploration {
            hydo_version: Some(env!("CARGO_PKG_VERSION").into()),
            epoch: crate::results::this_epoch(),
            mode: self.mode().into(),
            config: self.config(),
            image: self.image_info(sha),
            engine: self.engine_info(workers),
            summary,
            artifacts: ExplorationArtifacts {
                events: hydo_results::EVENTS.into(),
                report: (self.mode() != "schedule").then(|| "report.md".into()),
                consoles: Some("consoles".into()),
            },
            ..Exploration::default()
        }
    }

    fn shrink_model(&self, s: &Shrunk) -> hydo_results::Shrink {
        let r = &s.rec;
        let st = |k: &str| r.get(k).filter(|v| !v.is_null()).map(py_str);
        let minimal = py_str(&r["minimal"]);
        let confirmations: Vec<Reproduction> =
            s.conf.iter().map(|(t, x)| repro_of(t, Some(x))).collect();
        hydo_results::Shrink {
            class: s.key.0.clone(),
            signature: s.key.1.clone(),
            from_seed: r["from_seed"].as_i64().unwrap_or(0),
            verdict: py_str(&r["verdict"]),
            tries: r["tries"].as_u64().unwrap_or(0),
            min_class: st("min_class"),
            min_verdict: st("min_verdict"),
            identical: s.conf[0].1.get("fp") == s.conf[1].1.get("fp"),
            confirmations,
            repro_command: Some(self.by_hand(&minimal)),
            composition: st("composition"),
            commands: r
                .get("commands")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(py_str).collect()),
            contents: r.get("contents").and_then(Value::as_object).cloned(),
            stable: st("stable"),
            stable_schedule: st("stable_schedule"),
            stable_note: st("stable_note"),
            minimal,
            ..hydo_results::Shrink::default()
        }
    }
}
