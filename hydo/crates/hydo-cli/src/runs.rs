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
use hydo_engine::image::{boot_files, sha256_file};
use hydo_engine::protocol::verdict_kind;
use hydo_engine::{BootSpec, ControlledEngine, CpuLevel, RunResult};
use hydo_manifest::Guest;
use hydo_schedule::Schedule;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------- images and engine options

/// The image file names of this platform: (what `hydo build` writes, a positional image's suffix).
pub(crate) fn image_pattern(ctx: &Ctx) -> (&'static str, &'static str) {
    match ctx.platform {
        Platform::Linux => ("-disk.img", ".img"),
        Platform::Mac => ("-arm64.cpio.gz", ".cpio.gz"),
    }
}

/// Milestone 368: of several images in one folder, the one whose build record
/// (`<image>.json`, `manifest.path`) names a manifest called `hydo.yaml`, when exactly one does;
/// else the newest by its file's time (a tie: the last by name). With the reason in words.
pub(crate) fn pick_image(imgs: &[PathBuf]) -> (PathBuf, &'static str) {
    let manifest_of = |p: &Path| -> Option<String> {
        let rec = std::fs::read_to_string(format!("{}.json", p.display())).ok()?;
        let v: Value = serde_json::from_str(&rec).ok()?;
        Some(v.pointer("/manifest/path")?.as_str()?.to_string())
    };
    let own: Vec<&PathBuf> = imgs
        .iter()
        .filter(|p| {
            manifest_of(p).is_some_and(|m| {
                Path::new(&m).file_name().and_then(|f| f.to_str()) == Some("hydo.yaml")
            })
        })
        .collect();
    if own.len() == 1 {
        return (own[0].clone(), "built from hydo.yaml");
    }
    let newest = imgs
        .iter()
        .max_by_key(|p| {
            (
                std::fs::metadata(p).and_then(|m| m.modified()).ok(),
                p.to_path_buf(),
            )
        })
        .cloned()
        .unwrap_or_default();
    (newest, "the newest")
}

/// `--image IMG`, or a leading positional image, or the one image `hydo build` wrote in ./hydo-out
/// (then ./.hydo, where a project's hydo/hydo.yaml builds). Of several, [`pick_image`] takes one
/// and says which.
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
            if imgs.len() > 1 {
                // Milestone 368: a build of a copy of the manifest (`--set` on `hydo-big.yaml`)
                // writes a second image beside the first, and every command stopped; take the
                // one built from the folder's own hydo.yaml, else the newest, and say which
                let (pick, why) = pick_image(&imgs);
                let others: Vec<String> = imgs
                    .iter()
                    .filter(|p| **p != pick)
                    .map(|p| p.display().to_string())
                    .collect();
                eprintln!(
                    "hydo: note: no image given, and there are {} images; this takes {} ({why}). \
                     Pass --image <path> for another: {}",
                    imgs.len(),
                    pick.display(),
                    others.join(", ")
                );
                imgs = vec![pick];
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
/// Python CLIs fell back the same way when the image had no manifest). The one reading every
/// boot path uses ([`hydo_control::agent::image_guest`], M207).
pub fn image_guest(image: &Path) -> Guest {
    hydo_control::agent::image_guest(image).unwrap_or_else(|_| Guest::from_value(&Value::Null))
}

/// The engine options the explorer passes for this image (explore.py's engine_args_for). The
/// CPU cache leaves (`guest.cpu_cache`, M203) always come from the image: no recorded run
/// carries them apart from its engine arguments.
pub fn engine_args(image: &Path, ram: Option<u64>, cpu: Option<String>, vcpus: u64) -> Vec<String> {
    let g = image_guest(image);
    let (ram, cpu) = (ram.unwrap_or(g.ram_mib as u64), cpu.unwrap_or(g.cpu));
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
    if hydo_engine::image::cpu_cache_of(image, g.cpu_cache) {
        a.push("--cpu-cache".to_string());
        a.push("on".to_string());
    }
    a
}

/// The vCPU options of `hydo run`, `hydo repro` and `hydo explain` (M205): `--vcpus N` (hvx
/// only; default `HVX_EXPLORE_VCPUS`, else 1) and `--turn-margin N` (the in-kernel scheduler's
/// turn-end margin, M85; hvx with several vCPUs). They become engine arguments, which the run's
/// result.json records (`engine.args`), so `hydo repro <run dir>` runs with them again. The
/// turn lengths themselves are schedule tokens (`q=`, `qs=`, `qb=`, `qm=`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOpts {
    /// None: not given (one vCPU).
    pub vcpus: Option<u32>,
    pub turn_margin: Option<u64>,
}

/// The vCPU and turn options' lines of the commands' help texts.
pub const RUN_OPTS_HELP: &str =
    "  --vcpus N        run the guest on N vCPUs (hvx or KVM, 1..8; default 1, or HVX_EXPLORE_VCPUS);
                   they take turns of the schedule's q= guest branches on hvx (default 2^20),
                   of q= counted exits on KVM (default 256)
  --turn-margin N  the in-kernel scheduler's turn-end margin in branches (hvx, several vCPUs)";

impl RunOpts {
    /// Take `--vcpus N` and `--turn-margin N` from `args`; dies on a bad value. Without
    /// `--vcpus`, `HVX_EXPLORE_VCPUS` (which the engine read on its own before M205) names the
    /// count, so that the run records it.
    pub fn take(ctx: &Ctx, args: &mut Vec<String>) -> RunOpts {
        let num = |flag: &str, v: &str| -> u64 {
            v.parse()
                .unwrap_or_else(|_| die(&format!("{flag} {v}: not a whole number")))
        };
        let vcpus = take(args, "--vcpus")
            .map(|v| num("--vcpus", &v))
            .or_else(|| {
                ctx.config
                    .env
                    .nonempty(hydo_config::registry::HVX_EXPLORE_VCPUS)
                    .map(|v| num(hydo_config::registry::HVX_EXPLORE_VCPUS, &v))
            });
        let turn_margin = take(args, "--turn-margin").map(|v| num("--turn-margin", &v));
        let o = RunOpts {
            vcpus: vcpus.map(|n| n.min(u32::MAX as u64) as u32),
            turn_margin,
        };
        if let Err(e) = o.check(ctx.platform, ctx.kvm) {
            die(&e);
        }
        o
    }

    /// Whether this platform's engine can run them (`kvm`: Linux's KVM engine).
    pub fn check(&self, platform: Platform, kvm: bool) -> Result<(), String> {
        let n = self.vcpus.unwrap_or(1);
        if !(1..=8).contains(&n) {
            return Err(format!(
                "--vcpus {n}: the vCPU count must be 1..8 (hvx's HVX_MAX_VCPUS)"
            ));
        }
        if n > 1 && platform == Platform::Mac {
            return Err(format!(
                "--vcpus {n}: the Mac engine runs one vCPU (several vCPUs need hvx or KVM on Linux)"
            ));
        }
        if let Some(m) = self.turn_margin {
            if kvm {
                return Err(format!(
                    "--turn-margin {m}: the KVM engine's turns are counted in exits and need no margin"
                ));
            }
            if n < 2 || !(8..=(1 << 20)).contains(&m) {
                return Err(format!(
                    "--turn-margin {m}: needs --vcpus N > 1 and a value in [8, 2^20]"
                ));
            }
        }
        Ok(())
    }

    /// The engine arguments of `image` with these options.
    pub fn eargs(&self, image: &Path) -> Vec<String> {
        let mut a = engine_args(image, None, None, self.vcpus.unwrap_or(1) as u64);
        if let Some(m) = self.turn_margin {
            a.push("--turn-margin".into());
            a.push(m.to_string());
        }
        a
    }

