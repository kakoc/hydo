//! `hydo-bench <command> [options]`: the command lines of run.py, confirm.py, listrun.py,
//! depth1map.py, d7prob.py and addbuggify.py (options by their full names, `--opt value` or
//! `--opt=value`).

use crate::cells::{self, Cell};
use crate::defects::{check_kind, Set};
use crate::pools::Engines;
use crate::{analysis, buggify, confirm, listrun, rebaseline};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage: hydo-bench <command> [options]   (hydo/bench/README.md)

  run      --strategy random|swarm|swarm-pos|process [--new-faults] [--buggify] [--bg-images]
           [--concurrency] [--more-faults --cf-range LO-HI] [--reactive fault|heal|both]
           [--knob-rule cut|any] [--vcpus N [--turns]]
           [--window N] [--defects all|m59|d1-...,...] [--reps 1-3] [--budget 60] [--workers 3]
           --out DIR | --dry
               cells of strategy x defect x repetitions; appends DIR/results.jsonl (--dry: print
               each seed's schedule and start no VM)
  table    DIR...                       the detection table of finished cells (run --table DIR... too)
  confirm  --out DIR [--defects all|m59|...] [--workers 3] [--no-control] [--base] [--bg-images] [--vcpus N]
               every defect's trigger schedules twice on its image and once on its control
  list     --images IMG[,IMG...] --list FILE --out DIR [--workers 3] [--vcpus N] [--repeat 1|2] [--env K=V]...
           [--bg-images]
               a fixed list of schedules on each image (IMG: an image file, a defect name for its
               image of the kind, or control:<defect> for its control)
  depth1map DEFECT-SWEEP-DIR CONTROL-SWEEP-DIR [--sig REGEX] [--json FILE]
  d7prob   [--seeds 200000]
  addbuggify MAIN.GO [--check]
  rebaseline [--check | --write [--accept-verdicts]] [--thorough] [--only NAME[,NAME...]]
           [--platform hvx|hvm|kvm] [--jobs 2 (Linux) | 4 (Mac)] [--changed-only] [--build-only]
           [--reuse-builds | --no-build] [--work DIR] [--inputs DIR] [--release DIR] [--list FILE]
           [--timeout 3600] [--env K=V]...
               every recorded example run of this machine's platform (hydo/onboard/recorded.json).
               Default (fast): an image whose inputs are those of the build kept in the work
               folder is not built again, else built once; a schedule with a cached result for
               the image's bytes and the engine is not run; the others run once (one fresh boot
               per image, the rest in a worker pool from the provisioned snapshot, where the
               first schedule runs too and must give its fresh boot's values), a second
               time only when the first did not give the recorded values. --thorough (releases,
               nightly): every image built twice and compared, every schedule run twice from
               fresh boots, nothing reused. A report of verdict changes, nondeterminism and
               unreproducible builds (docs/REBASELINE.md \"How to re-baseline\"); exit 1 on
               any of those, 3 when only values moved and --check did not write them
Images are read from $HYDO_BUILD (default ~/hydo/build): the manifest-built agent images
explore-ob-bench-<defect>-agent-disk.img (hydo/bench/build-bench-manifest.sh). --kind takes only agent (the
build-stack.sh images, --kind stack, were deleted in Milestone 145; --kind shell in Milestone 136).
The hvx engine runs in this process when it can open /dev/hvx (run hydo-bench as ~/bin/hvx-run sudo ...);
otherwise every VM is the program hydo-hvx ($HYDO_HVX_EXPLORE) started through ~/bin/hvx-run ($HYDO_HVX_RUN).";

/// Options as (name, value); flags get "".
struct Opts {
    named: Vec<(String, String)>,
    free: Vec<String>,
}

const FLAGS: [&str; 18] = [
    "--new-faults",
    "--buggify",
    "--bg-images",
    "--concurrency",
    "--more-faults",
    "--turns",
    "--no-control",
    "--base",
    "--check",
    "--table",
    "--dry",
    "--write",
    "--accept-verdicts",
    "--changed-only",
    "--reuse-builds",
    "--no-build",
    "--build-only",
    "--thorough",
];

fn parse(argv: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        named: Vec::new(),
        free: Vec::new(),
    };
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if let Some(rest) = a.strip_prefix("--") {
            if let Some((k, v)) = rest.split_once('=') {
                o.named.push((format!("--{k}"), v.to_string()));
            } else if FLAGS.contains(&a.as_str()) {
                o.named.push((a.clone(), String::new()));
            } else {
                let v = argv
                    .get(i + 1)
                    .ok_or_else(|| format!("{a} needs a value"))?;
                o.named.push((a.clone(), v.clone()));
                i += 1;
            }
        } else {
            o.free.push(a.clone());
        }
        i += 1;
    }
    Ok(o)
}

