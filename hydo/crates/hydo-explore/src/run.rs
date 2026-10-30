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

use crate::args::{self, Args};
use crate::compose;
use crate::errlog::{self, Known};
use crate::fc;
use crate::gen::{Backend, Ex, TJ_MAX};
use crate::pool::{self, errlog_of, Host, Pool, Res};
use crate::py::{dumps, dumps_line, fl, head, py_str, round1};
use crate::report::{self, assert_failures, cell, same_failure, sig_key, verdict_kind};
use crate::sched::S;
use crate::space::{grpc_targets, Space};
use hydo_engine::{hvm, hvx, BootSpec, CpuLevel, Engine, SnapshotStore};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Where the explorer runs: the repository or release root (hydo/, hv/), and whether it was
/// started by the `hydo` command (the reports' "by hand" lines then name `hydo run`).
#[derive(Clone, Debug)]
pub struct Env {
    pub root: PathBuf,
    pub cli: bool,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn env_or(k: &str, d: PathBuf) -> PathBuf {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(d)
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
    known: Option<Known>,
}

impl Cx {
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
        if self.cpu == "v3" && self.ex.backend == Backend::Hvx {
            a.push("--cpu".into());
            a.push("v3".into());
        }
        a
    }

    fn hvm_repro_line(&self, sched: &str) -> String {
        let img = self.hvm_image();
        let env = hvm::disk_for(&img)
            .map(|d| format!("HVM_DISK={} ", d.display()))
            .unwrap_or_default();
        format!(
            "{}{} {}one {} {} <outdir> \"{}\"",
            env,
            self.hvm_dir().join("hvmx").display(),
            self.ram_args()
                .iter()
                .map(|x| format!("{x} "))
                .collect::<String>(),
            self.hvm_kernel
                .clone()
                .unwrap_or_else(|| self.hvm_default_kernel())
                .display(),
            img.display(),
            sched
        )
    }

    /// explore.py `by_hand`: the command line that runs one schedule.
    fn by_hand(&self, sched: &str) -> String {
        if self.ex.backend == Backend::Hvm {
            return format!("`{}`", self.hvm_repro_line(sched));
        }
        if self.env.cli {
            return format!(
                "`hydo run --image {} \"{}\"`",
                self.build.join(&self.image).display(),
                sched
            );
        }
        format!(
            "`~/bin/hvx-run sudo ./hvx_explore {}one ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz ~/hydo/build/{} <outdir> \"{}\"`",
            self.engine_args().iter().map(|x| format!("{x} ")).collect::<String>(),
            self.image,
            sched
        )
    }

    fn render(&self, s: &S) -> String {
        self.ex.render(s)
    }

    fn parse(&self, t: &str) -> S {
        self.ex
            .parse(t)
            .unwrap_or_else(|e| die(&format!("explore.py: cannot read the schedule {t:?}: {e}")))
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
            eprintln!("{}\nexplore.py: error: {e}", args::USAGE);
            return 2;
        }
    };
    let cx = setup(a, env);
    run(cx)
}