    /// The options in recorded engine arguments (a run's `engine.args`).
    pub fn of_eargs(eargs: &[String]) -> RunOpts {
        let get = |f: &str| {
            eargs
                .windows(2)
                .find(|w| w[0] == f)
                .and_then(|w| w[1].parse::<u64>().ok())
        };
        RunOpts {
            vcpus: get("--vcpus").filter(|n| *n > 1).map(|n| n as u32),
            turn_margin: get("--turn-margin"),
        }
    }
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
        cpu_cache: false,
        // the image decides how the KVM engine runs it (M204), whatever arguments a recorded run
        // carries
        epoch: hydo_engine::image::image_epoch(image).0,
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
            "--cpu-cache" => spec.cpu_cache = eargs[i + 1] == "on",
            _ => {}
        }
        i += 2;
    }
    spec
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
    // the default kernel of images built before the fingerprint epoch 3, or of this epoch's
    // images (with cgroups, M234), needs no line
    let now = mac_kernel(ctx).with_file_name(hydo_image::kernels::arm64_kernel(false).file);
    if !quiet && k != mac_kernel(ctx) && k != now {
        let rel = k.strip_prefix(&ctx.root).unwrap_or(&k);
        println!(
            "hydo: guest kernel {} (recorded in the image)",
            rel.display()
        );
    }
    k
}

/// Why a guest of the engine options `eargs` does not fit into this machine's memory now, with
/// the running `hydo inspect` sessions, each of which holds a guest until it is stopped
/// (Milestones 179 and 239); None when it fits.
pub fn memory_refusal(ctx: &Ctx, eargs: &[String]) -> Option<String> {
    let ram = eargs
        .windows(2)
        .find(|w| w[0] == "--ram-mib")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(1024);
    let e = hydo_engine::guest_fits(ram).err()?;
    let running = crate::inspect::session::running(ctx);
    Some(match crate::inspect::session::memory_hint(&running) {
        Some(h) => format!("{e}. {h}"),
        None => format!("{e}. No hydo inspect session is running."),
    })
}