impl Opts {
    fn get(&self, k: &str) -> Option<&str> {
        self.named
            .iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }
    fn all(&self, k: &str) -> Vec<String> {
        self.named
            .iter()
            .filter(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .collect()
    }
    fn flag(&self, k: &str) -> bool {
        self.named.iter().any(|(n, _)| n == k)
    }
    fn num<T: std::str::FromStr>(&self, k: &str, d: T) -> Result<T, String> {
        match self.get(k) {
            None => Ok(d),
            Some(v) => v.parse().map_err(|_| format!("{k}: not a number: {v}")),
        }
    }
    fn check(&self, allowed: &[&str]) -> Result<(), String> {
        for (n, _) in &self.named {
            if !allowed.contains(&n.as_str()) {
                return Err(format!("unknown option {n}"));
            }
        }
        Ok(())
    }
}

fn range(s: &str) -> Result<(i64, i64), String> {
    let bad = || format!("{s}: expected N or LO-HI");
    match s.split_once('-') {
        Some((a, b)) => Ok((
            a.trim().parse().map_err(|_| bad())?,
            b.trim().parse().map_err(|_| bad())?,
        )),
        None => {
            let n = s.trim().parse().map_err(|_| bad())?;
            Ok((n, n))
        }
    }
}

fn vcpus(o: &Opts) -> Result<u32, String> {
    let v: u32 = o.num("--vcpus", 1)?;
    if !(1..=8).contains(&v) {
        return Err("--vcpus must be 1..8".into());
    }
    Ok(v)
}

fn cmd_run(root: &Path, cfg: &hydo_config::Config, o: &Opts) -> Result<(), String> {
    if o.flag("--table") {
        print!(
            "{}",
            cells::table(&o.free.iter().map(PathBuf::from).collect::<Vec<_>>())?
        );
        return Ok(());
    }
    o.check(&[
        "--strategy",
        "--knob-rule",
        "--new-faults",
        "--buggify",
        "--bg-images",
        "--concurrency",
        "--more-faults",
        "--cf-range",
        "--vcpus",
        "--turns",
        "--window",
        "--defects",
        "--reps",
        "--budget",
        "--workers",
        "--out",
        "--kind",
        "--reactive",
        "--dry",
    ])?;
    let strategy = o
        .get("--strategy")
        .ok_or("--strategy is required (random, swarm, swarm-pos or process)")?;
    if !["random", "swarm", "swarm-pos", "process"].contains(&strategy) {
        return Err(format!(
            "--strategy {strategy}: random, swarm, swarm-pos or process (portfolio needs the archived portfolio.py; restore it from git, see docs/HISTORY.md M108)"
        ));
    }
    let knob = o.get("--knob-rule").unwrap_or("cut");
    if knob != "cut" && knob != "any" {
        return Err("--knob-rule: cut or any".into());
    }
    let v = vcpus(o)?;
    let turns = o.flag("--turns");
    if turns && v < 2 {
        return Err("--turns needs --vcpus N > 1".into());
    }
    let more = o.flag("--more-faults");
    let cf = match o.get("--cf-range") {
        Some(r) => Some(range(r)?),
        None => None,
    };
    if more && cf.is_none() {
        return Err("--more-faults needs --cf-range LO-HI (the FLUSH count of the control: explore --schedule \"disk=cut dseed=0 cfn | run\")".into());
    }
    // M139: reactions (A=) on top of the strategy's schedules, from their own stream
    let reactive = o.get("--reactive").map(str::to_string);
    if let Some(r) = &reactive {
        if !["fault", "heal", "both"].contains(&r.as_str()) {
            return Err(format!("--reactive {r}: fault, heal or both"));
        }
    }
    let (lo, hi) = range(o.get("--reps").unwrap_or("1-3"))?;
    let dry = o.flag("--dry");
    let out = match o.get("--out") {
        Some(d) => d,
        None if dry => "",
        None => return Err("--out is required".into()),
    };
    let c = Cell {
        strategy: strategy.to_string(),
        knob_rule_any: knob == "any",
        new_faults: o.flag("--new-faults"),
        buggify: o.flag("--buggify"),
        concurrency: o.flag("--concurrency"),
        more_faults: more,
        cf_range: if more { cf } else { None },
        reactive,
        vcpus: v,
        turns,
        window: o.num("--window", 0)?,
        reps: (lo..=hi).collect(),
        budget: o.num("--budget", 60)?,
        workers: o.num("--workers", 3)?,
        out: PathBuf::from(out),
    };
    if dry {
        // M139: the cell's schedules, one `<seed> <schedule>` line each; no VM is started
        let ex = c.ex();
        for &rep in &c.reps {
            let seeds: Vec<i64> = (rep * 1000 + 1..=rep * 1000 + c.budget).collect();
            for (sd, text) in cells::schedules(&ex, &c.strategy, &seeds)? {
                println!("{sd} {text}");
            }
        }
        return Ok(());
    }
    check_kind(o.get("--kind"))?;
    let set = Set::new(o.flag("--bg-images"));
    let names = Set::names(o.get("--defects").unwrap_or("all"))?;
    let eng = Engines::new(root, v, Vec::new(), cfg);
    cells::run_cell(&c, &set, &eng, &names)
}

fn cmd_confirm(root: &Path, cfg: &hydo_config::Config, o: &Opts) -> Result<(), String> {
    o.check(&[
        "--out",
        "--defects",
        "--workers",
        "--no-control",
        "--vcpus",
        "--base",
        "--bg-images",
        "--kind",
    ])?;
    let out = o.get("--out").ok_or("--out is required")?;
    check_kind(o.get("--kind"))?;
    let set = Set::new(o.flag("--bg-images"));
    let names = Set::names(o.get("--defects").unwrap_or("all"))?;
    let eng = Engines::new(root, vcpus(o)?, Vec::new(), cfg);
    confirm::confirm(
        &set,
        &eng,
        &names,
        Path::new(out),
        &confirm::Opts {
            workers: o.num("--workers", 3)?,
            no_control: o.flag("--no-control"),
            base: o.flag("--base"),
        },
    )
}

/// One `list --images` entry: an image file name (`….img`) as given, a defect name as that
/// defect's image of the selected kind, `control:<defect>` as its control.
fn list_image(set: &Set, i: &str) -> Result<String, String> {
    if i.ends_with(".img") {
        return Ok(i.to_string());
    }
    let (control, name) = match i.strip_prefix("control:") {
        Some(n) => (true, n),
        None => (false, i),
    };
    if !Set::known(name) {
        return Err(format!(
            "--images {i}: an image file (….img), a defect name or control:<defect>"
        ));
    }
    Ok(if control {
        set.control_of(name)
    } else {
        set.image_of(name)
    })
}

fn cmd_list(root: &Path, cfg: &hydo_config::Config, o: &Opts) -> Result<(), String> {
    o.check(&[
        "--images",
        "--list",
        "--workers",
        "--out",
        "--vcpus",
        "--repeat",
        "--env",
        "--kind",
        "--bg-images",
    ])?;
    check_kind(o.get("--kind"))?;
    let set = Set::new(o.flag("--bg-images"));
    let images = o
        .get("--images")
        .ok_or("--images is required")?
        .split(',')
        .map(|i| list_image(&set, i))
        .collect::<Result<Vec<String>, String>>()?;
    let lp = o.get("--list").ok_or("--list is required")?;
    let text = std::fs::read_to_string(lp).map_err(|e| format!("{lp}: {e}"))?;
    let out = o.get("--out").ok_or("--out is required")?;
    let repeat: u32 = o.num("--repeat", 1)?;
    if repeat != 1 && repeat != 2 {
        return Err("--repeat: 1 or 2".into());
    }
    let eng = Engines::new(root, vcpus(o)?, o.all("--env"), cfg);
    let t = listrun::run(
        &eng,
        &images,
        &listrun::items_of(&text),
        o.num("--workers", 3)?,
        Path::new(out),
        repeat,
    )?;
    print!("{t}");
    Ok(())
}

fn cmd_rebaseline(root: &Path, cfg: &hydo_config::Config, o: &Opts) -> Result<i32, String> {
    o.check(&[
        "--check",
        "--write",
        "--accept-verdicts",
        "--only",
        "--platform",
        "--jobs",
        "--changed-only",
        "--reuse-builds",
        "--no-build",
        "--build-only",
        "--thorough",
        "--work",
        "--inputs",
        "--release",
        "--list",
        "--timeout",
        "--env",
    ])?;
    if o.flag("--check") && o.flag("--write") {
        return Err("--check and --write exclude each other".into());
    }
    if o.flag("--accept-verdicts") && !o.flag("--write") {
        return Err("--accept-verdicts goes with --write".into());
    }
    if o.flag("--reuse-builds") && o.flag("--no-build") {
        return Err("--reuse-builds and --no-build exclude each other".into());
    }
    if !o.free.is_empty() {
        return Err(format!(
            "rebaseline takes no arguments: {}",
            o.free.join(" ")
        ));
    }
    let platform = o
        .get("--platform")
        .unwrap_or(rebaseline::default_platform())
        .to_string();
    if !["hvx", "hvm", "kvm"].contains(&platform.as_str()) {
        return Err(format!("--platform {platform}: hvx, hvm or kvm"));
    }
    let abs = |p: &str| -> PathBuf {
        let p = PathBuf::from(p);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir().unwrap_or_default().join(p)
        }
    };
    let work = o.get("--work").map(abs).unwrap_or_else(|| {
        cfg.env
            .home()
            .join(".cache")
            .join("hydo-rebaseline")
            .join(&platform)
    });
    let extra_env = o
        .all("--env")
        .iter()
        .map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or(format!("--env {kv}: KEY=VALUE"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let opts = rebaseline::Opts {
        root: root.canonicalize().unwrap_or(root.to_path_buf()),
        list: o
            .get("--list")
            .map(abs)
            .unwrap_or_else(|| root.join("hydo").join("onboard").join("recorded.json")),
        inputs: o
            .get("--inputs")
            .map(abs)
            .unwrap_or_else(|| work.join("in")),
        work,
        release: o.get("--release").map(abs),
        platform,
        only: o
            .get("--only")
            .map(|s| s.split(',').map(str::to_string).collect())
            .unwrap_or_default(),
        write: o.flag("--write"),
        accept_verdicts: o.flag("--accept-verdicts"),
        // at most two VMs at a time on Linux (the bare-metal machine's rule), four on a Mac
        jobs: o.num("--jobs", if cfg!(target_os = "macos") { 4 } else { 2 })?,
        changed_only: o.flag("--changed-only"),
        reuse_builds: o.flag("--reuse-builds"),
        no_build: o.flag("--no-build"),
        build_only: o.flag("--build-only"),
        thorough: o.flag("--thorough"),
        config: cfg.clone(),
        timeout: o.num("--timeout", 3600)?,
        quiet_file: cfg.pool.quiet_file.clone(),
        extra_env,
    };
    rebaseline::rebaseline(&opts)
}

fn cmd_depth1map(o: &Opts) -> Result<(), String> {
    o.check(&["--sig", "--json"])?;
    let [d, c] = o.free.as_slice() else {
        return Err("depth1map DEFECT-SWEEP-DIR CONTROL-SWEEP-DIR".into());
    };
    let (text, json) =
        analysis::depth1map(Path::new(d), Path::new(c), o.get("--sig").unwrap_or(""))?;
    print!("{text}");
    if let Some(j) = o.get("--json") {
        std::fs::write(j, json).map_err(|e| format!("{j}: {e}"))?;
    }
    Ok(())
}

fn cmd_addbuggify(o: &Opts) -> Result<(), String> {
    o.check(&["--check"])?;
    let [path] = o.free.as_slice() else {
        return Err("addbuggify MAIN.GO [--check]".into());
    };
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    // Python's text mode read the file with universal newlines
    let src = raw.replace("\r\n", "\n").replace('\r', "\n");
    let (out, counts) = buggify::rewrite(&src);
    println!("{}", buggify::report(path, &counts));
    if !o.flag("--check") && out != src {
        std::fs::write(path, out).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(())
}

/// The entry point: the exit status. `cfg` is the configuration the program read at its start.
pub fn main_with(argv: &[String], root: &Path, cfg: &hydo_config::Config) -> i32 {
    let Some((cmd, rest)) = argv.split_first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let o = match parse(rest) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("hydo-bench: {e}\n{USAGE}");
            return 2;
        }
    };
    let r = match cmd.as_str() {
        "run" => cmd_run(root, cfg, &o),
        "table" => cells::table(&o.free.iter().map(PathBuf::from).collect::<Vec<_>>())
            .map(|t| print!("{t}")),
        "confirm" => cmd_confirm(root, cfg, &o),
        "list" => cmd_list(root, cfg, &o),
        "depth1map" => cmd_depth1map(&o),
        "d7prob" => o
            .check(&["--seeds"])
            .and_then(|_| o.num("--seeds", 200000i64))
            .map(|n| print!("{}", analysis::d7prob(n))),
        "addbuggify" => cmd_addbuggify(&o),
        "rebaseline" => {
            return match cmd_rebaseline(root, cfg, &o) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("hydo-bench: {e}");
                    if e.starts_with("unknown option")
                        || e.contains("exclude each other")
                        || e.contains("goes with")
                        || e.starts_with("--")
                    {
                        2
                    } else {
                        1
                    }
                }
            }
        }
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other}")),
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("hydo-bench: {e}");
            1
        }
    }
}