fn setup(a: Args, env: &Env) -> Cx {
    let onboard = env.root.join("hydo").join("onboard");
    let mut known = match &a.known_errors {
        None => Known {
            v: json!({"path": null, "sha256": null, "unknown": "report", "known": []}),
            path: None,
        },
        Some(p) => Known::load(&onboard, Some(p))
            .unwrap_or_else(|e| die(&format!("explore.py: --known-errors: {e}"))),
    };
    if let Some(e) = &a.errors {
        known.set_unknown(e);
    }
    if a.fc_adaptive && a.sweep.as_deref() != Some("freeze") {
        die("explore.py: --fc-adaptive goes with --sweep freeze");
    }
    let ex = Ex {
        backend: if a.backend == "hvm" {
            Backend::Hvm
        } else {
            Backend::Hvx
        },
        ..Ex::default()
    };
    let build = env_or("HYDO_BUILD", home().join("hydo").join("build"));
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
        known: Some(known),
        a,
    };
    let a = cx.a.clone();
    if cx.ex.backend == Backend::Hvm {
        let img = cx.hvm_image();
        cx.ex.hvm_disk = hvm::disk_for(&img).is_some();
        // explore.py stopped here with a traceback for a file that is not an arm64 initramfs
        cx.ex.hvm_tc = hvm::image_has_tc(&img).unwrap_or_else(|e| {
            die(&format!(
                "hydo: {}: not a hydo arm64 image ({e}; no etc/hydo/manifest.sh)",
                img.display()
            ))
        });
        if hvm::image_kernel(&img).is_some() {
            let k = hvm::kernel_for(&img, cx.hvm_kernel.as_deref(), &cx.hvm_default_kernel())
                .unwrap_or_else(|e| die(&e));
            eprintln!("guest kernel: {} (recorded in the image)", k.display());
            cx.hvm_kernel = Some(k);
        }
    }
    let base = Path::new(&a.image)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if a.manifest.is_some() || a.image.starts_with("explore-ob-") || base.starts_with("explore-ob-")
    {
        let sp = if let Some(m) = &a.manifest {
            Space::from_path(Path::new(m))
        } else if cx.ex.backend == Backend::Hvm {
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
    let (lo, hi) = if cx.ex.backend == Backend::Hvx {
        (256, 3072)
    } else {
        (256, 4096)
    };
    match v.as_i64() {
        Some(r) if (lo..=hi).contains(&r) && !v.is_boolean() => cx.ram_mib = r,
        _ => die(&format!(
            "explore.py: {src}={}: guest RAM must be a whole number of MiB in [{lo}, {hi}] on --backend {}",
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
            "explore.py: guest CPU {}: expected v2 or v3",
            hydo_manifest::py::repr(&cpu)
        )),
    }
    if cx.cpu == "v3" && cx.ex.backend != Backend::Hvx {
        die(&format!(
            "explore.py: guest CPU v3 (AVX) is an x86_64 option; --backend {} cannot run it",
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
        die("explore.py: --vcpus must be 1..8 (hvx's HVX_MAX_VCPUS)");
    }
    if a.vcpus > 1 && cx.ex.backend == Backend::Hvm {
        die("explore.py: --vcpus > 1 needs --backend hvx (the Mac VMM has one vCPU and no PMU clock)");
    }
    if a.turns && a.vcpus < 2 {
        die("explore.py: --turns needs --vcpus N > 1 (turn tokens on a one-vCPU VM are an ERROR)");
    }
    cx.ex.turns = a.turns;
    cx.ex.bursts = a.bursts;
    if a.bursts && !a.turns {
        die("explore.py: --bursts needs --turns (and --vcpus N > 1)");
    }
    if let Some(tm) = a.turn_margin {
        if a.vcpus < 2 || !(8..=(1 << 20)).contains(&tm) {
            die("explore.py: --turn-margin needs --vcpus N > 1 and a value in [8, 2^20]");
        }
    }
    cx.ex.new_faults = a.new_faults;
    cx.ex.compose = !a.no_compose;
    cx.ex.knob_rule_any = a.knob_rule == "any";
    cx.ex.concurrency = a.concurrency;
    cx.ex.more_faults = a.more_faults;
    cx.ex.freeze_call = a.freeze_call;
    cx.ex.fc_nmax = a.fc_nmax;
    cx.ex.protocol_faults = a.protocol_faults;
    if a.protocol_faults && cx.ex.pf_targets.is_empty() {
        die("explore.py: --protocol-faults needs an image whose manifest has fakes: or proxies: (and allows `protocol` in faults:, or has no faults: list)");
    }
    cx.ex.grpc_on = a.grpc_faults;
    cx.ex.grpc_targets = if a.grpc_faults {
        grpc_targets(m.as_ref())
    } else {
        Vec::new()
    };
    if a.grpc_faults && cx.ex.grpc_targets.is_empty() {
        die("explore.py: --grpc-faults needs an image whose manifest has a proxy with protocol: grpc (and allows `protocol` in faults:, or has no faults: list)");
    }
    if a.freeze_call && cx.ex.fc_pairs.is_empty() {
        die("explore.py: --freeze-call needs an image whose manifest lists `freeze` in faults: and has two services that talk (manifest_space.py)");
    }
    if a.more_faults && cx.ex.backend == Backend::Hvm {
        die("explore.py: --more-faults needs --backend hvx (X= and cf= are stack/hvx_explore tokens)");
    }
    if a.more_faults && a.cf_range != "auto" {
        let r: Vec<i64> = a
            .cf_range
            .split('-')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        if r.len() != 2 {
            die(&format!(
                "explore.py: --cf-range {}: lo-hi or auto",
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

/// Build the engine and size the pool. The snapshot store: in memory when every worker's VM and
/// its in-memory S0/S1 fit in the available memory (hvx; HYDO_SNAPSHOTS=files|memory decides).
fn make_pool(cx: &Cx) -> Pool {
    let consoles = cx.out.join("consoles");
    let mut extra = Vec::new();
    if let Some(tm) = cx.a.turn_margin {
        extra.push(format!("HVX_TURN_MARGIN={tm}"));
    }
    let (engine, spec): (Arc<dyn Engine + Send + Sync>, BootSpec) = if cx.ex.backend == Backend::Hvm
    {
        let e = hvm::Hvm {
            hvmx: cx.hvm_dir().join("hvmx"),
            env: Vec::new(),
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
            engine: env_or(
                "HYDO_HVX_EXPLORE",
                cx.env.root.join("hv").join("hvx").join("hvx_explore"),
            ),
            hvx_run: Some(env_or("HYDO_HVX_RUN", home().join("bin").join("hvx-run"))),
            snap_dir: std::env::var("HYDO_SNAP_DIR")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/var/tmp/hvx-snap".into()),
            extra_env: extra,
            env: Vec::new(),
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
        (Arc::new(e), spec)
    };
    let (n, spec) = size_pool(cx, engine.as_ref(), spec);
    let host = Host {
        outdir: cx.out.clone(),
        onboard: cx.env.root.join("hydo").join("onboard"),
        known: cx.known.clone(),
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
    let per_mem = per_files + 2 * (ram + img_mib);
    let avail = hydo_engine::available_mib();
    let want = std::env::var("HYDO_SNAPSHOTS").ok();
    let hvx = cx.ex.backend == Backend::Hvx;
    let n = match cx.a.workers {
        Some(n) => n,
        None if hvx => hydo_engine::machine_workers(per_mem).min(engine.max_workers(&spec).max(1)),
        None => engine.max_workers(&spec),
    };
    let fits = avail.is_some_and(|m| m >= n as u64 * per_mem);
    let memory = hvx
        && match want.as_deref() {
            Some("memory") => true,
            Some("files") => false,
            _ => fits,
        };
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

fn run(cx: Cx) -> i32 {
    let _ = std::fs::create_dir_all(&cx.out);
    let t0 = Instant::now();
    let pool = make_pool(&cx);
    if let Some(sch) = &cx.a.schedule {
        let res = pool.run_all(&[("manual".to_string(), sch.clone())], true);
        println!("{}", dumps_line(&Value::Object(res["manual"].1.clone())));
        pool.close();
        return 0;
    }
    if cx.a.branch.is_some() {
        return branch_main(&cx, pool, t0);
    }
    let mut cx = cx;
    let (seeds, items, pre_res, fc_info) = match &cx.a.sweep {
        Some(sw) => {
            if let Some(sp) = &cx.ex.space {
                if let Err(e) = sp.check_sweep(sw) {
                    pool.close();
                    die(&e);
                }
            }
            let (points, items, pre, info) = if sw == "freeze" && cx.a.fc_adaptive {
                match fc::adaptive(&cx.ex, &pool, &cx.a, &cx.out) {
                    Ok(o) => (o.points, Some(o.items), Some(o.res), Some(o.info)),
                    Err(e) => {
                        pool.close();
                        die(&e)
                    }
                }
            } else {
                let p = if sw == "freeze" {
                    fc::freeze_sweep(&cx.ex, &pool, &cx.a, &cx.out)
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
            let lines: String = seeds
                .iter()
                .zip(&points)
                .map(|(i, (lab, _))| dumps_line(&json!({"seed": i, "point": lab})) + "\n")
                .collect();
            let _ = std::fs::write(cx.out.join("sweep-points.jsonl"), lines);
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
            (seeds, items, pre, info)
        }
        None => {
            let (lo, hi) =
                cx.a.seeds
                    .split_once('-')
                    .and_then(|(l, h)| {
                        Some((l.trim().parse::<i64>().ok()?, h.trim().parse::<i64>().ok()?))
                    })
                    .unwrap_or_else(|| die(&format!("explore.py: --seeds {}: lo-hi", cx.a.seeds)));
            let seeds: Vec<i64> = (lo..=hi).collect();
            if cx.ex.more_faults && cx.ex.cf_range.is_none() {
                let r = measure_cf_range(&pool, &cx.out);
                match r {
                    Some(r) => {
                        eprintln!("cf= range (measured with cfn): {}-{}", r.0, r.1);
                        cx.ex.cf_range = Some(r);
                    }
                    None => {
                        pool.close();
                        die("explore.py: --cf-range auto: no FLUSH count from the cfn probe (see worker-*.err)");
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
            (seeds, items, None, None)
        }
    };
    explore(&cx, pool, t0, seeds, items, pre_res, fc_info)
}

/// explore.py `measure_cf_range`.
fn measure_cf_range(pool: &Pool, out: &Path) -> Option<(i64, i64)> {
    let tag = "cfn-probe";
    pool.run_all(&[(tag.into(), "disk=cut dseed=0 cfn | run".into())], true);
    let re = regex::Regex::new(&format!(
        r"^FLUSHCUT {} target=\d+ flushes=(\d+)",
        regex::escape(tag)
    ))
    .ok()?;
    for i in 0..pool.workers() {
        let f = out.join(format!("worker-{i}.err"));
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

/// One failure's record (runs.jsonl / report.json "failures").
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
    let af = assert_failures(&cx.out, &tag);
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
    log: Vec<String>,
}

/// A shrink's test of a candidate's (verdict, result): does it still fail the same way?
type Keep<'a> = &'a dyn Fn(&str, &Res) -> bool;
/// The error-log kinds and their shrink records (report.md's error section).
type ErrorsReport = (Map<String, Value>, Map<String, Value>);

/// explore.py `shrink`. Returns (minimal, its verdict, tries) and appends the log lines.
#[allow(clippy::too_many_arguments)]
fn shrink(
    cx: &Cx,
    pool: &Pool,
    s: S,
    verdict: &str,
    log: &mut Vec<String>,
    tjk: Option<(i64, i64)>,
    keep: Option<Keep<'_>>,
    prefix: &str,
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
                log.push(dumps_line(&json!({"shrink": items[next].1, "verdict": v})));
                let kept = match keep {
                    Some(k) => k(&v, r),
                    None => same_failure(&cur_v, &v),
                };
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
    );
    let mut mt = cx.render(&m);
    let conf_pair = |t: &str| {
        let r = pool.run_all(
            &[(ma.clone(), t.to_string()), (mb.clone(), t.to_string())],
            true,
        );
        (r[&ma].1.clone(), r[&mb].1.clone())
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
                    let (ca, cb) = conf_pair(&st);
                    log.push(dumps_line(&json!({"stable": st, "verdict": ca["verdict"]})));
                    if same_failure(&mv, &py_str(&ca["verdict"])) && ca["fp"] == cb["fp"] {
                        m = ms;
                        mt = st.clone();
                        st_map.insert("stable_schedule".into(), json!(st));
                        c = Some((ca, cb));
                    } else {
                        st_map.insert(
                            "stable_note".into(),
                            json!(format!(
                                "the resolved composition did not fail the same way ({}); the minimal schedule keeps the seed form",
                                head(&py_str(&ca["verdict"]), 120)
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
                conf = c.unwrap_or_else(|| conf_pair(&mt));
                stable = Some(st_map);
            }
            Err(e) => {
                eprintln!("hydo-explore: the composition of {mt}: {e}");
                conf = conf_pair(&mt);
            }
        },
        _ => conf = conf_pair(&mt),
    }
    let mut rec = Map::new();
    rec.insert("from_seed".into(), f["seed"].clone());
    rec.insert("minimal".into(), json!(mt));
    rec.insert("verdict".into(), json!(mv));
    rec.insert("tries".into(), json!(n));
    rec.insert("min_class".into(), json!(cx.ex.finding_class(&m)));
    rec.insert("fp_a".into(), conf.0["fp"].clone());
    rec.insert("fp_b".into(), conf.1["fp"].clone());
    rec.insert("min_verdict".into(), conf.0["verdict"].clone());
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
    Shrunk { key, rec, log }
}

/// explore.py `errlog_shrink` for one kind.
fn shrink_errkind(cx: &Cx, pool: &Pool, x: &Value) -> (String, Value, Vec<String>) {
    let sig = py_str(&x["sig"]);
    let mut log = Vec::new();
    let keep = |_: &str, r: &Res| {
        r.get("errors_new")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|s| py_str(s) == sig))
    };
    let s0 = cx.parse(&py_str(&x["first_new_schedule"]));
    let (m, _, n) = shrink(
        cx,
        pool,
        s0,
        "",
        &mut log,
        None,
        Some(&keep),
        &format!("errshrink-{sig}"),
    );
    let mt = cx.render(&m);
    let (ta, tb) = (format!("errmin-a-{sig}"), format!("errmin-b-{sig}"));
    let conf = pool.run_all(&[(ta.clone(), mt.clone()), (tb.clone(), mt.clone())], true);
    let (ra, rb) = (&conf[&ta].1, &conf[&tb].1);
    let v = json!({"minimal": mt, "tries": n, "fp_a": ra["fp"], "fp_b": rb["fp"], "verdict": ra["verdict"],
                   "present_a": keep("", ra), "present_b": keep("", rb)});
    (sig, v, log)
}

#[allow(clippy::too_many_arguments)]
fn explore(
    cx: &Cx,
    pool: Pool,
    t0: Instant,
    seeds: Vec<i64>,
    items: Vec<(String, String)>,
    pre_res: Option<HashMap<String, (String, Res)>>,
    fc_info: Option<Map<String, Value>>,
) -> i32 {
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
    let mut runs = std::fs::File::create(cx.out.join("runs.jsonl"))
        .unwrap_or_else(|e| die(&format!("{}: {e}", cx.out.display())));
    let mut fails: Vec<Rec> = Vec::new();
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
                let _ = runs.write_all((dumps_line(&Value::Object(rec.clone())) + "\n").as_bytes());
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
                    fails.push(rec);
                }
                next += 1;
            }
        }
        t_sweep = match &fc_info {
            Some(i) => {
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
        let an_runs: Vec<(i64, String, Option<Value>)> = seeds
            .iter()
            .map(|sd| {
                let tag = format!("seed-{sd}");
                (
                    *sd,
                    all.get(&tag).map(|x| x.0.clone()).unwrap_or_default(),
                    errlog_of(&cx.out, &tag),
                )
            })
            .collect();
        let errors = if an_runs.iter().any(|x| x.2.is_some()) {
            let agg = errlog::collect(&an_runs);
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
            let done: Vec<(String, Value, Vec<String>)> = hs
                .into_iter()
                .map(|h| h.join().expect("a shrink"))
                .collect();
            (agg, an_runs, done)
        });
        (shrunk, errors, all)
    });
    drop(runs);
    // the re-runs
    for f in fails.iter_mut() {
        let sd = f["seed"].as_i64().unwrap_or(0);
        let rr = rep.get(&format!("repro-{sd}")).cloned().unwrap_or_default();
        f.insert(
            "repro_fp".into(),
            rr.get("fp").cloned().unwrap_or(Value::Null),
        );
        f.insert(
            "repro_verdict".into(),
            rr.get("verdict").cloned().unwrap_or(Value::Null),
        );
        let mut ok = rr.get("fp") == f.get("fp");
        for k in 1..nrep {
            let rk = rep
                .get(&format!("repro{}-{sd}", k + 1))
                .cloned()
                .unwrap_or_default();
            f.insert(
                format!("repro{}_fp", k + 1),
                rk.get("fp").cloned().unwrap_or(Value::Null),
            );
            ok = ok && rk.get("fp") == f.get("fp");
        }
        f.insert("reproduced".into(), json!(ok));
    }
    // shrink.jsonl: each chain's lines in explore.py's order (representatives by first seed, then
    // the error-log kinds)
    let mut slog = String::new();
    for s in &shrunk {
        for l in &s.log {
            slog.push_str(l);
            slog.push('\n');
        }
    }
    let mut eshr = Map::new();
    if let Some((_, _, done)) = &errors {
        for (sig, v, log) in done {
            for l in log {
                slog.push_str(l);
                slog.push('\n');
            }
            eshr.insert(sig.clone(), v.clone());
        }
    }
    let _ = std::fs::write(cx.out.join("shrink.jsonl"), slog);
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
    let mut summary = Map::new();
    summary.insert("schedules".into(), json!(seeds.len()));
    summary.insert(
        "strategy".into(),
        json!(a
            .sweep
            .as_ref()
            .map(|s| format!("sweep:{s}"))
            .unwrap_or_else(|| a.strategy.clone())),
    );
    summary.insert("image".into(), json!(cx.image));
    summary.insert("workers".into(), json!(nworkers));
    summary.insert("sweep_wall_s".into(), fl(round1(t_sweep)));
    summary.insert(
        "runs_per_hour".into(),
        fl(round1(seeds.len() as f64 / t_sweep.max(1e-9) * 3600.0)),
    );
    summary.insert(
        "mean_run_s".into(),
        fl(round1(secs.iter().sum::<f64>() / secs.len().max(1) as f64)),
    );
    summary.insert("worker_boot_s".into(), fl(round1(boot)));
    summary.insert("total_wall_s".into(), fl(round1(total)));
    summary.insert("failures".into(), json!(fails.len()));
    summary.insert(
        "reproduced".into(),
        json!(fails.iter().filter(|f| f["reproduced"] == true).count()),
    );
    if nrep != 1 {
        summary.insert("repro_runs".into(), json!(nrep));
    }
    if let Some(info) = &fc_info {
        for (k, v) in info {
            summary.insert(k.clone(), v.clone());
        }
    }
    let qw = pool::quiet_waited();
    if qw > 0.0 {
        summary.insert("quiet_wait_s".into(), fl(round1(qw)));
    }
    if cache_hits > 0 {
        summary.insert("cache_hits".into(), json!(cache_hits));
    }
    add_machine(cx, &mut summary);
    if cx.ex.backend == Backend::Hvm {
        if let Some((_, sha)) = hvm::image_kernel(&cx.hvm_image()) {
            let k = cx
                .hvm_kernel
                .clone()
                .unwrap_or_else(|| cx.hvm_default_kernel());
            summary.insert(
                "guest_kernel".into(),
                json!(format!(
                    "{} (sha256 {})",
                    k.file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    head(&sha, 16)
                )),
            );
        }
    }
    let mut shrunk_json = Vec::new();
    let mut shrunk_map: Vec<((String, String), Rec)> = Vec::new();
    for s in &shrunk {
        let mut o = Map::new();
        o.insert("class".into(), json!(s.key.0));
        o.insert("signature".into(), json!(s.key.1));
        for (k, v) in &s.rec {
            o.insert(k.clone(), v.clone());
        }
        shrunk_json.push(Value::Object(o));
        shrunk_map.push((s.key.clone(), s.rec.clone()));
    }
    let mut rep_j = Map::new();
    let known = cx.known.clone().expect("loaded");
    let mut errs_report = None;
    if let Some((agg, an_runs, _)) = &errors {
        summary.insert(
            "error_kinds_new".into(),
            json!(agg
                .values()
                .filter(|x| x["new_runs"].as_i64().unwrap_or(0) > 0)
                .count()),
        );
        summary.insert(
            "runs_with_new_error_kinds".into(),
            json!(an_runs
                .iter()
                .filter(|x| x
                    .2
                    .as_ref()
                    .is_some_and(|an| an["new"].as_array().is_some_and(|n| !n.is_empty())))
                .count()),
        );
        let kinds: Vec<Value> = agg
            .values()
            .map(|x| {
                let mut o = x.as_object().cloned().unwrap_or_default();
                o.insert("new".into(), json!(x["new_runs"].as_i64().unwrap_or(0) > 0));
                o.insert(
                    "shrunk".into(),
                    eshr.get(&py_str(&x["sig"])).cloned().unwrap_or(Value::Null),
                );
                Value::Object(o)
            })
            .collect();
        let kv = &known.v;
        let doc = json!({"known_file": kv["path"], "known_sha256": kv["sha256"], "unknown": kv["unknown"], "kinds": kinds});
        let _ = std::fs::write(cx.out.join("errors.json"), dumps(&doc, 1));
        errs_report = Some((agg.clone(), eshr.clone()));
        rep_j.insert("summary".into(), Value::Object(summary.clone()));
        rep_j.insert(
            "failures".into(),
            Value::Array(fails.iter().cloned().map(Value::Object).collect()),
        );
        rep_j.insert("shrunk".into(), Value::Array(shrunk_json));
        rep_j.insert("errors".into(), doc);
    } else {
        rep_j.insert("summary".into(), Value::Object(summary.clone()));
        rep_j.insert(
            "failures".into(),
            Value::Array(fails.iter().cloned().map(Value::Object).collect()),
        );
        rep_j.insert("shrunk".into(), Value::Array(shrunk_json));
    }
    let _ = std::fs::write(cx.out.join("report.json"), dumps(&Value::Object(rep_j), 1));
    let tags: Vec<String> = seeds.iter().map(|sd| format!("seed-{sd}")).collect();
    let table = report::property_table(&cx.out, &cx.env.root.join("hydo"), &tags);
    if let Some(t) = &table {
        let _ = std::fs::write(cx.out.join("properties.txt"), t);
    }
    write_report(
        cx,
        &summary,
        &fails,
        &shrunk_map,
        table.as_deref(),
        errs_report.as_ref(),
        &known,
    );
    println!("{}", dumps_line(&Value::Object(summary)));
    0
}

fn add_machine(cx: &Cx, summary: &mut Map<String, Value>) {
    if cx.ram_mib != 1024 {
        summary.insert("ram_mib".into(), json!(cx.ram_mib));
    }
    if cx.vcpus != 1 {
        summary.insert("vcpus".into(), json!(cx.vcpus));
    }
    if cx.cpu != "v2" {
        summary.insert("cpu".into(), json!(cx.cpu));
    }
}

/// explore.py `write_report`.
fn write_report(
    cx: &Cx,
    summary: &Map<String, Value>,
    fails: &[Rec],
    shrunk: &[((String, String), Rec)],
    table: Option<&str>,
    errors: Option<&ErrorsReport>,
    known: &Known,
) {
    let mut l: Vec<String> = vec![
        "# Explorer report".into(),
        String::new(),
        "| | |".into(),
        "|---|---|".into(),
    ];
    for (k, v) in summary {
        l.push(format!("| {k} | {} |", cell(v)));
    }
    let mut by: std::collections::BTreeMap<(String, String), Vec<&Rec>> = Default::default();
    for f in fails {
        let k = (py_str(&f["class"]), sig_key(&py_str(&f["verdict"])));
        by.entry(k).or_default().push(f);
    }
    l.extend(
        [
            "",
            "## Failures by class and signature",
            "",
            "| class | signature | runs | seeds | reproduced |",
            "|---|---|---|---|---|",
        ]
        .map(String::from),
    );
    for ((c, sig), fs) in &by {
        l.push(format!(
            "| {c} | `{sig}` | {} | {} | {}/{} |",
            fs.len(),
            fs.iter()
                .take(8)
                .map(|f| py_str(&f["seed"]))
                .collect::<Vec<_>>()
                .join(" "),
            fs.iter().filter(|f| f["reproduced"] == true).count(),
            fs.len()
        ));
    }
    l.extend(["", "## Minimal reproducers", ""].map(String::from));
    let turns_re = regex::Regex::new(r"(?:^|\s)(?:qs?|qb)=").expect("p");
    for ((c, sig), v) in shrunk {
        let mc = v.get("min_class").map(py_str).unwrap_or_else(|| c.clone());
        let hd = if mc == *c {
            mc.clone()
        } else {
            format!("{mc} (found as {c}; the minimal schedule needs only {mc})")
        };
        let minimal = py_str(&v["minimal"]);
        l.push(format!("### {hd}: `{sig}`"));
        l.push(String::new());
        l.push(format!(
            "- from seed {}, {} shrink runs",
            py_str(&v["from_seed"]),
            py_str(&v["tries"])
        ));
        l.push(format!("- minimal schedule: `{minimal}`"));
        l.push(format!(
            "- needs a tick-timing change: {}",
            if minimal.contains(" tj") { "yes" } else { "no" }
        ));
        if v.get("composition").is_some_and(hydo_manifest::py::truthy) {
            l.push(format!(
                "- composition (test composer, W=): {}",
                py_str(&v["composition"])
            ));
        }
        if v.get("stable").is_some_and(hydo_manifest::py::truthy) {
            let st = v.get("stable_schedule").map(py_str);
            l.push(format!(
                "- stable form (commands by id; `hydo save` writes it): {}",
                if st.as_deref() == Some(minimal.as_str()) {
                    "the minimal schedule".to_string()
                } else {
                    format!(
                        "`{}` ({})",
                        st.unwrap_or_else(|| "None".into()),
                        v.get("stable_note").map(py_str).unwrap_or_default()
                    )
                }
            ));
        }
        if cx.vcpus > 1 {
            let host = minimal.split('|').next().unwrap_or("");
            l.push(format!(
                "- needs vCPU turn lengths: {}",
                if turns_re.is_match(host) { "yes" } else { "no" }
            ));
        }
        l.push(format!("- verdict: `{}`", py_str(&v["min_verdict"])));
        l.push(format!(
            "- fingerprint, two runs: `{}` / `{}` ({})",
            py_str(&v["fp_a"]),
            py_str(&v["fp_b"]),
            if v["fp_a"] == v["fp_b"] {
                "identical"
            } else {
                "DIFFERENT"
            }
        ));
        l.push(format!("- by hand: {}", cx.by_hand(&minimal)));
        l.push(String::new());
    }
    l.extend(
        [
            "## Property table over every schedule (hvx/ASSERTIONS.md)",
            "",
        ]
        .map(String::from),
    );
    match table {
        None => l.extend(
            [
                "The guest emitted no HYDO-ASSERT records (an image built without the assertion SDK).",
                "",
            ]
            .map(String::from),
        ),
        Some(t) => {
            l.push("```".into());
            l.push(t.trim_end().to_string());
            l.push("```".into());
            l.push(String::new());
            l.extend(report::liveness_summary(&cx.out));
        }
    }
    if let Some((agg, eshr)) = errors {
        l.extend(errlog::report_md(agg, known, eshr, &|s| cx.by_hand(s)));
    }
    let _ = std::fs::write(cx.out.join("report.md"), l.join("\n") + "\n");
}

/// explore.py `branch_main`: each seed's schedule runs to the marker, then timing variants.
fn branch_main(cx: &Cx, pool: Pool, t0: Instant) -> i32 {
    let a = &cx.a;
    let (lo, hi) = a
        .seeds
        .split_once('-')
        .and_then(|(l, h)| Some((l.trim().parse::<i64>().ok()?, h.trim().parse::<i64>().ok()?)))
        .unwrap_or_else(|| die(&format!("explore.py: --seeds {}: lo-hi", a.seeds)));
    let pm = cx.ex.tj_pm[cx.ex.tj_pm.len() / 2];
    let marker = a.branch.clone().unwrap_or_default();
    let reqs: Vec<(String, hydo_engine::BranchRequest, String)> = (lo..=hi)
        .map(|sd| {
            let text = cx.render(&cx.ex.generator(&a.strategy, sd as u64));
            let tag = format!("seed-{sd}");
            let sched = hydo_schedule::Schedule::parse(&text)
                .unwrap_or_else(|e| die(&format!("{text}: {e}")));
            (
                tag.clone(),
                hydo_engine::BranchRequest {
                    tag,
                    variants: a.variants as i32,
                    permille: pm as u32,
                    max_delay: TJ_MAX as u64,
                    marker: marker.clone(),
                    schedule: sched,
                },
                text,
            )
        })
        .collect();
    let t1 = Instant::now();
    let done = pool.branch_all(reqs.clone());
    let t_sweep = t1.elapsed().as_secs_f64();
    let mut runs = String::new();
    let (mut fails, mut nrun, mut nbp, mut secs) = (Vec::<Rec>::new(), 0usize, 0usize, 0.0f64);
    for b in &done {
        nbp += usize::from(b.tjlo.is_some());
        for (sub, st, rr) in &b.runs {
            nrun += 1;
            secs += rr["secs"].as_f64().unwrap_or(0.0);
            let mut rec = Map::new();
            rec.insert("tag".into(), json!(sub));
            rec.insert("schedule".into(), json!(st));
            rec.insert("class".into(), json!(cx.ex.finding_class(&cx.parse(st))));
            rec.insert("branch_tjlo".into(), json!(b.tjlo));
            rec.insert("branch_held".into(), json!(b.held));
            for (k, v) in rr {
                rec.insert(k.clone(), v.clone());
            }
            runs.push_str(&(dumps_line(&Value::Object(rec.clone())) + "\n"));
            if verdict_kind(&py_str(&rr["verdict"])) != "ok" {
                fails.push(rec);
            }
        }
    }
    let _ = std::fs::write(cx.out.join("runs.jsonl"), runs);
    let items: Vec<(String, String)> = fails
        .iter()
        .enumerate()
        .map(|(i, f)| (format!("repro-{i}"), py_str(&f["schedule"])))
        .collect();
    let rep = pool.run_all(&items, true);
    for (i, f) in fails.iter_mut().enumerate() {
        let rr = rep[&format!("repro-{i}")].1.clone();
        f.insert("repro_fp".into(), rr["fp"].clone());
        f.insert("repro_verdict".into(), rr["verdict"].clone());
        let ok = rr["fp"] == f["fp"];
        f.insert("reproduced".into(), json!(ok));
    }
    let mut base_sig: HashMap<String, String> = HashMap::new();
    for f in &fails {
        let tag = py_str(&f["tag"]);
        if let Some(b) = tag.strip_suffix("-base") {
            base_sig.insert(b.to_string(), report::signature(&py_str(&f["verdict"])));
        }
    }
    for f in fails.iter_mut() {
        let tag = py_str(&f["tag"]);
        let pre = tag
            .rsplit_once("-v")
            .map(|x| x.0.to_string())
            .unwrap_or_else(|| tag.clone());
        let t = !tag.ends_with("-base")
            && base_sig.get(&pre) != Some(&report::signature(&py_str(&f["verdict"])));
        f.insert("timing_only".into(), json!(t));
    }
    let mut summary = Map::new();
    summary.insert("schedules".into(), json!(reqs.len()));
    summary.insert("branch_points".into(), json!(nbp));
    summary.insert("runs".into(), json!(nrun));
    summary.insert("variants_per_point".into(), json!(a.variants));
    summary.insert("tjpm".into(), json!(pm));
    summary.insert("sweep_wall_s".into(), fl(round1(t_sweep)));
    summary.insert(
        "runs_per_hour".into(),
        fl(round1(nrun as f64 / t_sweep.max(1.0) * 3600.0)),
    );
    summary.insert("mean_run_s".into(), fl(round1(secs / nrun.max(1) as f64)));
    summary.insert("failures".into(), json!(fails.len()));
    summary.insert(
        "reproduced_standalone".into(),
        json!(fails.iter().filter(|f| f["reproduced"] == true).count()),
    );
    summary.insert(
        "timing_only_failures".into(),
        json!(fails.iter().filter(|f| f["timing_only"] == true).count()),
    );
    summary.insert(
        "total_wall_s".into(),
        fl(round1(t0.elapsed().as_secs_f64())),
    );
    add_machine(cx, &mut summary);
    let doc = json!({"summary": summary, "failures": fails});
    let _ = std::fs::write(cx.out.join("report.json"), dumps(&doc, 1));
    let mut l = vec![
        format!("# Branch report (marker `{marker}`)"),
        String::new(),
        "| | |".into(),
        "|---|---|".into(),
    ];
    for (k, v) in &summary {
        l.push(format!("| {k} | {} |", cell(v)));
    }
    l.extend(
        [
            "",
            "## Failures",
            "",
            "| run | class | timing only | verdict | reproduced standalone |",
            "|---|---|---|---|---|",
        ]
        .map(String::from),
    );
    for f in &fails {
        l.push(format!(
            "| {} | {} | {} | `{}` | {} |",
            py_str(&f["tag"]),
            py_str(&f["class"]),
            if f["timing_only"] == true {
                "yes"
            } else {
                "no"
            },
            head(&py_str(&f["verdict"]), 90),
            if f["reproduced"] == true { "yes" } else { "NO" }
        ));
    }
    let _ = std::fs::write(cx.out.join("report.md"), l.join("\n") + "\n");
    pool.close();
    println!("{}", dumps_line(&Value::Object(summary)));
    0
}