/// This platform's engine for `image` and the boot spec of a run into `outdir` with the engine
/// options `eargs` (`hydo run`'s choice: hvx, or KVM with `--backend kvm`, on Linux; hvm on the
/// Mac). Dies with `hydo: ` when the engine or the image's kernel is missing.
pub fn controlled_engine(
    ctx: &Ctx,
    image: &Path,
    outdir: &Path,
    eargs: &[String],
    quiet: bool,
) -> (Arc<dyn ControlledEngine + Send + Sync>, BootSpec) {
    // M179: a guest may have up to 8 GiB; refuse before the engine fails to make the VM
    if let Some(why) = memory_refusal(ctx, eargs) {
        die(&why);
    }
    match ctx.platform {
        Platform::Linux if ctx.kvm => {
            // the KVM build of the image's recorded kernel, and the image's initramfs
            let dirs = [ctx.guest(), ctx.p("build"), ctx.p("hv/kvm/linux/out")];
            let vcpus = eargs
                .iter()
                .position(|a| a == "--vcpus")
                .and_then(|i| eargs.get(i + 1))
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(1);
            let b = hydo_engine::kvm::boot_files_n(
                image,
                &dirs,
                ctx.config.kvm.kernel.as_deref(),
                vcpus,
            )
            .unwrap_or_else(|msg| die(&msg));
            if let Some(n) = &b.note {
                eprintln!("hydo: {n}");
            }
            let spec = spec_from_args(eargs, image, b.kernel, Some(b.initrd), &abspath(outdir));
            let k = hydo_engine::kvm::Kvm::for_release(&ctx.root, ctx.config.env.clone());
            (Arc::new(k), spec)
        }
        Platform::Linux => {
            ctx.require_hvx();
            // M205: `--turn-margin N` (an engine argument of `hydo run`) is the engine's
            // HVX_TURN_MARGIN, as the explorer passes it
            let mut extra_env = RunOpts::of_eargs(eargs)
                .turn_margin
                .map(|m| vec![format!("{}={m}", hydo_config::registry::HVX_TURN_MARGIN)])
                .unwrap_or_default();
            // Milestone 237: an image with `perf:` gets the engine's work record (the guest's
            // retired branches at every stop), which `hydo run` turns into result.json `perf`
            if hydo_control::agent::plan_of_image(image).is_ok_and(|p| p.perf.is_some()) {
                extra_env.push(format!("{}=1", hydo_config::registry::HYDO_WORK_RECORD));
            }
            let hvx = Hvx {
                engine: hvxenv::engine(ctx),
                hvx_run: Some(ctx.libexec().join("hvx-run")),
                extra_env,
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
            (Arc::new(hvx), spec)
        }
        Platform::Mac => {
            let kernel = mac_kernel_for(ctx, image, quiet);
            let spec = spec_from_args(eargs, image, kernel, None, outdir);
            if spec.vcpus != 1 {
                die("the Mac engine runs one vCPU (--vcpus needs hvx)");
            }
            (
                Arc::new(Hvm::for_release(&ctx.root, ctx.config.env.clone())),
                spec,
            )
        }
    }
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
    run_engine_try(ctx, image, sched, outdir, eargs, quiet, policy)
        .unwrap_or_else(|w| die(&format!("not run (no VM was started): {w}")))
}

/// [`run_engine`], with a schedule that is not run returned as `Err(why)`: one the parser refuses,
/// or one the host controller refuses before any VM because it names what the image lacks
/// (`hydo ci run` reports such a replay as refused, never as a pass).
fn run_engine_try(
    ctx: &Ctx,
    image: &Path,
    sched: &str,
    outdir: &Path,
    eargs: &[String],
    quiet: bool,
    policy: &Policy,
) -> Result<(Option<RunResult>, String), String> {
    run_engine_edited(ctx, image, sched, outdir, eargs, quiet, policy, None)
}

/// A change of the image's plan before the run (`hydo rules check`, Milestone 236: the candidate
/// rules as final invariants). The image is not changed.
pub type PlanEdit<'a> = &'a dyn Fn(&mut hydo_control::Plan);

/// One quiet run of `sched` with the image's plan changed by `edit` (`hydo rules check`). The run
/// is not recorded as a `hydo run` is: no result.json is written, only the consoles and
/// engine.log. Err: the schedule was not run.
pub fn run_with_plan(
    ctx: &Ctx,
    image: &Path,
    sched: &str,
    outdir: &Path,
    eargs: &[String],
    edit: PlanEdit<'_>,
) -> Result<(Option<RunResult>, String), String> {
    run_engine_edited(
        ctx,
        image,
        sched,
        outdir,
        eargs,
        true,
        &Policy::Schedule,
        Some(edit),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_engine_edited(
    ctx: &Ctx,
    image: &Path,
    sched: &str,
    outdir: &Path,
    eargs: &[String],
    quiet: bool,
    policy: &Policy,
    edit: Option<PlanEdit<'_>>,
) -> Result<(Option<RunResult>, String), String> {
    let _ = std::fs::create_dir_all(outdir);
    let s = Schedule::parse(sched).map_err(|e| format!("bad schedule {sched:?}: {e}"))?;
    let (eng, spec) = controlled_engine(ctx, image, outdir, eargs, quiet);
    // a host part this engine refuses (a combination its module refuses, a token it cannot
    // apply) is refused here, before any VM (M208)
    if let Some(why) = hydo_engine::refusal_on(eng.kind(), spec.vcpus, spec.epoch, &s) {
        return Err(format!("the engine refuses the schedule: {why}"));
    }
    let mut a = AgentEngine::for_image(eng, image).unwrap_or_else(|e| die(&format!("hydo: {e}")));
    if let Some(f) = edit {
        f(&mut a.plan);
    }
    if a.plan.walk && a.plan.walk_script.is_none() && matches!(policy, Policy::Schedule) {
        a.plan.walk_script = walk_script_for(ctx, image, sched, &s, outdir, eargs, quiet)?;
    }
    if let Some(t) = &a.plan.walk_script {
        let _ = std::fs::write(outdir.join(WALK_SCRIPT_FILE), t);
    }
    if !quiet {
        println!("hydo: an agent image: the host controller drives the run (/etc/hydo/control)");
    }
    let run = match a
        .with_control(&ctx.config.control)
        .one_with(&spec, &s, policy)
    {
        Ok(r) => r,
        // a schedule the host controller refuses (what it names, a seam): no VM was started
        Err(hydo_engine::EngineError::Unsupported(w)) => return Err(w),
        Err(e) => die(&format!("cannot start the engine: {e}")),
    };
    let _ = std::fs::write(outdir.join("engine.log"), &run.output);
    if !quiet {
        for line in engine_notes(&run.output) {
            println!("{line}");
        }
        if let Some(r) = &run.result {
            println!("{}", r.line);
        }
    }
    Ok((run.result, run.output))
}

/// The file of a recording replay's run directory that holds the actions it repeated
/// (Milestone 270): the `walk-act` lines of the run it replays, `<step>.<k> <json>` each.
pub const WALK_SCRIPT_FILE: &str = "walk-script.txt";

/// The folder of a recording replay's run directory with the run of the same schedule without
/// `rec`, whose actions the recording repeats (Milestone 270).
pub const WALKED_DIR: &str = "walked";

/// The script of a recording replay that no caller gave (Milestone 270). A walk with `rec`
/// repeats the actions that the same schedule without `rec` takes, so that the recording shows
/// the failing run: recording slows the browser, and a walk that chose its actions again took
/// others. This runs that schedule into `<outdir>/walked/` and takes the `walk-act` lines of its
/// console. None for a schedule whose walk does not record, and for a run that printed no
/// actions (an image built before Milestone 270, whose walker chooses again).
fn walk_script_for(
    ctx: &Ctx,
    image: &Path,
    sched: &str,
    s: &Schedule,
    outdir: &Path,
    eargs: &[String],
    quiet: bool,
) -> Result<Option<String>, String> {
    let records = s
        .guest
        .iter()
        .any(|t| matches!(t, hydo_schedule::GuestToken::Walk(w) if w.rec));
    if !records {
        return Ok(None);
    }
    let Some(plain) = crate::delegated::without_recording(sched) else {
        return Ok(None);
    };
    let dir = outdir.join(WALKED_DIR);
    if !quiet {
        println!(
            "hydo: the walk records (rec), so it repeats the actions of the same schedule without rec; that run first: {plain} (in {})",
            dir.display()
        );
    }
    let (r, _) = run_engine_edited(
        ctx,
        image,
        &plain,
        &dir,
        eargs,
        true,
        &Policy::Schedule,
        None,
    )?;
    let console = read_text(&dir.join("one.log")).unwrap_or_default();
    let script = hydo_manifest::browser::walk_script(&console);
    if !quiet {
        match (&script, &r) {
            (Some(t), Some(r)) => println!(
                "hydo: it took {} actions and ended {}; the recording repeats them",
                t.lines().count(),
                r.verdict
            ),
            (Some(t), None) => println!(
                "hydo: it took {} actions; the recording repeats them",
                t.lines().count()
            ),
            (None, _) => println!(
                "hydo: that run printed no actions (an image built before Milestone 270), so the recording chooses its actions again and may take others"
            ),
        }
    }
    Ok(script)
}

/// The lines of a run's engine output that `hydo run` prints before the RESULT line: the spin
/// observer's notes (hvm: a run ended at a counter loop), and the host controller's note that
/// it ran the schedule again in the engine's rewind mode. Before 0.3.1 the second was only in
/// `engine.log`, so a reader saw two runs' worth of time and no reason.
fn engine_notes(output: &str) -> Vec<&str> {
    output
        .lines()
        .filter(|l| {
            l.starts_with("hvmx: one") || l.starts_with("hydo: the run ended as SPIN; ran it again")
        })
        .collect()
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

fn verdict_of(res: &Map<String, Value>) -> String {
    res.get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The function contracts of one run (Milestone 241): the engine's records
/// (`<tag>.contracts.jsonl`) judged with the image's plan; prints each broken clause and the
/// number of calls checked, keeps the findings in res and folds the verdict.
pub fn contracts_one(
    image: &Path,
    outdir: &Path,
    tag: &str,
    sched: &str,
    mut res: Map<String, Value>,
    quiet: bool,
) -> Map<String, Value> {
    let plan = match hydo_contracts::plan::plan_of_image(image) {
        None => return res,
        Some(Ok(p)) => p,
        Some(Err(e)) => die(&e),
    };
    let records = outdir.join(format!("{tag}{}", hydo_contracts::plan::RECORDS_SUFFIX));
    match hydo_explore::contracts::fold(&plan, &records, &mut res) {
        Ok(j) => {
            if !quiet {
                // a plan of error paths only (Milestone 306) has no calls to speak of
                let calls = plan.programs.iter().any(|p| !p.functions.is_empty());
                let summary = if calls {
                    hydo_explore::contracts::summary(&j.report)
                } else {
                    Vec::new()
                };
                for l in summary
                    .into_iter()
                    .chain(hydo_explore::contracts::lines(&j.report))
                    .chain(hydo_explore::contracts::values_lines(&j))
                {
                    println!("{l}");
                }
                if let Some(rp) = hydo_explore::paths::of_res(&res) {
                    println!("{}", hydo_explore::paths::run_line(&rp));
                }
                if let Some(rf) = hydo_explore::funcs::of_res(&res, sched) {
                    println!("{}", hydo_explore::funcs::run_line(&rf));
                }
            }
        }
        Err(e) => println!("hydo: warning: function contracts: {e}"),
    }
    res
}

/// The run's final verdict, when the host's checks (history checks, error logs) changed the one
/// the `RESULT` line printed: the guest's checks only. A script that reads the `RESULT` line
/// would otherwise report a lost write that a history check found as OK (the libSQL
/// onboarding's friction item 8).
fn final_verdict_line(printed: &str, last: &str) -> Option<String> {
    (printed != last).then(|| {
        format!(
            "hydo: VERDICT {last}   (the run's verdict with the host's checks, as result.json \
             has it; the RESULT line above has the guest's checks only)"
        )
    })
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
    // faults after the workload's last step do nothing (hydo_control::names::step_warnings)
    if let (Ok(plan), Ok(s)) = (
        hydo_control::agent::plan_of_image(image),
        hydo_schedule::Schedule::parse(sched),
    ) {
        let mut w = hydo_control::names::step_warnings(&plan, &s);
        w.append(&mut run.warnings);
        run.warnings = w;
    }
    run.tag = None;
    run.hydo_version = Some(env!("CARGO_PKG_VERSION").into());
    run.epoch = hydo_explore::results::this_epoch();
    let p = image.display().to_string();
    run.image = Some(hydo_explore::results::image_of(&p, image, image_sha));
    let mut engine = hydo_results::Engine {
        backend: Some(backend.into()),
        args: Some(eargs.to_vec()),
        host_cpu: hydo_results::HostCpu::of(hydo_engine::host_cpu()),
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
    // Milestone 237: the performance measures of an image with `perf:`
    run.perf = crate::perf::of_run_dir(outdir, image, backend);
    run.artifacts
        .get_or_insert_with(Default::default)
        .engine_log = Some("engine.log".into());
    run
}

/// One schedule from a fresh boot into `outdir`, quietly, with the host's checks and
/// `result.json`, as `hydo run` makes it (for `hydo explain`, M165). None when the run produced no
/// RESULT line (its engine.log says why).
pub fn run_to_dir(
    ctx: &Ctx,
    image: &Path,
    image_sha: Option<String>,
    sched: &str,
    outdir: &Path,
    opts: &RunOpts,
) -> Option<hydo_results::Run> {
    try_run_to_dir(ctx, image, image_sha, sched, outdir, opts)
        .unwrap_or_else(|w| die(&format!("not run (no VM was started): {w}")))
}

/// [`run_to_dir`], with a schedule that is not run (refused before any VM) returned as
/// `Err(why)` (`hydo ci run`'s replays).
pub fn try_run_to_dir(
    ctx: &Ctx,
    image: &Path,
    image_sha: Option<String>,
    sched: &str,
    outdir: &Path,
    opts: &RunOpts,
) -> Result<Option<hydo_results::Run>, String> {
    try_run_to_dir_scripted(ctx, image, image_sha, sched, outdir, opts, None)
}

/// [`try_run_to_dir`] with the script of a recording replay (Milestone 270): the `walk-act`
/// lines of the run that the recording replays, such as an exploration's confirmation of the
/// minimal schedule. Without one, a schedule with `rec` runs the same schedule without `rec`
/// first and takes its actions ([`walk_script_for`]).
pub fn try_run_to_dir_scripted(
    ctx: &Ctx,
    image: &Path,
    image_sha: Option<String>,
    sched: &str,
    outdir: &Path,
    opts: &RunOpts,
    script: Option<String>,
) -> Result<Option<hydo_results::Run>, String> {
    let eargs = opts.eargs(image);
    let set = |p: &mut hydo_control::Plan| {
        if script.is_some() {
            p.walk_script = script.clone();
        }
    };
    let (r, _) = run_engine_edited(
        ctx,
        image,
        sched,
        outdir,
        &eargs,
        true,
        &Policy::Schedule,
        Some(&set),
    )?;
    let Some(r) = r else {
        return Ok(None);
    };
    let res = host_checks(outdir, result_map(&r), true);
    let res = errors_one(outdir, sched, res, None);
    let run = run_document(
        outdir,
        image,
        image_sha,
        sched,
        &eargs,
        &res,
        ctx.backend_name(),
    );
    write_run_document(outdir, &run);
    Ok(Some(run))
}

/// One line per eventually property that held in this run: how long after its anchor it held and
/// its deadline, so that a deadline can be chosen from runs (the libSQL onboarding rebuilt its
/// image with a longer one to learn this). `hydo report` shows the same in the property table.
pub fn eventually_lines(run: &hydo_results::Run) -> Vec<String> {
    let Some(t) = &run.properties else {
        return Vec::new();
    };
    t.properties
        .iter()
        .filter(|p| p.assertion == "eventually" && p.fails == 0)
        .filter_map(|p| {
            let s = p.slowest_ok_s?;
            Some(match p.within_s {
                Some(w) => format!(
                    "hydo: eventually {}: held {s:.2} s of guest time after its anchor (deadline {w} s)",
                    p.id
                ),
                None => format!(
                    "hydo: eventually {}: held {s:.2} s of guest time after its anchor",
                    p.id
                ),
            })
        })
        .collect()
}

/// A run's warnings (results `warnings`: faults that do nothing, a pass that checked nothing, a
/// reaction that never fired), one `hydo: warning:` line each.
pub fn print_warnings(run: &hydo_results::Run) {
    for w in &run.warnings {
        println!("hydo: warning: {w}");
    }
}

fn write_run_document(outdir: &Path, run: &hydo_results::Run) {
    let _ = hydo_results::write_document(&outdir.join(hydo_results::RUN_DOC), run);
}

/// `hydo run --memory` (Milestone 239): each service's peak memory at the run's end, read by a
/// replay, for sizing `guest.ram_mib`.
fn print_memory(ctx: &Ctx, outdir: &Path) {
    match crate::inspect::diagnose::memory_peaks(ctx, outdir) {
        Ok(v) => {
            let mib = |k: u64| k.div_ceil(1024);
            for m in &v {
                if m.processes == 0 {
                    println!(
                        "hydo: memory: {:<16} no process at the run's end",
                        m.service
                    );
                } else {
                    println!(
                        "hydo: memory: {:<16} peak {} MiB, now {} MiB ({} process{})",
                        m.service,
                        mib(m.peak_kib),
                        mib(m.now_kib),
                        m.processes,
                        if m.processes == 1 { "" } else { "es" }
                    );
                }
            }
            let total: u64 = v.iter().map(|m| m.peak_kib).sum();
            println!(
                "hydo: memory: the services' peaks add up to {} MiB; give guest.ram_mib that plus \
                 room for the workload, the page cache and the kernel (hydo guide services)",
                mib(total)
            );
        }
        Err(e) => println!("hydo: memory: not read: {e}"),
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

/// Stop `hydo run` before any VM with the refusal `why`: exit status 5
/// ([`hydo_explore::REFUSED`]), and with `json` the line `{"refused": "<why>", "status": 5}` on
/// stdout. Inside `hydo ci run` the status stays that command's ([`crate::ctx::DIE_CODE`]).
fn refuse(why: &str, json: bool) -> ! {
    use std::sync::atomic::Ordering::Relaxed;
    if crate::ctx::DIE_CODE.load(Relaxed) != 1 {
        die(why);
    }
    crate::ctx::REFUSAL_JSON.store(json, Relaxed);
    crate::ctx::die_rc(why, hydo_explore::REFUSED)
}

pub fn cmd_run(ctx: &Ctx, argv: &[String]) -> i32 {
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "usage: hydo run [IMG] \"<schedule>\" [--image IMG] [--out DIR] [--known-errors FILE] \
             [--no-diagnose] [--memory] [--json] [--record-findings] [--vcpus N] [--turn-margin N]{}\n{RUN_OPTS_HELP}\n  \
             --memory         after the run, print each service's peak memory (one more replay\n                   \
             to the run's end; the run itself is unchanged)\n  \
             --record-findings  record the run's finding in the findings store at its end\n                   \
             ($HYDO_FINDINGS_STORE or ~/.hydo/findings; `hydo findings --help`)\n  \
             --json           a refusal is also one JSON line on stdout, {{\"refused\": \"<reason>\", \"status\": 5}}\n  \
             runs one schedule from a fresh boot, e.g. hydo run \"disk=cut | run C=12 \
             R=everysec\".\n  IMG defaults to the one image `hydo build` wrote in ./hydo-out (or ./.hydo). \
             The run's\n  directory (result.json, consoles) is printed; `hydo repro <run dir>` repeats it.\n  \
             The exit status is 0 whenever the run finished, whatever its verdict (OK, VIOLATION or\n  \
             ERROR); a script reads the verdict from result.json (verdict.kind). It is 5 when the run\n  \
             is refused before any VM, because the guest does not fit into this machine's memory.\n  \
             `hydo guide faults` lists the schedule tokens.",
            if ctx.platform == Platform::Linux {
                " [--backend hvx|kvm]"
            } else {
                ""
            }
        );
        return 0;
    }
    let mut args = argv.to_vec();
    let outdir = take(&mut args, "--out");
    let known = take(&mut args, "--known-errors");
    let no_diagnose = match args.iter().position(|a| a == "--no-diagnose") {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    };
    let memory = match args.iter().position(|a| a == "--memory") {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    };
    let json = match args.iter().position(|a| a == "--json") {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    };
    // Milestone 365: record the run's findings in the findings store at its end
    let record = match args.iter().position(|a| a == "--record-findings") {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    };
    let opts = RunOpts::take(ctx, &mut args);
    let image = find_image(ctx, &mut args);
    if args.len() != 1 {
        die("usage: hydo run [IMG] \"<schedule>\" [--image IMG] [--out DIR] [--known-errors FILE] [--memory] [--vcpus N] [--turn-margin N]   e.g. hydo run \"disk=cut | run C=12 R=everysec\"");
    }
    // Milestone 374: the schedule in its canonical text (`T=10:+120` is `T=10:120`), which the
    // guest sees and result.json records
    let sched = match hydo_schedule::Schedule::canonical(&args[0]) {
        Some(c) if c != args[0] => {
            println!("hydo: the schedule as hydo writes it: {c}");
            c
        }
        _ => args[0].clone(),
    };
    let outdir = outdir.map(PathBuf::from).unwrap_or_else(new_run_dir);
    let eargs = opts.eargs(&image);
    // Milestone 339: a guest that does not fit into this machine's memory is a refusal, exit
    // status 5 as for `hydo explore` (Milestone 328), with its JSON line under `--json`
    if let Some(why) = memory_refusal(ctx, &eargs) {
        refuse(&why, json);
    }
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
    let printed = verdict_of(&res);
    let res = host_checks(&outdir, res, false);
    let res = contracts_one(&image, &outdir, "one", &sched, res, false);
    let res = errors_one(&outdir, &sched, res, known.as_deref());
    if let Some(line) = final_verdict_line(&printed, &verdict_of(&res)) {
        println!("{line}");
    }
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
    for l in eventually_lines(&run) {
        println!("{l}");
    }
    // hvx keeps 64 KiB of a VM's console (the kernel module's buffer); hydo-hvx says in
    // engine.log when a VM filled it (hydo_hvx::engine::CONSOLE_FULL_MARK). Since Milestone 368
    // the run's console carries the note too, and the run's warnings say it; engine.log speaks
    // only for a console without the note.
    let elog = std::fs::read_to_string(outdir.join("engine.log")).unwrap_or_default();
    let noted = run
        .warnings
        .iter()
        .any(|w| hydo_explore::results::is_console_cut_warning(w));
    if let Some(l) = elog
        .lines()
        .find(|l| l.contains("hvx: console full"))
        .filter(|_| !noted)
    {
        println!(
            "hydo: warning: {}. Print less (short history records, no per-id lines), or run on \
             the KVM engine (--backend kvm), which keeps 32 MiB.",
            l.trim()
        );
    }
    print_warnings(&run);
    // M178: why a failed run failed, after everything the run recorded is written (the
    // evidence is read by a separate replay; `--no-diagnose` skips it)
    if run.verdict.kind != "ok" {
        if let Some(d) = crate::inspect::diagnose::for_run(ctx, &outdir, !no_diagnose) {
            println!("hydo: diagnosis: {}", d.summary);
            if let Some(l) = d.likely.first() {
                println!("hydo: likely: {l}");
            }
            println!(
                "hydo: details in {} (\"diagnosis\"); look inside: hydo inspect start {} --at end",
                outdir.join(hydo_results::RUN_DOC).display(),
                outdir.display()
            );
            let mut run = run;
            run.diagnosis = Some(d);
            write_run_document(&outdir, &run);
        }
    }
    if memory {
        print_memory(ctx, &outdir);
    }
    if record {
        crate::findings::record_after(ctx, &outdir, false);
    }
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
    let mut eargs = engine_args(
        &image,
        Some(e.ram_mib.unwrap_or(1024)),
        Some(e.cpu.clone().unwrap_or_else(|| "v2".into())),
        e.vcpus.unwrap_or(1),
    );
    // M205: the exploration's turn margin (recorded in its engine arguments)
    if let Some(m) = RunOpts::of_eargs(e.args.as_deref().unwrap_or(&[])).turn_margin {
        eargs.push("--turn-margin".into());
        eargs.push(m.to_string());
    }
    let mut out = Vec::new();
    for f in &doc.failures {
        out.push(ReproItem {
            // a turn sweep's failure has no seed: its tag names the turn length (M205)
            label: match (f.run.seed, f.run.tag.as_deref()) {
                (Some(s), _) => format!("seed {s}"),
                (None, Some(t)) => t.to_string(),
                (None, None) => "seed ".to_string(),
            },
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
            // the whole signature: cut at 40 characters it ended inside a key name (0.3.1)
            label: format!("minimal {}", x.signature),
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
            let res = contracts_one(&it.image, &dir, "one", &it.sched, res, true);
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
            print_warnings(&run);
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

/// The image's saved regressions and the plan meta they are checked against: inside the image
/// before the fingerprint epoch 4, beside it (`<image>.regressions.json`) from epoch 4 (M269).
fn image_regressions(image: &Path) -> hydo_control::agent::ImageRegressions {
    hydo_control::agent::regressions_of_image(image).unwrap_or_else(|e| die(&format!("hydo: {e}")))
}

/// One regression's pre-run check (M98). Returns why it must not run.
fn regression_precheck(r: &Value, meta: &Value) -> Option<String> {
    let name = py_str(&r["name"]);
    if meta.get("stable_compose").is_some_and(truthy) {
        let st = hydo_composer::regression_status(r, meta, None)
            .unwrap_or_else(|e| die(&format!("regression {name}: {e}")));
        for w in &st.warnings {
            println!("hydo: warning: {name}: {w}");
        }
        if let Some(e) = st.error.as_ref().filter(|e| !e.is_empty()) {
            println!(
                "FAIL  {}  expect {}  not run: {}\n      {}",
                name,
                py_str(&r["expect"]),
                e,
                py_str(&r["schedule"])
            );
            return Some(e.clone());
        }
    } else if let Some(w) = hydo_composer::regression_warning(r, meta)
        .unwrap_or_else(|e| die(&format!("regression {name}: {e}")))
    {
        println!("hydo: warning: {w}");
    }
    None
}

/// Milestone 355: why a regression whose `# params: k=v` line names build parameters does not run
/// on an image built with `built` (its `/etc/hydo/params`): the values it needs, the image's, and
/// the build that makes its image. None when the image has every value (or the regression names
/// none).
pub fn regression_params_differ(r: &Value, built: &[(String, String)]) -> Option<String> {
    let Value::Object(want) = r.get("params")? else {
        return None;
    };
    let mut other: Vec<String> = Vec::new();
    for (k, v) in want {
        let v = py_str(v);
        match built.iter().find(|(n, _)| n == k) {
            Some((_, h)) if *h == v => {}
            Some((_, h)) => other.push(format!("{k}={v} (this image: {k}={h})")),
            None => other.push(format!("{k}={v} (this image has no parameter {k})")),
        }
    }
    if other.is_empty() {
        return None;
    }
    let sets: Vec<String> = want
        .iter()
        .map(|(k, v)| format!("--set {k}={}", py_str(v)))
        .collect();
    Some(format!(
        "it needs the build parameters {}; build its image with `hydo build {}` and run `hydo \
         repro --regressions` on that image",
        other.join(", "),
        sets.join(" ")
    ))
}

/// Milestone 360: a regression that [`replay_regressions`] left out, because its `# params:`
/// line names other build parameters than the image's.
pub fn left_out(g: &hydo_results::CiRegression) -> bool {
    g.verdict.is_none()
        && g.error
            .as_deref()
            .is_some_and(|e| e.starts_with("not run: it needs the build parameters"))
}

/// Every saved regression an image carries, replayed from fresh boots into `base/<k>-<name>`, one
/// result each (`hydo repro --regressions`, `hydo ci run`). Prints a PASS or FAIL line per
/// regression.
pub fn replay_regressions(ctx: &Ctx, image: &Path, base: &Path) -> Vec<hydo_results::CiRegression> {
    let got = image_regressions(image);
    let (meta, regs) = (got.meta, got.regressions);
    let eargs = engine_args(image, None, None, 1);
    // Milestone 355: the image's build parameters, for a regression's `# params:` line
    let built: Vec<(String, String)> = hydo_engine::image::embedded_file(image, "etc/hydo/params")
        .map(|t| hydo_manifest::parse_params_file(&t))
        .unwrap_or_default();
    let mut out = Vec::new();
    for (k, r) in regs.iter().enumerate() {
        let name = py_str(&r["name"]);
        let sched = py_str(&r["schedule"]);
        let ex = py_str(&r["expect"]);
        if let Some(why) = regression_params_differ(r, &built) {
            println!("SKIP  {name}  expect {ex}  not run: {why}\n      {sched}");
            out.push(hydo_results::CiRegression {
                name,
                schedule: sched,
                expect: ex,
                as_expected: true,
                error: Some(format!("not run: {why}")),
                ..Default::default()
            });
            continue;
        }
        if let Some(e) = regression_precheck(r, &meta) {
            out.push(hydo_results::CiRegression {
                name,
                schedule: sched,
                expect: ex,
                error: Some(e),
                ..Default::default()
            });
            continue;
        }
        let dir = base.join(format!("{k:03}-{name}"));
        let (fp, v, error) =
            match run_engine_try(ctx, image, &sched, &dir, &eargs, true, &Policy::Schedule) {
                Ok((Some(res), _)) => {
                    let res = host_checks(&dir, result_map(&res), true);
                    (py_str(&res["fp"]), py_str(&res["verdict"]), None)
                }
                Ok((None, _)) => ("-".into(), "ERROR no RESULT line".into(), None),
                Err(w) => (
                    "-".into(),
                    format!("not run: {w}"),
                    Some(format!("not run (no VM was started): {w}")),
                ),
            };
        let ok = error.is_none() && regression_ok(&ex, &v, &fp);
        println!(
            "{:<5} {}  expect {}  fp={}  {}\n      {}",
            if ok { "PASS" } else { "FAIL" },
            name,
            ex,
            fp,
            sched,
            head(&v, 160)
        );
        out.push(hydo_results::CiRegression {
            name,
            schedule: sched,
            expect: ex,
            verdict: Some(v),
            fp: Some(fp),
            as_expected: ok,
            error,
        });
    }
    out
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
    let found = image_regressions(&image);
    let regs = found.regressions;
    if regs.is_empty() {
        match &found.file {
            None => println!(
                "hydo: {} carries no regressions (hydo/regressions/*.schedule or test.regressions in the plan)",
                image.display()
            ),
            // M269: an image of the fingerprint epoch 4 or later has them beside it
            Some(f) => println!(
                "hydo: {} has no saved regressions: there is no {} beside it (`hydo build` writes it from hydo/regressions/*.schedule and test.regressions)",
                image.display(),
                f.display()
            ),
        }
        return 0;
    }
    let base = image
        .parent()
        .unwrap_or(Path::new(""))
        .join(format!("regressions-{}", strftime("%Y%m%d-%H%M%S")));
    let got = replay_regressions(ctx, &image, &base);
    // a regression the host controller refuses stops the command, as before `hydo ci run`
    if let Some(e) = got.iter().find_map(|r| r.error.as_deref()) {
        if e.starts_with("not run (no VM") {
            die(e);
        }
    }
    let bad = got.iter().filter(|r| !r.as_expected).count();
    // Milestone 355: the regressions of other build parameters, which did not run here
    let skipped = got
        .iter()
        .filter(|r| {
            r.error
                .as_deref()
                .is_some_and(|e| e.starts_with("not run: it needs the build parameters"))
        })
        .count();
    let others = if skipped > 0 {
        format!(", {skipped} left out: they need other build parameters")
    } else {
        String::new()
    };
    println!(
        "hydo: {} of {} regressions as expected{others} (consoles under {})",
        regs.len() - bad - skipped,
        regs.len() - skipped,
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

/// Where `hydo save` writes without `--to`: `hydo/regressions` from the repository's root, and
/// `regressions` from inside the `hydo/` folder itself (a `hydo.yaml` here and no
/// `hydo/hydo.yaml`), where `hydo run` and `hydo explore` also work. Before, a save from inside
/// the folder wrote a second `hydo/hydo/regressions` that no build read (the Valkey onboarding).
fn default_regressions_dir(cwd: &Path) -> String {
    if cwd.join("hydo.yaml").is_file() && !cwd.join("hydo").join("hydo.yaml").is_file() {
        "regressions".into()
    } else {
        "hydo/regressions".into()
    }
}

/// The image an exploration ran: its resolved path, else the path as given, relative to the
/// folder it was run in or to the install's guest folder.
fn exploration_image(ctx: &Ctx, doc: &hydo_results::Exploration) -> Option<PathBuf> {
    let mut c: Vec<PathBuf> = Vec::new();
    if let Some(r) = &doc.image.resolved {
        c.push(PathBuf::from(r));
    }
    let p = PathBuf::from(&doc.image.path);
    c.push(p.clone());
    if !p.is_absolute() {
        c.push(ctx.guest().join(&p));
    }
    c.into_iter().find(|p| p.is_file())
}

/// Milestone 360: the build parameters of an exploration's image, for the `# params:` line of the
/// regressions `hydo save` writes. Before, the trial of Milestone 359 added the line by hand, and
/// a regression found with `--set memq=10000` ran on every build. Milestone 363: also the
/// defaults, so that a regression found on the default build is left out on another build.
/// `Err`: the image has `params:` and was built again since the exploration. An image that is
/// gone, or a manifest without `params:`, gives no line.
fn exploration_params(
    ctx: &Ctx,
    doc: &hydo_results::Exploration,
) -> Result<Vec<(String, String)>, String> {
    let Some(img) = exploration_image(ctx, doc) else {
        // nothing to read: the files keep the form they had before Milestone 360
        return Ok(Vec::new());
    };
    let Ok(text) = hydo_engine::image::embedded_manifest(&img) else {
        return Ok(Vec::new());
    };
    let defaults: Vec<(String, String)> = match hydo_manifest::parse_text(&text, "hydo.yaml", true)
    {
        Ok(m) => match m.get("params") {
            Some(Value::Object(o)) => o
                .iter()
                .map(|(k, v)| {
                    let v = match v {
                        Value::Null => String::new(),
                        Value::String(s) => s.clone(),
                        other => py_str(other),
                    };
                    (k.clone(), v)
                })
                .collect(),
            _ => return Ok(Vec::new()),
        },
        Err(_) => return Ok(Vec::new()),
    };
    if let (Some(want), Ok(got)) = (doc.image.sha256.as_deref(), sha256_file(&img)) {
        if want != got {
            return Err(format!(
                "its image {} was built again since the exploration",
                img.display()
            ));
        }
    }
    let built = hydo_engine::image::embedded_file(&img, "etc/hydo/params")
        .map(|t| hydo_manifest::parse_params_file(&t))
        .unwrap_or_default();
    Ok(params_line_values(&defaults, &built))
}

/// The values of a `# params:` line: every value the image was built with, sorted, when the
/// manifest declares `params:` (`defaults`), also the defaults (Milestone 363: the trial's 5
/// regressions of `memq=0` ran on the `memq=10000` build without a word). A value with white
/// space cannot be one word of the line and is left out; the build's check then does not hold
/// the regression back for it.
fn params_line_values(
    defaults: &[(String, String)],
    built: &[(String, String)],
) -> Vec<(String, String)> {
    if defaults.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<(String, String)> = built
        .iter()
        .filter(|(_, val)| !val.chars().any(char::is_whitespace))
        .cloned()
        .collect();
    v.sort();
    v
}

/// The schedule and the `# params:` words of a saved regression's text: two files of one
/// finding have the same.
fn regression_identity(text: &str) -> (String, String) {
    let mut params = String::new();
    let mut sched = String::new();
    for l in text.lines() {
        let l = l.trim();
        if let Some(p) = l.strip_prefix("# params:") {
            params = p.split_whitespace().collect::<Vec<_>>().join(" ");
        } else if !l.is_empty() && !l.starts_with('#') {
            sched = l.to_string();
        }
    }
    (sched, params)
}

/// Where `hydo save` writes a finding named `name`: the first of `name`, `name-2`, `name-3`, ...
/// that is free, or that already holds the same finding (then `false`: nothing to write). Before
/// Milestone 360 a second finding with the name of an earlier file was dropped with `exists,
/// kept`.
fn save_path(to: &str, name: &str, text: &str) -> (String, bool) {
    let id = regression_identity(text);
    let mut k = 1;
    loop {
        let n = if k == 1 {
            name.to_string()
        } else {
            format!("{name}-{k}")
        };
        let p = pjoin(to, &format!("{n}.schedule"));
        match std::fs::read_to_string(&p) {
            Ok(old) if regression_identity(&old) == id => return (p, false),
            Err(_) if !Path::new(&p).exists() => return (p, true),
            _ => k += 1,
        }
    }
}

/// What makes two shrinks of an exploration one regression for `hydo save` (Milestone 367):
/// the class, the finding's statement and key as `hydo ci` and the configurations table name it
/// (a violation by its check, so `put x7f was lost` and `put a3c was lost` are one), and the
/// fault kinds of `--shrink-causes`, which keep the shrinks of other causes apart. None for a
/// shrink whose verdict names no finding.
fn save_key(
    doc: &hydo_results::Exploration,
    s: &hydo_results::Shrink,
) -> Option<(String, String, String, Option<String>)> {
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
    let (kind, title, k) = crate::ci::key::of_verdict(&verdict, &fails)?;
    Some((
        s.class.clone(),
        crate::ci::key::statement(&kind, &title),
        k,
        s.fault_kinds.clone(),
    ))
}

pub fn cmd_save(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    let to = take(&mut args, "--to").unwrap_or_else(|| default_regressions_dir(Path::new(".")));
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
    let params = match exploration_params(ctx, &doc) {
        Ok(v) => v,
        Err(why) => {
            println!(
                "hydo: warning: the build parameters of the exploration's image were not read \
                 ({why}); add a `# params: name=value` line to each file if it was built with \
                 `--set`"
            );
            Vec::new()
        }
    };
    let params_line = (!params.is_empty()).then(|| {
        let kv: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("# params: {}", kv.join(" "))
    });
    let (mut n, mut kept, mut same) = (0, 0, 0);
    // Milestone 367: one regression per class and finding. The SeaweedFS configurations
    // exploration shrank five power cuts of one class whose verdicts named other objects, and
    // `hydo save` wrote five files of one finding; the configurations table showed one
    for s in &doc.shrinks {
        if let Some(k) = save_key(&doc, s) {
            if let Some(seed) = doc
                .shrinks
                .iter()
                .take_while(|x| !std::ptr::eq(*x, s))
                .find(|x| save_key(&doc, x).as_ref() == Some(&k))
                .map(|x| x.from_seed)
            {
                println!(
                    "hydo: the shrink of seed {} is another failure of the finding of seed {seed} \
                     ({} {}), left out",
                    s.from_seed, k.0, k.1
                );
                same += 1;
                continue;
            }
        }
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
        let mut text = hydo_composer::saved_regression_text(&x)
            .unwrap_or_else(|e| die(&format!("{}: {e}", args[0])));
        if let Some(l) = &params_line {
            // after the line that names the seed and the class
            text = match text.split_once('\n') {
                Some((first, rest)) => format!("{first}\n{l}\n{rest}"),
                None => format!("{l}\n{text}"),
            };
        }
        let (p, new) = save_path(&to, name, &text);
        if !new {
            println!("hydo: {p} holds this finding already, kept");
            kept += 1;
            continue;
        }
        if let Err(e) = std::fs::write(&p, text) {
            die(&format!("{p}: {e}"));
        }
        n += 1;
        println!("hydo: wrote {p}");
        if truthy(&x["stable_note"]) {
            println!("hydo: note: {}", py_str(&x["stable_note"]));
        }
    }
    let kept = if kept > 0 {
        format!(", {kept} kept: they were saved before")
    } else {
        String::new()
    };
    let kept = if same > 0 {
        format!("{kept}, {same} left out: another failure of a finding saved above")
    } else {
        kept
    };
    // Milestone 387: a problem reported before its shrink has no minimal schedule to save yet
    let mut open: Vec<(String, String)> = Vec::new();
    for f in &doc.failures {
        if hydo_results::shrink_state::is_open(f.shrink_state.as_deref()) {
            let g = (
                hydo_explore::advice::shrink_group(f.run.class.as_deref().unwrap_or(""))
                    .to_string(),
                f.signature.clone(),
            );
            if !open.contains(&g) {
                open.push(g);
            }
        }
    }
    if !open.is_empty() {
        println!(
            "hydo: {} problem{} not shrunk yet, so not saved: `hydo shrink {} --problem N` \
             (N as `hydo report` numbers it) shrinks one, then `hydo save` again",
            open.len(),
            if open.len() == 1 { " is" } else { "s are" },
            crate::inspect::work::quote(&args[0])
        );
    }
    let with = match &params_line {
        Some(l) => format!(" with `{l}` (the image's build parameters)"),
        None => String::new(),
    };
    println!("hydo: {n} regressions saved{with}{kept}; `hydo build` writes them beside the image, `hydo repro --regressions` replays them");
    0
}

#[cfg(test)]
mod tests {
    use super::final_verdict_line;

    /// Milestone 355: a regression that names build parameters runs only on an image built with
    /// them, and the reason names the build that makes its image.
    #[test]
    fn a_regression_of_other_build_parameters_names_its_image() {
        use super::regression_params_differ;
        let r = serde_json::json!({"name": "wait", "params": {"mode": "master"}});
        let master = [("mode".to_string(), "master".to_string())];
        let wait = [("mode".to_string(), "wait".to_string())];
        assert_eq!(regression_params_differ(&r, &master), None);
        assert_eq!(
            regression_params_differ(&r, &wait).as_deref(),
            Some(
                "it needs the build parameters mode=master (this image: mode=wait); build its \
                 image with `hydo build --set mode=master` and run `hydo repro --regressions` on \
                 that image"
            )
        );
        assert!(regression_params_differ(&r, &[])
            .unwrap()
            .contains("this image has no parameter mode"));
        let plain = serde_json::json!({"name": "x"});
        assert_eq!(regression_params_differ(&plain, &wait), None);
    }

    /// Milestone 360: `hydo save` names the values of `--set` on a `# params:` line, and gives a
    /// second finding of one name a file of its own instead of dropping it. Milestone 363: the
    /// line names the defaults too, when the manifest has `params:`.
    #[test]
    fn save_keeps_every_finding_and_names_the_build_parameters() {
        let kv = |a: &[(&str, &str)]| -> Vec<(String, String)> {
            a.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let defaults = kv(&[("memq", "0"), ("sync", "yes")]);
        // the default build: its values, so that another build leaves the regression out
        assert_eq!(super::params_line_values(&defaults, &defaults), defaults);
        assert_eq!(
            super::params_line_values(&defaults, &kv(&[("sync", "yes"), ("memq", "10000")])),
            kv(&[("memq", "10000"), ("sync", "yes")])
        );
        // a regression saved from the default build is left out on the `--set memq=10000` build
        let saved = serde_json::json!({"params": {"memq": "0", "sync": "yes"}});
        let other = kv(&[("memq", "10000"), ("sync", "yes")]);
        assert!(super::regression_params_differ(&saved, &other)
            .unwrap()
            .contains("memq=0 (this image: memq=10000)"));
        assert_eq!(super::regression_params_differ(&saved, &defaults), None);
        // a manifest without params: gives no line
        assert!(super::params_line_values(&[], &[]).is_empty());
        // a value with white space cannot be a word of the line
        assert_eq!(
            super::params_line_values(&defaults, &kv(&[("memq", "0"), ("sync", "a b")])),
            kv(&[("memq", "0")])
        );
        let d = std::env::temp_dir().join(format!("hydo-save-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let to = d.to_string_lossy().to_string();
        let a = "# found by `hydo explore` (seed 2): process\n# expect: violation\ndisk=none | run K=x@1+1\n";
        let b = "# found by `hydo explore` (seed 9): process\n# expect: violation\ndisk=none | run A=e:kill-x\n";
        let (p, new) = super::save_path(&to, "process-v", a);
        assert!(new && p.ends_with("process-v.schedule"));
        std::fs::write(&p, a).unwrap();
        // the same finding again: kept, nothing written
        assert_eq!(super::save_path(&to, "process-v", a), (p.clone(), false));
        // another finding of the same name: the next free name
        let (p2, new) = super::save_path(&to, "process-v", b);
        assert!(new && p2.ends_with("process-v-2.schedule"), "{p2}");
        std::fs::write(&p2, b).unwrap();
        // the first finding with other build parameters is another finding
        let a10 = a.replacen('\n', "\n# params: memq=10000\n", 1);
        let (p3, new) = super::save_path(&to, "process-v", &a10);
        assert!(new && p3.ends_with("process-v-3.schedule"), "{p3}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Milestone 367: the shrinks of one class whose verdicts name other objects of one check
    /// are one regression; another check, another class, or another cause is another.
    #[test]
    fn save_takes_one_shrink_per_class_and_finding() {
        let shrink = |class: &str, verdict: &str, kinds: Option<&str>| hydo_results::Shrink {
            class: class.into(),
            signature: verdict.into(),
            verdict: verdict.into(),
            fault_kinds: kinds.map(String::from),
            ..Default::default()
        };
        let doc = hydo_results::Exploration::default();
        let a = shrink(
            "durable-defaults",
            "VIOLATION [objects.acked-outcome] put fa3c was acknowledged and has no final state",
            None,
        );
        let b = shrink(
            "durable-defaults",
            "VIOLATION [objects.acked-outcome] put be7d was acknowledged and has no final state",
            None,
        );
        let ka = super::save_key(&doc, &a).unwrap();
        assert_eq!(ka.1, "VIOLATION [objects.acked-outcome]");
        assert_eq!(Some(&ka), super::save_key(&doc, &b).as_ref());
        let other_check = shrink(
            "durable-defaults",
            "VIOLATION [objects.no-loss] m3 lost",
            None,
        );
        assert_ne!(Some(&ka), super::save_key(&doc, &other_check).as_ref());
        let other_class = shrink(
            "process",
            "VIOLATION [objects.acked-outcome] put fa3c was acknowledged and has no final state",
            None,
        );
        assert_ne!(Some(&ka), super::save_key(&doc, &other_class).as_ref());
        let other_cause = shrink(
            "durable-defaults",
            "VIOLATION [objects.acked-outcome] put fa3c was acknowledged and has no final state",
            Some("C disk=cut"),
        );
        assert_ne!(Some(&ka), super::save_key(&doc, &other_cause).as_ref());
        assert_eq!(
            super::save_key(
                &doc,
                &shrink("x", "HYDO-ORACLE: OK invariants=1 held (x: y)", None)
            ),
            None
        );
    }

    /// `hydo save` from inside the `hydo/` folder writes into its `regressions/`, not into a
    /// nested `hydo/hydo/regressions` (the Valkey onboarding's friction).
    #[test]
    fn save_writes_into_the_folder_it_runs_in() {
        let d = std::env::temp_dir().join(format!("hydo-save-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("hydo")).unwrap();
        // the repository's root: hydo/hydo.yaml below it
        std::fs::write(d.join("hydo").join("hydo.yaml"), "format: 1\n").unwrap();
        assert_eq!(super::default_regressions_dir(&d), "hydo/regressions");
        // inside the hydo/ folder
        assert_eq!(
            super::default_regressions_dir(&d.join("hydo")),
            "regressions"
        );
        // anywhere else
        std::fs::create_dir_all(d.join("other")).unwrap();
        assert_eq!(
            super::default_regressions_dir(&d.join("other")),
            "hydo/regressions"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The RESULT line prints the guest's checks only; when the host's checks change the verdict,
    /// `hydo run` prints the final one (the libSQL onboarding's friction item 8).
    /// `hydo run` prints why a run took a second pass (0.3.1, the Mac reader's finding).
    #[test]
    fn the_rewind_note_is_printed() {
        let out = "hvmx: one: ended at a counter loop (spin)\nnoise\nhydo: the run ended as SPIN; \
                   ran it again in the engine's rewind mode (disk=none spin=rewind | run)\nRESULT one\n";
        assert_eq!(
            super::engine_notes(out),
            vec![
                "hvmx: one: ended at a counter loop (spin)",
                "hydo: the run ended as SPIN; ran it again in the engine's rewind mode (disk=none spin=rewind | run)"
            ]
        );
    }

    #[test]
    fn the_final_verdict_is_printed_when_the_hosts_checks_changed_it() {
        let g = "HYDO-ORACLE: OK invariants=1 held (x)";
        assert_eq!(final_verdict_line(g, g), None);
        let v = "HYDO-ORACLE: VIOLATION [sql.lap] lost-append";
        assert_eq!(
            final_verdict_line(g, v).unwrap(),
            "hydo: VERDICT HYDO-ORACLE: VIOLATION [sql.lap] lost-append   (the run's verdict \
             with the host's checks, as result.json has it; the RESULT line above has the \
             guest's checks only)"
        );
    }

    /// M205: `--vcpus` and `--turn-margin` become engine arguments, and a run's recorded
    /// arguments give them back (`hydo repro <run dir>`, `hydo explain <run dir>`).
    #[test]
    fn the_vcpu_options_go_through_the_recorded_engine_arguments() {
        use super::{Platform, RunOpts};
        let o = RunOpts {
            vcpus: Some(2),
            turn_margin: Some(160),
        };
        let a = ["--ram-mib", "2048", "--vcpus", "2", "--turn-margin", "160"].map(String::from);
        assert_eq!(RunOpts::of_eargs(&a), o);
        assert_eq!(RunOpts::of_eargs(&[]), RunOpts::default());
        // one vCPU is the default, written or not
        let one = ["--vcpus", "1"].map(String::from);
        assert_eq!(RunOpts::of_eargs(&one), RunOpts::default());
        assert!(o.check(Platform::Linux, false).is_ok());
        // the KVM engine runs several vCPUs (docs/design/kvm-vcpus.md); its turns need no margin
        assert!(o
            .check(Platform::Linux, true)
            .unwrap_err()
            .contains("need no margin"));
        let kvm2 = RunOpts {
            vcpus: Some(2),
            turn_margin: None,
        };
        assert!(kvm2.check(Platform::Linux, true).is_ok());
        assert!(o
            .check(Platform::Mac, false)
            .unwrap_err()
            .contains("Mac engine"));
        let bad = |v, m| RunOpts {
            vcpus: Some(v),
            turn_margin: m,
        };
        assert!(bad(9, None).check(Platform::Linux, false).is_err());
        assert!(bad(0, None).check(Platform::Linux, false).is_err());
        assert!(bad(1, Some(160)).check(Platform::Linux, false).is_err());
        assert!(bad(2, Some(4)).check(Platform::Linux, false).is_err());
    }

    /// Milestone 368: of two images in one folder (a build of a copy of the manifest with
    /// `--set`), the one built from hydo.yaml, else the newest.
    #[test]
    fn of_two_images_the_one_of_hydo_yaml_or_the_newest() {
        use super::pick_image;
        use std::path::Path;
        let dir = std::env::temp_dir().join(format!("hydo-pick-image-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("explore-ob-a-disk.img");
        let b = dir.join("explore-ob-b-disk.img");
        let rec = |img: &Path, m: &str| {
            std::fs::write(img, b"x").unwrap();
            std::fs::write(
                format!("{}.json", img.display()),
                format!("{{\"manifest\":{{\"path\":\"{m}\",\"sha256\":\"0\"}}}}"),
            )
            .unwrap();
        };
        rec(&a, "hydo/hydo.yaml");
        std::thread::sleep(std::time::Duration::from_millis(20));
        rec(&b, "hydo/hydo-batch.yaml");
        let imgs = vec![a.clone(), b.clone()];
        assert_eq!(pick_image(&imgs), (a.clone(), "built from hydo.yaml"));
        rec(&a, "hydo/hydo-other.yaml");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&b, b"y").unwrap();
        assert_eq!(pick_image(&imgs), (b.clone(), "the newest"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
