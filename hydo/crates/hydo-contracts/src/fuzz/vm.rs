//! Function fuzzing inside hydo's virtual machine (Milestone 292, docs/design/function-fuzzing.md,
//! "Inside the virtual machine"). The native driver's generator, harness and judge, with the calls
//! made in a guest under a schedule of faults:
//!
//! - the harness is the copy's test program built for the guest (`x86_64-unknown-linux-musl` on
//!   hvx and KVM, `aarch64-unknown-linux-musl` on the Mac engine), statically linked;
//! - hydo writes a project whose workload makes the calls, one workload step per batch, each call
//!   in a process of its own and in an empty folder of its own on the data disk. `hydo build` makes
//!   the image, and `hydo run` runs it under each schedule. The answers come back on the console;
//! - a function with `// hydo: durable <check>` gets a second phase: the check runs in the
//!   recovery boot after a power cut (or at the end of a run without one), once for every call
//!   that started, with the call's input and its result (`None` for a call that did not return);
//! - a failing call shrinks alone in a run of one step under the same faults, and its run
//!   replays: the image holds the inputs and the schedule holds the faults.
//!
//! The module calls `hydo build` and `hydo run` as programs, so that hydo-contracts stays free of
//! the engines.

use super::{cargo, describe, value, FTy, Failure, Fn, Outcome, Pass};
use crate::plan::ClauseKind;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use value::{Rng, Val};

/// `hydo fuzz --vm --help`.
pub const USAGE: &str =
    "usage: hydo fuzz --vm [CRATE_DIR] [--fn NAME]... [--cases N] [--seed S] [--schedules N]
                      [--schedule S]... [--faults KIND,...] [--batch N] [--out DIR]
                      [--toolchain TC] [--release] [--timeout SECS] [--shrink-runs N]
                      [--data-mib N] [--run-timeout SECS] [--backend hvx|kvm]
  tests each contracted Rust function of the crate inside hydo's virtual machine, under faults.
  It builds the crate's harness for the guest, writes a project whose workload makes the calls
  (one workload step per batch of --batch calls, each call in an empty folder of its own on the
  data disk), builds its image with `hydo build`, and runs it under one schedule after the other
  with `hydo run`. Each call is judged by `ensures`, as natively. A function with
  `// hydo: durable <check>` also gets its check called after the run, in the recovery boot
  after a power cut, once for each call that started: what the call promised before the cut
  must hold after it. A fault inside a call comes from a reaction: the call that the schedule
  names (E=<step>.<call>) reports its start, and A=fuzz.call+<ms>:<action> acts that long after
  it. A failing call is shrunk alone under the same faults, and replays from its image and
  schedule. `hydo guide fuzz` has the comment lines.
  --cases N        inputs per function that keep `requires` (default 200)
  --seed S         the seed of the inputs and of the schedules (default 1)
  --schedules N    schedules to run (default 12): the first has no fault, the others are drawn
  --schedule S     run this schedule only (repeat it for more), e.g. 'disk=cut | run C=4'
  --faults K,...   the kinds the drawn schedules take: cut (C=, cf= on hvx, and a cut inside a
                   call), disk (disk=ioerr), clock (T=, and a clock step inside a call), pause
                   (P=workload, and a pause of the call's process or of a thread it started),
                   kill (the call's process killed inside the call) (default all five)
  --batch N        calls per workload step (default 25)
  --timeout SECS   a call that takes longer in guest time is stopped (default 10)
  --shrink-runs N  runs that one shrink may take (default 16)
  --data-mib N     the data disk's size (default 256)
  --run-timeout S  a `hydo run` that takes longer on this machine is stopped, and the fuzzing
                   goes on (default 600 plus 2 per call of the image)
  --backend B      passed to `hydo run`
  The exit status is 1 when a clause or a check broke or a call failed, 2 for a mistake in the
  options or the contracts, and 0 otherwise.";

/// The guest triple of this host's engine.
pub fn guest_triple() -> &'static str {
    if std::env::consts::ARCH == "aarch64" {
        "aarch64-unknown-linux-musl"
    } else {
        "x86_64-unknown-linux-musl"
    }
}

/// The writer whose `durable` line names `f`, when one does (`lines`: writer, check).
pub fn checked_by(f: &Fn, lines: &[(String, String)]) -> Option<String> {
    lines
        .iter()
        .find(|(_, c)| names(&f.name, c))
        .map(|(w, _)| w.clone())
}

/// Whether a function's full name is `want` or ends with `::want`.
fn names(full: &str, want: &str) -> bool {
    full == want || full.ends_with(&format!("::{want}"))
}

struct Opts {
    dir: PathBuf,
    fns: Vec<String>,
    cases: u64,
    seed: u64,
    schedules: u64,
    fixed: Vec<String>,
    faults: Vec<String>,
    batch: usize,
    out: PathBuf,
    toolchain: Option<String>,
    release: bool,
    timeout: u64,
    shrink_runs: u64,
    data_mib: u64,
    /// The host's limit on one `hydo run`, in seconds (0: 600 plus 2 per call).
    run_timeout: u64,
    backend: Option<String>,
}

const KINDS: [&str; 5] = ["cut", "disk", "clock", "pause", "kill"];

fn opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        dir: PathBuf::from("."),
        fns: vec![],
        cases: 200,
        seed: 1,
        schedules: 12,
        fixed: vec![],
        faults: KINDS.iter().map(|s| s.to_string()).collect(),
        batch: 25,
        out: PathBuf::from("hydo-out/fuzz-vm"),
        toolchain: None,
        release: false,
        timeout: 10,
        shrink_runs: 16,
        data_mib: 256,
        run_timeout: 0,
        backend: None,
    };
    let mut it = args.iter();
    let mut dir_set = false;
    while let Some(a) = it.next() {
        let mut val = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        let num = |v: String| -> Result<u64, String> {
            v.replace('_', "")
                .parse()
                .map_err(|_| format!("{a} {v}: not a whole number"))
        };
        match a.as_str() {
            "--vm" => {}
            "--fn" => o.fns.push(val()?),
            "--cases" => o.cases = num(val()?)?,
            "--seed" => o.seed = num(val()?)?,
            "--schedules" => o.schedules = num(val()?)?.max(1),
            "--schedule" => o.fixed.push(val()?),
            "--faults" => {
                let v = val()?;
                o.faults = v.split(',').map(|s| s.trim().to_string()).collect();
                if let Some(bad) = o.faults.iter().find(|k| !KINDS.contains(&k.as_str())) {
                    return Err(format!(
                        "--faults {v}: `{bad}` is not a kind; the kinds are {}",
                        KINDS.join(", ")
                    ));
                }
            }
            "--batch" => o.batch = num(val()?)?.clamp(1, 1000) as usize,
            "--out" => o.out = PathBuf::from(val()?),
            "--toolchain" => o.toolchain = Some(val()?),
            "--release" => o.release = true,
            "--timeout" => o.timeout = num(val()?)?.max(1),
            "--shrink-runs" => o.shrink_runs = num(val()?)?,
            "--data-mib" => o.data_mib = num(val()?)?.max(64),
            "--run-timeout" => o.run_timeout = num(val()?)?.max(10),
            "--backend" => o.backend = Some(val()?),
            s if s.starts_with('-') => {
                return Err(format!("unknown option {s} (hydo fuzz --vm --help)"))
            }
            s => {
                if dir_set {
                    return Err(format!("one crate folder only: {s}"));
                }
                o.dir = PathBuf::from(s);
                dir_set = true;
            }
        }
    }
    Ok(o)
}

/// `hydo fuzz --vm ...`.
pub fn main(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let o = match opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("hydo fuzz: {e}");
            return 2;
        }
    };
    match run_all(&o) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hydo fuzz: {e}");
            2
        }
    }
}

/// One call of the layout: the function (an index into the writers), its step and its place in
/// the step's batch, and its input.
#[derive(Clone)]
struct Call {
    w: usize,
    step: usize,
    k: usize,
    args: Vec<Val>,
}

/// A function to fuzz with the harness module of its own and of its check.
struct Writer {
    /// Index into the built functions.
    f: usize,
    /// The check's index into the built functions.
    check: Option<usize>,
}

/// What the guest answered for one call, read from the consoles.
#[derive(Clone, Debug, Default)]
struct Answers {
    calls: BTreeMap<(usize, usize), Outcome>,
    /// Each check's answer, with the result the guest handed it (`None`: the call did not
    /// return).
    checks: BTreeMap<(usize, usize), (Option<Val>, Outcome)>,
    /// The checks ran in a recovery boot after a power cut.
    after_cut: bool,
    /// The call whose process the schedule killed (Milestone 296).
    killed: Option<(usize, usize)>,
}

/// One run of an image under a schedule.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RunInfo {
    pub schedule: String,
    pub dir: String,
    pub image: String,
    pub verdict: String,
    pub fingerprint: String,
    pub power_cut: bool,
    pub calls: u64,
    pub checks: u64,
    /// Milestone 296: for a fault inside a call, whether its reaction acted, and on which call.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub in_call: String,
}

/// A failure over the runs, for the report and `vm.json`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct VmFinding {
    pub function: String,
    /// `call` (a clause of the function, a panic, a call that did not return) or `check` (the
    /// durability check after the run).
    pub phase: String,
    pub what: String,
    /// Calls that failed this way, over all runs.
    pub count: u64,
    /// Runs in which it failed.
    pub runs: u64,
    pub first_schedule: String,
    pub input: String,
    pub gave: String,
    /// The smallest input that failed the same way, and its run.
    pub smallest: String,
    pub shrink_steps: u64,
    pub replay_image: String,
    pub replay_schedule: String,
    pub replay_call: String,
    /// The replay of the smallest input's run gave the same fingerprint.
    pub replayed: bool,
    pub note: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct VmReport {
    pub package: String,
    pub triple: String,
    pub functions: Vec<String>,
    pub cases: u64,
    pub runs: Vec<RunInfo>,
    pub findings: Vec<VmFinding>,
    pub held: Vec<String>,
    pub skipped: Vec<String>,
}

/// The key of a failure: the writer, the phase (false: the call; true: its check), and how.
type Key = (usize, bool, Failure);

fn run_all(o: &Opts) -> Result<i32, String> {
    let started = std::time::Instant::now();
    let triple = guest_triple();
    let cg = cargo::Cargo {
        toolchain: o.toolchain.clone(),
        release: o.release,
    };
    let pkg = cargo::metadata(&cg, &o.dir)?;
    let sources = pkg.sources();
    let (all, skipped, errors) = super::find_all(&pkg.dir, &sources, &pkg.name);
    for e in &errors {
        eprintln!("hydo fuzz: {e}");
    }
    if !errors.is_empty() {
        return Ok(2);
    }
    // the functions to fuzz and their checks, in one list for the build
    let mut built: Vec<Fn> = Vec::new();
    let mut writers: Vec<Writer> = Vec::new();
    let mut notes: Vec<String> = skipped
        .iter()
        .map(|s| format!("{} ({}:{}): {}", s.name, s.file.display(), s.line, s.why))
        .collect();
    let all_names: Vec<&str> = all.iter().map(|f| f.name.as_str()).collect();
    let want = super::wanted_names(&o.fns, &all_names);
    let (plain, checks) = super::split_checks(all, &sources);
    let mut checks: Vec<Option<Fn>> = checks.into_iter().map(|(c, _)| Some(c)).collect();
    for f in plain {
        if !super::named(&f, &want) {
            continue;
        }
        let mut check = None;
        if let Some((name, line)) = f.contract.durable.clone() {
            let at = checks
                .iter()
                .position(|c| c.as_ref().is_some_and(|c| names(&c.name, &name)));
            match at {
                None => {
                    return Err(format!(
                        "{}:{line}: `durable {name}`: no function `{name}` with a contract that \
                         hydo can call; write `// hydo: ensures result` before it",
                        f.file.display()
                    ))
                }
                Some(i) => {
                    let c = checks[i].take().unwrap_or_else(|| unreachable!());
                    check_signature(&f, &c)
                        .map_err(|e| format!("{}:{line}: {e}", f.file.display()))?;
                    built.push(c);
                    check = Some(built.len() - 1);
                }
            }
        }
        built.push(f);
        writers.push(Writer {
            f: built.len() - 1,
            check,
        });
    }
    if writers.is_empty() {
        return Err(if o.fns.is_empty() {
            format!(
                "no function of {} can be fuzzed (`hydo fuzz --list` says why)",
                pkg.dir.display()
            )
        } else {
            format!("no function named {} can be fuzzed", o.fns.join(", "))
        });
    }
    let out = abs(&o.out)?;
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let copy = out.join("crate");
    if copy.exists() {
        std::fs::remove_dir_all(&copy).map_err(|e| format!("{}: {e}", copy.display()))?;
    }
    std::fs::create_dir_all(&copy).map_err(|e| e.to_string())?;
    cargo::copy_package(&pkg, &copy)?;
    let first = pkg
        .target_of(&pkg.dir.join(&built[writers[0].f].file))
        .ok_or("the function is in no library or binary of the package")?
        .clone();
    std::fs::write(copy.join("Cargo.toml"), cargo::manifest(&pkg, &first))
        .map_err(|e| e.to_string())?;
    // the harness for the guest: a static musl program
    let mut flags = String::from("-C target-feature=+crt-static");
    if cfg!(target_os = "macos") {
        flags.push_str(" -C linker=rust-lld -C link-self-contained=yes");
    }
    std::env::set_var("CARGO_BUILD_TARGET", triple);
    if triple.starts_with("aarch64") {
        std::env::set_var("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS", &flags);
    } else {
        std::env::set_var("CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS", &flags);
    }
    eprintln!(
        "hydo: fuzz: building the harness of {} function{} for {triple} ({})",
        built.len(),
        if built.len() == 1 { "" } else { "s" },
        copy.display()
    );
    let t0 = std::time::Instant::now();
    let _ = std::fs::remove_file(out.join("build.log"));
    let _ = std::fs::remove_dir_all(out.join("harness"));
    let (exe, active, failed) = super::build_each(
        &cg,
        &pkg,
        &copy,
        &out.join("target"),
        &first,
        &out.join("build.log"),
        &super::Set {
            fns: &built,
            others: &[],
        },
    )?;
    std::env::remove_var("CARGO_BUILD_TARGET");
    for (k, why) in &failed {
        notes.push(format!(
            "{}: its harness did not build: {why}",
            built[*k].name
        ));
    }
    let Some(exe) = exe else {
        return Err(format!(
            "no harness built (cargo's output: {})",
            out.join("build.log").display()
        ));
    };
    eprintln!(
        "hydo: fuzz: the harness built in {} s",
        t0.elapsed().as_secs()
    );
    writers.retain(|w| active.contains(&w.f));
    for w in &mut writers {
        if w.check.is_some_and(|c| !active.contains(&c)) {
            notes.push(format!(
                "{}: its durability check did not build, so it runs without one",
                built[w.f].name
            ));
            w.check = None;
        }
    }
    if writers.is_empty() {
        return Err("no function's harness built".into());
    }
    // the inputs
    let mut calls: Vec<Call> = Vec::new();
    let mut step = 0;
    for (wi, w) in writers.iter().enumerate() {
        let f = &built[w.f];
        let mut rng = Rng::new(o.seed ^ hash(&f.name));
        let (mut n, mut tries) = (0u64, 0usize);
        let mut k_in = 0;
        while n < o.cases && tries < (o.cases as usize).saturating_mul(50).max(100) {
            let a = f.generate(&mut rng, tries);
            tries += 1;
            if !f.admits(&a) {
                continue;
            }
            calls.push(Call {
                w: wi,
                step,
                k: k_in,
                args: a,
            });
            n += 1;
            k_in += 1;
            if k_in == o.batch {
                k_in = 0;
                step += 1;
            }
        }
        if k_in > 0 {
            step += 1;
        }
    }
    let steps = step.max(1);
    let mut ctx = Ctx {
        o,
        built: &built,
        writers: &writers,
        exe: &exe,
        copy: &copy,
        out: &out,
        explore_image: PathBuf::new(),
        explore_calls: calls.clone(),
        explore_fps: BTreeMap::new(),
    };
    let image = ctx.image("explore", &calls, steps)?;
    ctx.explore_image = image.clone();
    // the schedules
    let places: Vec<(usize, usize)> = calls.iter().map(|c| (c.step, c.k)).collect();
    let scheds: Vec<String> = if o.fixed.is_empty() {
        draw_schedules(o.seed, o.schedules, &o.faults, steps, &places, cf_ok(o))
    } else {
        o.fixed.clone()
    };
    let mut report = VmReport {
        package: pkg.name.clone(),
        triple: triple.into(),
        functions: writers.iter().map(|w| built[w.f].name.clone()).collect(),
        cases: calls.len() as u64,
        skipped: notes,
        ..VmReport::default()
    };
    let mut found: BTreeMap<Key, (u64, u64, usize, Call, Outcome)> = BTreeMap::new();
    // Milestone 296: each run's first call that failed each way, for a shrink under another
    // schedule when the first one does not shrink
    let mut others: BTreeMap<Key, Vec<(usize, Call)>> = BTreeMap::new();
    let mut judged: BTreeMap<(usize, usize), u64> = BTreeMap::new();
    for (si, s) in scheds.iter().enumerate() {
        let (info, ans) = ctx.run(&image, s, &format!("s{si:03}"), &calls)?;
        ctx.explore_fps.insert(s.clone(), info.fingerprint.clone());
        println!(
            "hydo: fuzz: run {}/{}: {s}: {} calls, {} checks{}{}: {}",
            si + 1,
            scheds.len(),
            info.calls,
            info.checks,
            if info.power_cut {
                " after the power cut"
            } else {
                ""
            },
            match info.in_call.as_str() {
                "" => String::new(),
                m => format!(", {m}"),
            },
            // the invariant only calls the checks, and the host judges their answers
            info.verdict.trim_end_matches(" (fuzz.checks: )")
        );
        let mut seen_here: Vec<Key> = Vec::new();
        for c in &calls {
            for (key, o2) in ctx.judge(c, &ans, &mut judged) {
                let e = found
                    .entry(key.clone())
                    .or_insert((0, 0, si, c.clone(), o2));
                e.0 += 1;
                if !seen_here.contains(&key) {
                    e.1 += 1;
                    others.entry(key.clone()).or_default().push((si, c.clone()));
                    seen_here.push(key);
                }
            }
        }
        report.runs.push(info);
    }
    // the findings, shrunk
    for (key, (count, runs, si, call, gave)) in &found {
        let (w, is_check, x) = key;
        let wr = &writers[*w];
        let f = &built[wr.f];
        let what = if *is_check {
            let c = &built[wr.check.unwrap_or(wr.f)];
            format!("{}: {}", c.name, describe(c, x))
        } else {
            describe(f, x)
        };
        let sched = &scheds[*si];
        println!(
            "hydo: {} {}: {what} on {count} call{} in {runs} run{}, first under `{sched}`",
            if *is_check {
                "durability check broken:"
            } else {
                "contract broken:"
            },
            f.name,
            if *count == 1 { "" } else { "s" },
            if *runs == 1 { "" } else { "s" },
        );
        let shown_gave = if *is_check {
            built[wr.check.unwrap_or(wr.f)].show_after(gave)
        } else {
            f.show_after(gave)
        };
        println!("hydo:   input: {}", f.show_args(&call.args));
        println!("hydo:   gave: {shown_gave}");
        if *si == 0 && o.fixed.is_empty() {
            // the first schedule has no fault
            println!(
                "hydo:   it failed without a fault too: the function or its check is wrong \
                 before any fault meets it"
            );
        }
        // a schedule whose input does not fail alone (a timed fault of the whole run) gives way to
        // the next one under which the call failed, up to three schedules (Milestone 296)
        let mut sh = ctx.shrink(key, call, sched)?;
        let mut tried = 1;
        for (sj, cj) in others.get(key).into_iter().flatten().skip(1) {
            if tried >= 3 || !sh.note.starts_with("not shrunk") {
                break;
            }
            tried += 1;
            let again = ctx.shrink(key, cj, &scheds[*sj])?;
            if !again.note.starts_with("not shrunk") {
                println!(
                    "hydo:   shrunk under `{}`, where it failed too; alone in one step, the \
                     input passed under the first schedule",
                    scheds[*sj]
                );
                sh = again;
            }
        }
        let mut fnd = VmFinding {
            function: f.name.clone(),
            phase: if *is_check { "check" } else { "call" }.into(),
            what,
            count: *count,
            runs: *runs,
            first_schedule: sched.clone(),
            input: f.show_args(&call.args),
            gave: shown_gave,
            smallest: f.show_args(&sh.args),
            shrink_steps: sh.steps,
            replay_image: sh.image.display().to_string(),
            replay_schedule: sh.schedule.clone(),
            replay_call: format!("step {} call {}", sh.step, sh.k),
            replayed: false,
            note: sh.note.clone(),
        };
        // the replay: the same image and schedule give the same fingerprint
        let (again, _) = ctx.run(
            &sh.image,
            &sh.schedule,
            &format!("{}-again", sh.tag),
            &sh.calls,
        )?;
        fnd.replayed = !again.fingerprint.is_empty() && again.fingerprint == sh.fingerprint;
        println!(
            "hydo:   smallest input ({} shrink step{}): {}",
            sh.steps,
            if sh.steps == 1 { "" } else { "s" },
            fnd.smallest
        );
        if !sh.note.is_empty() {
            println!("hydo:   {}", sh.note);
        }
        println!(
            "hydo:   again: hydo run {} '{}'   ({}; {} the same fingerprint, {})",
            fnd.replay_image,
            fnd.replay_schedule,
            fnd.replay_call,
            if fnd.replayed { "gave" } else { "did NOT give" },
            sh.fingerprint
        );
        report.findings.push(fnd);
    }
    // the clauses that held
    for (wi, w) in writers.iter().enumerate() {
        let f = &built[w.f];
        for (ci, c) in f.contract.clauses.iter().enumerate() {
            if c.kind != ClauseKind::Ensures {
                continue;
            }
            if found
                .keys()
                .any(|(a, b, x)| *a == wi && !*b && *x == Failure::Clause(ci))
            {
                continue;
            }
            let n = judged.get(&(wi, 0)).copied().unwrap_or(0);
            let line = format!(
                "{}: held: ensures {} (line {}) on {n} calls",
                f.name, c.text, c.line
            );
            println!("hydo: fuzz: {line}");
            report.held.push(line);
        }
        if let Some(ci) = w.check {
            if !found.keys().any(|(a, b, _)| *a == wi && *b) {
                let n = judged.get(&(wi, 1)).copied().unwrap_or(0);
                let after = judged.get(&(wi, 2)).copied().unwrap_or(0);
                let line = format!(
                    "{}: held: the durability check {} on {n} calls, {after} of them after a power cut",
                    f.name, built[ci].name
                );
                println!("hydo: fuzz: {line}");
                report.held.push(line);
            }
        }
    }
    let j = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(out.join("vm.json"), format!("{j}\n")).map_err(|e| e.to_string())?;
    println!(
        "hydo: fuzz: {} calls of {} function{} in {} run{}, {} finding{}, {} s; the report is {}",
        calls.len(),
        writers.len(),
        if writers.len() == 1 { "" } else { "s" },
        report.runs.len(),
        if report.runs.len() == 1 { "" } else { "s" },
        report.findings.len(),
        if report.findings.len() == 1 { "" } else { "s" },
        started.elapsed().as_secs(),
        out.join("vm.json").display()
    );
    Ok(if report.findings.is_empty() { 0 } else { 1 })
}

/// The check takes the function's parameters, then the call's result as an `Option` (`None`
/// for a call that did not return), and returns `bool` or `Option<bool>`.
fn check_signature(f: &Fn, c: &Fn) -> Result<(), String> {
    let want = || {
        let mut p: Vec<String> = f
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty.rust()))
            .collect();
        p.push(format!("done: Option<{}>", f.ret.rust()));
        format!("fn {}({}) -> Option<bool>", c.short_name(), p.join(", "))
    };
    let ok = c.params.len() == f.params.len() + 1
        && c.params.iter().zip(&f.params).all(|(a, b)| a.ty == b.ty)
        && c.params.last().is_some_and(|p| {
            p.ty == FTy::Opt(Box::new(f.ret.clone())) && matches!(p.pass, Pass::Value)
        })
        && (c.ret == FTy::Bool || c.ret == FTy::Opt(Box::new(FTy::Bool)));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "the durability check `{}` takes the parameters of `{}` and then the call's result: \
             {}",
            c.short_name(),
            f.short_name(),
            want()
        ))
    }
}

fn abs(p: &Path) -> Result<PathBuf, String> {
    Ok(if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map_err(|e| e.to_string())?.join(p)
    })
}

fn hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// The schedules of an exploration: the first without a fault, then one drawn kind each, in
/// turn. `calls` are the places (step, call) of the calls; `cf` says whether the engine applies
/// `cf=` (hvx).
///
/// Since Milestone 296 a schedule may put its fault inside one call: the schedule names a drawn
/// call with `E=<step>.<call>`, that call reports its start as the event `fuzz.call`, and the
/// reaction `A=fuzz.call+<ms>:<action>` acts that long after it. The actions are a power cut, a
/// clock step, a stop of the workload's tree (the call's process and the workload's shell), a
/// freeze of the first thread that the call started (`hydo-call#2`), and a kill of the call's
/// process. Of each kind's schedules in turn: a cut comes between steps (`C=`), inside a call,
/// and at a flush (`cf=`, hvx) or inside a call; a clock step or a pause comes inside a call
/// twice and between steps once; a kill always comes inside a call.
pub fn draw_schedules(
    seed: u64,
    n: u64,
    kinds: &[String],
    steps: usize,
    calls: &[(usize, usize)],
    cf: bool,
) -> Vec<String> {
    let mut out = vec!["disk=none | run".to_string()];
    let mut r = Rng::new(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5eed);
    let steps = steps as u64;
    let mut tries = 0;
    const DELAYS: [u64; 7] = [0, 2, 5, 10, 20, 50, 100];
    const SECS: [i64; 8] = [-86_400, -3_600, -300, -30, 30, 300, 3_600, 86_400];
    while (out.len() as u64) < n && !kinds.is_empty() && tries < n * 20 {
        tries += 1;
        let kind = &kinds[(out.len() - 1) % kinds.len()];
        // the how-manieth schedule of its kind
        let nth = (out.len() - 1) / kinds.len();
        let inside = !calls.is_empty()
            && match kind.as_str() {
                "cut" => nth % 3 == 1 || (nth % 3 == 2 && !cf),
                _ => nth % 3 != 2,
            };
        let dseed = r.word();
        // a step in the second half of the run, so that most calls are made before the fault
        let late = |r: &mut Rng| steps / 2 + 1 + r.below(steps - steps / 2);
        // a call, and the reaction's head that acts `ms` after its start
        let react = |r: &mut Rng| -> String {
            let (s, k) = calls[r.below(calls.len() as u64) as usize];
            let ms = DELAYS[r.below(DELAYS.len() as u64) as usize];
            let delay = if ms == 0 {
                String::new()
            } else {
                format!("+{ms}")
            };
            format!("E={s}.{k} A={CALL_EVENT}{delay}")
        };
        let s = match kind.as_str() {
            "cut" if inside => format!("disk=cut dseed={dseed:016x} | run {}:cut", react(&mut r)),
            "cut" if cf && nth % 3 == 2 => format!(
                "disk=cut dseed={dseed:016x} cf={} | run",
                1 + r.below((calls.len() as u64 * 8).max(8))
            ),
            "cut" => format!("disk=cut dseed={dseed:016x} | run C={}", late(&mut r)),
            "disk" => {
                let p = 1 + r.below(3);
                if r.below(2) == 0 {
                    format!("disk=ioerr:{p} dseed={dseed:016x} | run")
                } else {
                    format!("disk=ioerr:{p} dseed={dseed:016x} | run C={}", late(&mut r))
                }
            }
            "clock" if inside => {
                let head = react(&mut r);
                let d = SECS[r.below(SECS.len() as u64) as usize];
                let d = if d > 0 {
                    format!("+{d}")
                } else {
                    d.to_string()
                };
                format!("disk=none | run {head}:clock{d}")
            }
            "clock" => format!(
                "disk=none | run T={}:{}",
                r.below(steps),
                SECS[r.below(SECS.len() as u64) as usize]
            ),
            "pause" if inside => {
                let head = react(&mut r);
                let dur = [50u64, 500, 3_000][r.below(3) as usize];
                if r.below(2) == 0 {
                    format!("disk=none | run {head}:stop-workload-{dur}")
                } else {
                    format!("disk=none | run {head}:stop-workload/{CALL_THREAD}#2-{dur}")
                }
            }
            "kill" if !calls.is_empty() => format!(
                "disk=none | run {}:kill-workload/{CALL_PROCESS}",
                react(&mut r)
            ),
            "kill" => continue,
            _ => {
                let dur = [50u64, 500, 3_000][r.below(3) as usize];
                format!(
                    "disk=none | run P=workload@{}~{}:{dur}",
                    r.below(steps),
                    r.below(2_000)
                )
            }
        };
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// The schedule of a run of one step, from a schedule whose failing call was at `step`: a step
/// token before or at it moves to step 0, one after it to step 1 (the end of the one step), and
/// the call that `E=` names becomes call 0 of step 0; a reaction (`A=`) keeps its event. `cf=`
/// counts flushes from the workload's start, so a schedule with it stays as it is.
pub fn one_step_schedule(sched: &str, step: usize) -> String {
    let (host, guest) = sched.split_once('|').unwrap_or((sched, "run"));
    let mut words: Vec<String> = Vec::new();
    for w in guest.split_whitespace() {
        let moved = |n: &str| -> String {
            let n: usize = n.parse().unwrap_or(0);
            if n <= step {
                "0".into()
            } else {
                "1".into()
            }
        };
        if let Some(c) = w.strip_prefix("C=") {
            let n: usize = c.parse().unwrap_or(0);
            words.push(format!("C={}", if n <= step { 0 } else { 1 }));
        } else if w.starts_with("E=") {
            // Milestone 296: the call alone is call 0 of step 0
            words.push("E=0.0".into());
        } else if let Some(rest) = w.strip_prefix("T=") {
            let (n, v) = rest.split_once(':').unwrap_or((rest, ""));
            words.push(format!("T={}:{v}", moved(n)));
        } else if let Some(rest) = w.strip_prefix("P=workload@") {
            let cut = rest.find(['~', '+']).unwrap_or(rest.len());
            let n: usize = rest[..cut].parse().unwrap_or(0);
            if n == step {
                words.push(format!("P=workload@0{}", &rest[cut..]));
            } else if n < step {
                // a pause before the call's step: it ended before the call
            } else {
                words.push(format!("P=workload@1{}", &rest[cut..]));
            }
        } else {
            words.push(w.to_string());
        }
    }
    format!("{} | {}", host.trim(), words.join(" "))
}

/// Whether the engine applies `cf=`: hvx only (Milestone 296: the Mac engine and KVM get the
/// power cuts of `C=` and of a reaction instead).
fn cf_ok(o: &Opts) -> bool {
    match o.backend.as_deref() {
        Some("hvx") => true,
        Some(_) => false,
        None => cfg!(target_os = "linux") && Path::new("/dev/hvx").exists(),
    }
}

/// The event that the call named by `E=` reports when it starts (Milestone 296).
pub const CALL_EVENT: &str = "fuzz.call";

/// The name that the named call gives its thread; the threads it starts inherit it.
pub const CALL_THREAD: &str = "hydo-call";

/// The name of the program of the call that `E=` names (`/tmp/fuzz-call`, a link to
/// `/fuzz/harness`), which a kill inside a call names: a kill that comes after the call's end
/// meets no other call.
pub const CALL_PROCESS: &str = "fuzz-call";

/// The fault inside a call of a schedule (Milestone 296): the call `E=<step>.<call>` names, and
/// whether the reaction kills the call's process (then the call's own answer is not judged: the
/// schedule ended it).
pub fn in_call(sched: &str) -> Option<((usize, usize), bool)> {
    let guest = sched.split_once('|').map(|(_, g)| g).unwrap_or("");
    let mut at = None;
    let mut kill = false;
    for w in guest.split_whitespace() {
        if let Some(v) = w.strip_prefix("E=") {
            let (s, k) = v.split_once('.')?;
            at = Some((s.parse().ok()?, k.parse().ok()?));
        } else if let Some(r) = w.strip_prefix(&format!("A={CALL_EVENT}")) {
            kill = r.ends_with(&format!(":kill-workload/{CALL_PROCESS}"));
        }
    }
    at.map(|a| (a, kill))
}

/// The schedule with its `E=` naming the call `k` of step `s` instead.
pub fn with_call(sched: &str, s: usize, k: usize) -> String {
    let (host, guest) = sched.split_once('|').unwrap_or((sched, "run"));
    let words: Vec<String> = guest
        .split_whitespace()
        .map(|w| {
            if w.starts_with("E=") {
                format!("E={s}.{k}")
            } else {
                w.to_string()
            }
        })
        .collect();
    format!("{} | {}", host.trim(), words.join(" "))
}

/// Run a command with a limit on this machine's time: on the limit its process group is
/// killed. Ok(None) for a command that ran out of time.
fn output_within(c: &mut Command, secs: u64) -> std::io::Result<Option<std::process::Output>> {
    use std::os::unix::process::CommandExt as _;
    let dir = std::env::temp_dir();
    let tag = format!(
        "hydo-fuzz-vm-{}-{}",
        std::process::id(),
        hash(&format!("{c:?}"))
    );
    let (op, ep) = (
        dir.join(format!("{tag}.out")),
        dir.join(format!("{tag}.err")),
    );
    let mut child = c
        .process_group(0)
        .stdout(std::fs::File::create(&op)?)
        .stderr(std::fs::File::create(&ep)?)
        .spawn()?;
    let t0 = std::time::Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if t0.elapsed().as_secs() >= secs {
            // the whole group: `hydo run` and the engine it started
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .status();
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let out = std::fs::read(&op).unwrap_or_default();
    let err = std::fs::read(&ep).unwrap_or_default();
    let _ = std::fs::remove_file(&op);
    let _ = std::fs::remove_file(&ep);
    Ok(status.map(|status| std::process::Output {
        status,
        stdout: out,
        stderr: err,
    }))
}

struct Ctx<'a> {
    o: &'a Opts,
    built: &'a [Fn],
    writers: &'a [Writer],
    exe: &'a Path,
    copy: &'a Path,
    out: &'a Path,
    /// The exploration's image, its calls, and the fingerprint of each schedule's run of it.
    explore_image: PathBuf,
    explore_calls: Vec<Call>,
    explore_fps: BTreeMap<String, String>,
}

/// What a shrink ended with.
struct Shrunk {
    args: Vec<Val>,
    /// The calls of the replay's image.
    calls: Vec<Call>,
    steps: u64,
    image: PathBuf,
    schedule: String,
    step: usize,
    k: usize,
    fingerprint: String,
    tag: String,
    note: String,
}

impl Ctx<'_> {
    fn explore_fp(&self, sched: &str) -> String {
        self.explore_fps.get(sched).cloned().unwrap_or_default()
    }

    fn filter(&self, k: usize) -> String {
        format!("{}::run", self.built[k].module(k))
    }

    /// Write and build the project of these calls; the image's path.
    fn image(&self, name: &str, calls: &[Call], steps: usize) -> Result<PathBuf, String> {
        let dir = self.out.join("images").join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let cases = dir.join("cases");
        std::fs::create_dir_all(&cases).map_err(|e| e.to_string())?;
        let w = |p: PathBuf, t: &str| {
            std::fs::write(&p, t).map_err(|e| format!("{}: {e}", p.display()))
        };
        let mut by_step: BTreeMap<usize, Vec<&Call>> = BTreeMap::new();
        for c in calls {
            by_step.entry(c.step).or_default().push(c);
        }
        for (s, cs) in &by_step {
            let d = cases.join(s.to_string());
            std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
            let wr = &self.writers[cs[0].w];
            let mut text = String::new();
            for c in cs {
                let _ = write!(text, "{} ", c.k);
                for v in &c.args {
                    value::encode(v, &mut text);
                }
                text.push('\n');
            }
            w(d.join("calls"), &text)?;
            w(d.join("filter"), &format!("{}\n", self.filter(wr.f)))?;
            if let Some(ck) = wr.check {
                w(d.join("check"), &format!("{}\n", self.filter(ck)))?;
            }
        }
        std::fs::copy(self.exe, dir.join("harness")).map_err(|e| format!("the harness: {e}"))?;
        w(dir.join("run.sh"), &run_sh(self.o.timeout))?;
        w(dir.join("check.sh"), &check_sh(self.o.timeout))?;
        w(dir.join("dirs.sh"), DIRS_SH)?;
        w(
            dir.join("hydo.yaml"),
            &manifest(name, steps, self.o.data_mib, self.o.timeout, calls.len()),
        )?;
        let bdir = dir.join("hydo-out");
        let log = dir.join("build.txt");
        let t0 = std::time::Instant::now();
        let st = Command::new(hydo_exe())
            .args(["build", "hydo.yaml", "--out"])
            .arg(&bdir)
            .current_dir(&dir)
            .stdout(std::fs::File::create(&log).map_err(|e| e.to_string())?)
            .stderr(std::fs::File::create(dir.join("build.err")).map_err(|e| e.to_string())?)
            .status()
            .map_err(|e| format!("hydo build: {e}"))?;
        if !st.success() {
            let err = std::fs::read_to_string(dir.join("build.err")).unwrap_or_default();
            let tail: Vec<&str> = err.lines().rev().take(12).collect();
            return Err(format!(
                "hydo build of {} failed:\n{}",
                dir.display(),
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            ));
        }
        let img = find_image(&bdir)?;
        // the build's intermediates: the run needs only the disk image and its record
        if let Ok(rd) = std::fs::read_dir(&bdir) {
            for e in rd.flatten() {
                let p = e.path();
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if n.ends_with("-root") {
                    let _ = std::fs::remove_dir_all(&p);
                } else if n.ends_with("-ext4.img") || n.ends_with("-data.img") {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        eprintln!(
            "hydo: fuzz: image {} ({} calls in {steps} steps) built in {} s",
            img.display(),
            calls.len(),
            t0.elapsed().as_secs()
        );
        Ok(img)
    }

    /// Run the image under a schedule and read the answers back.
    fn run(
        &self,
        image: &Path,
        sched: &str,
        tag: &str,
        calls: &[Call],
    ) -> Result<(RunInfo, Answers), String> {
        let dir = self.out.join("runs").join(tag);
        let _ = std::fs::remove_dir_all(&dir);
        if let Some(p) = dir.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut c = Command::new(hydo_exe());
        c.arg("run")
            .arg(image)
            .arg(sched)
            .arg("--out")
            .arg(&dir)
            .arg("--no-diagnose");
        if let Some(b) = &self.o.backend {
            c.args(["--backend", b]);
        }
        let limit = match self.o.run_timeout {
            0 => 600 + 2 * calls.len() as u64,
            n => n,
        };
        let Some(o) = output_within(&mut c, limit).map_err(|e| format!("hydo run: {e}"))? else {
            // Milestone 296: a hung run is a run without answers, and the fuzzing goes on
            eprintln!(
                "hydo: fuzz: hydo run {sched} of {} took longer than {limit} s on this machine \
                 and was stopped (--run-timeout)",
                image.display()
            );
            let _ = std::fs::create_dir_all(&dir);
            let info = RunInfo {
                schedule: sched.into(),
                dir: dir.display().to_string(),
                image: image.display().to_string(),
                verdict: format!("TIMEOUT on this machine after {limit} s"),
                ..RunInfo::default()
            };
            return Ok((info, Answers::default()));
        };
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            dir.join("hydo-run.txt"),
            [o.stdout.as_slice(), o.stderr.as_slice()].concat(),
        );
        let res: serde_json::Value = std::fs::read_to_string(dir.join("result.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .ok_or_else(|| {
                format!(
                    "hydo run {sched} wrote no result.json:\n{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                )
            })?;
        let first = std::fs::read_to_string(dir.join("one.log")).unwrap_or_default();
        let rec = std::fs::read_to_string(dir.join("one.recovery.log")).ok();
        let mut ans = self.read_answers(calls, &first, rec.as_deref());
        ans.killed = in_call(sched).filter(|(_, kill)| *kill).map(|(c, _)| c);
        // Milestone 296: the reaction of a fault inside a call says in the control log that it
        // saw the call's start
        let in_call_note = match in_call(sched) {
            None => String::new(),
            Some(((s, k), _)) => {
                let ctl = std::fs::read_to_string(dir.join("one.control.log"))
                    .or_else(|_| std::fs::read_to_string(dir.join("one.log")))
                    .unwrap_or_default();
                if ctl.contains(&format!("saw {CALL_EVENT}")) {
                    format!("the fault came inside call {s}.{k}")
                } else {
                    format!("call {s}.{k} did not report its start, so no fault came")
                }
            }
        };
        let info = RunInfo {
            schedule: sched.into(),
            dir: dir.display().to_string(),
            image: image.display().to_string(),
            verdict: res["verdict"]["text"]
                .as_str()
                .or(res["verdict"]["kind"].as_str())
                .unwrap_or("")
                .to_string(),
            fingerprint: res["fingerprints"]["fp"].as_str().unwrap_or("").to_string(),
            power_cut: rec.is_some(),
            calls: ans.calls.len() as u64,
            checks: ans.checks.len() as u64,
            in_call: in_call_note,
        };
        Ok((info, ans))
    }

    /// The answers of a run. Each call's answer is an id `<step>.<call>=<answer>` in
    /// `$HYDO_ACKED`, which the first boot reports at its end or at the power cut
    /// (`hydo-explore-acked:`); a console holds at most 64 KiB of the guest's own lines, and a
    /// report line has no such limit. Each check's answer is a `hydo-fuzz: check` line of the
    /// console that ran the checks.
    fn read_answers(&self, calls: &[Call], first: &str, recovery: Option<&str>) -> Answers {
        let mut a = Answers {
            after_cut: recovery.is_some(),
            ..Answers::default()
        };

        let steps: BTreeMap<usize, usize> = calls.iter().map(|c| (c.step, c.w)).collect();
        let step_call = |id: &str| -> Option<(usize, usize, usize)> {
            let (s, k) = id.split_once('.')?;
            let (s, k) = (s.parse().ok()?, k.parse().ok()?);
            Some((s, k, *steps.get(&s)?))
        };
        let acked = first
            .lines()
            .rev()
            .find_map(|l| l.trim_start().strip_prefix("hydo-explore-acked:"))
            .unwrap_or("");
        for word in acked.split_ascii_whitespace() {
            let Some((id, v)) = word.split_once('=') else {
                continue;
            };
            let Some((s, k, wi)) = step_call(id) else {
                continue;
            };
            if v == "L" {
                continue; // on the console
            }
            let f = &self.built[self.writers[wi].f];
            let o = self.parse(f, &mut v.split(','));
            a.calls.insert((s, k), o);
        }
        // the answers longer than ANSWER_CAP, from the first boot's console
        for line in first.lines() {
            let Some(at) = line.find("hydo-fuzz: call ") else {
                continue;
            };
            let mut w = line[at + 16..].split_ascii_whitespace();
            let (Some(s), Some(k)) = (w.next(), w.next()) else {
                continue;
            };
            let Some((s, k, wi)) = step_call(&format!("{s}.{k}")) else {
                continue;
            };
            let o = self.parse(&self.built[self.writers[wi].f], &mut w);
            a.calls.insert((s, k), o);
        }
        // the checks' answers: `hydo-fuzz: checks <step>.<call>=<S|N>=<answer> ...` lines of
        // the console of the boot that ran the checks
        let text = recovery.unwrap_or(first);
        let words: Vec<&str> = text
            .lines()
            .filter_map(|l| l.find(CHECKS_MARK).map(|i| &l[i + CHECKS_MARK.len()..]))
            .flat_map(str::split_ascii_whitespace)
            .collect();
        for word in words {
            let mut parts = word.splitn(3, '=');
            let (Some(id), Some(had), Some(v)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let Some((s, k, wi)) = step_call(id) else {
                continue;
            };
            let Some(ck) = self.writers[wi].check else {
                continue;
            };
            // S: the check had the call's result (from the same id), N: it had none
            let done = match had {
                "S" => match a.calls.get(&(s, k)) {
                    Some(Outcome::Returned { result, .. }) => Some(result.clone()),
                    _ => continue,
                },
                "N" => None,
                _ => continue,
            };
            let o = self.parse(&self.built[ck], &mut v.split(','));
            a.checks.insert((s, k), (done, o));
        }
        a
    }

    fn parse<'w>(&self, f: &Fn, w: &mut impl Iterator<Item = &'w str>) -> Outcome {
        match w.next() {
            Some("R") => f.returned(w).unwrap_or(Outcome::Ended),
            Some("P") => {
                let mut s = || match value::decode(&FTy::Str, w) {
                    Ok(Val::Str(s)) => s,
                    _ => String::new(),
                };
                let mut location = s();
                let base = format!("{}/", self.copy.display());
                if let Some(r) = location.strip_prefix(&base) {
                    location = r.to_string();
                }
                Outcome::Panicked {
                    location,
                    message: s(),
                }
            }
            Some("H") => Outcome::Hung,
            _ => Outcome::Ended,
        }
    }

    /// The failures of one call in one run: its own clauses, then its check's. `judged` counts
    /// the calls judged per writer: (w, 0) calls, (w, 1) checks, (w, 2) checks after a cut.
    fn judge(
        &self,
        c: &Call,
        a: &Answers,
        judged: &mut BTreeMap<(usize, usize), u64>,
    ) -> Vec<(Key, Outcome)> {
        let wr = &self.writers[c.w];
        let f = &self.built[wr.f];
        let mut out = Vec::new();
        // Milestone 296: the call whose process the schedule killed gave no answer of its own
        let killed = a.killed == Some((c.step, c.k));
        let call = a.calls.get(&(c.step, c.k)).filter(|_| !killed);
        if let Some(o) = call {
            *judged.entry((c.w, 0)).or_default() += 1;
            for x in f.judge(&c.args, o).failures {
                out.push(((c.w, false, x), o.clone()));
            }
        }
        if let (Some(ck), Some((done, o))) = (wr.check, a.checks.get(&(c.step, c.k))) {
            *judged.entry((c.w, 1)).or_default() += 1;
            if a.after_cut {
                *judged.entry((c.w, 2)).or_default() += 1;
            }
            let mut args = c.args.clone();
            args.push(Val::Opt(done.clone().map(Box::new)));
            for x in self.built[ck].judge(&args, o).failures {
                out.push(((c.w, true, x), o.clone()));
            }
        }
        out
    }

    /// Shrink a failure: the input alone in a run of one step under the same faults, then
    /// simpler inputs, a chunk of them per image and run. A fault inside a call (Milestone 296)
    /// meets one call per run, so each candidate of a chunk gets its own run of the chunk's
    /// image, with `E=` naming it.
    fn shrink(&self, key: &Key, call: &Call, sched: &str) -> Result<Shrunk, String> {
        let (w, _, _) = key;
        let f = &self.built[self.writers[*w].f];
        let n = SHRINKS.with(|s| {
            let mut s = s.borrow_mut();
            *s += 1;
            *s
        });
        let tag = format!("shrink{n}");
        let fails = |calls: &[Call],
                     img: &Path,
                     sched: &str,
                     t: &str|
         -> Result<(Vec<usize>, RunInfo), String> {
            let (info, ans) = self.run(img, sched, t, calls)?;
            let mut judged = BTreeMap::new();
            let hit = calls
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    self.judge(c, &ans, &mut judged)
                        .iter()
                        .any(|(k, _)| k == key)
                })
                .map(|(i, _)| i)
                .collect();
            Ok((hit, info))
        };
        let inside = in_call(sched).is_some();
        let mut best = Shrunk {
            args: call.args.clone(),
            calls: self.explore_calls.clone(),
            steps: 0,
            image: self.explore_image.clone(),
            schedule: sched.to_string(),
            step: call.step,
            k: call.k,
            fingerprint: self.explore_fp(sched),
            tag: format!("{tag}-full"),
            note: String::new(),
        };
        if sched.contains("cf=") {
            best.note = "not shrunk: a cf= power cut counts the disk's flushes from the \
                         workload's start, so the call stays in its run"
                .into();
            return Ok(best);
        }
        let one = one_step_schedule(sched, call.step);
        let mut runs = 0u64;
        // alone
        let alone = vec![Call {
            w: *w,
            step: 0,
            k: 0,
            args: call.args.clone(),
        }];
        let img = self.image(&format!("{tag}-0"), &alone, 1)?;
        runs += 1;
        let (hit, info) = fails(&alone, &img, &one, &format!("{tag}-0"))?;
        if hit.is_empty() {
            best.note = format!(
                "not shrunk: the input alone in one step passed under `{one}`, so the replay is \
                 the whole run"
            );
            return Ok(best);
        }
        best.image = img;
        best.calls = alone;
        best.schedule = one.clone();
        best.step = 0;
        best.k = 0;
        best.fingerprint = info.fingerprint;
        best.tag = format!("{tag}-0");
        let mut cur = call.args.clone();
        let chunk = self.o.batch.max(1);
        'outer: while runs < self.o.shrink_runs {
            let cands = f.candidates(&cur);
            if cands.is_empty() {
                break;
            }
            for part in cands.chunks(chunk) {
                if runs >= self.o.shrink_runs {
                    break 'outer;
                }
                let calls: Vec<Call> = part
                    .iter()
                    .enumerate()
                    .map(|(k, a)| Call {
                        w: *w,
                        step: 0,
                        k,
                        args: a.clone(),
                    })
                    .collect();
                let t = format!("{tag}-{}", runs + 1);
                let img = self.image(&t, &calls, 1)?;
                let mut hit = None;
                if inside {
                    // one run per candidate, each with the fault inside its own call
                    for i in 0..calls.len() {
                        if runs >= self.o.shrink_runs {
                            break;
                        }
                        runs += 1;
                        let sch = with_call(&one, 0, i);
                        let rt = format!("{t}.{i}");
                        let (h, info) = fails(&calls, &img, &sch, &rt)?;
                        if h.contains(&i) {
                            hit = Some((i, info, sch, rt));
                            break;
                        }
                    }
                } else {
                    runs += 1;
                    let (h, info) = fails(&calls, &img, &one, &t)?;
                    hit = h.first().map(|&i| (i, info, one.clone(), t.clone()));
                }
                let Some((i, info, sch, rt)) = hit else {
                    // a candidate image that passed is not kept: only the replays are
                    let _ = std::fs::remove_dir_all(self.out.join("images").join(&t));
                    continue;
                };
                cur = part[i].clone();
                best.steps += 1;
                best.args = cur.clone();
                best.image = img;
                best.calls = calls;
                best.k = i;
                best.fingerprint = info.fingerprint;
                best.schedule = sch;
                best.tag = rt;
                continue 'outer;
            }
            break;
        }
        if runs >= self.o.shrink_runs {
            best.note = format!(
                "the shrink stopped after --shrink-runs {}",
                self.o.shrink_runs
            );
        }
        Ok(best)
    }
}

thread_local! {
    /// The shrinks so far, for the names of their images and runs.
    static SHRINKS: std::cell::RefCell<u64> = const { std::cell::RefCell::new(0) };
}

/// This `hydo`, for `hydo build` and `hydo run`.
fn hydo_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("hydo"))
}

/// The image that `hydo build` wrote into a folder.
fn find_image(dir: &Path) -> Result<PathBuf, String> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.ends_with("-disk.img") || n.ends_with("-arm64.cpio.gz")
        })
        .collect();
    found.sort();
    found
        .pop()
        .ok_or_else(|| format!("hydo build wrote no image into {}", dir.display()))
}

fn manifest(name: &str, steps: usize, data_mib: u64, timeout: u64, calls: usize) -> String {
    format!(
        "# Written by `hydo fuzz --vm` (Milestones 292 and 296); every run of hydo fuzz --vm writes it again.
# The workload makes the calls of cases/<step>/calls, each call in a process of its own and in the
# folder /data/fuzz/<step>.<call>; check.sh calls each durability check after the run.
format: 1
name: fuzz-vm-{name}
description: function fuzzing under faults, {calls} calls in {steps} steps
root:
  files:
    - {{src: harness, dst: /fuzz/harness, mode: \"0755\"}}
    - {{src: run.sh, dst: /fuzz/run.sh, mode: \"0755\"}}
    - {{src: check.sh, dst: /fuzz/check.sh, mode: \"0755\"}}
    - {{src: dirs.sh, dst: /fuzz/dirs.sh, mode: \"0755\"}}
    - {{src: cases, dst: /fuzz/cases}}
data:
  size_mib: {data_mib}
provision: /fuzz/dirs.sh
knobs:
  # Milestone 296: E=<step>.<call> names the call that reports its start (the event fuzz.call),
  # for a fault inside it (A=fuzz.call+<ms>:<action>)
  - token: E
    name: the call that reports its start
    default: none
    values: [none]
    set: echo $HYDO_VALUE > /tmp/fz.event
workload:
  steps: {steps}
  run: /fuzz/run.sh $HYDO_FROM $HYDO_TO
  timeout: {wt}
invariants:
  - id: fuzz.checks
    msg: the durability checks were called (the host judges their answers)
    cmd: /fuzz/check.sh
faults: [cut, clock, disk]
",
        wt = (timeout * (calls as u64 + 2)).clamp(60, 3600),
    )
}

/// The folders of the calls, made at provisioning, which ends with a `sync`: a folder that a
/// power cut takes away would be a failure that no function made.
const DIRS_SH: &str = "#!/bin/sh
# hydo fuzz --vm: one folder per call on the data disk, made before the run
for d in /fuzz/cases/*; do
  s=${d##*/}
  while read -r k rest; do mkdir -p /data/fuzz/$s.$k; done < $d/calls
done
sync
";

/// The start of check.sh's console lines, which hold the checks' answers.
pub const CHECKS_MARK: &str = "hydo-fuzz: checks";

/// The most bytes of long answers that run.sh writes to the console of one boot: hvx keeps the
/// first 64 KiB of a VM's console, and the checks' lines need the rest.
pub const LONG_ANSWERS_CAP: usize = 32 * 1024;

/// The longest answer that goes to `$HYDO_ACKED`; a longer one goes to the console, and its
/// check gets `None` for the result.
pub const ANSWER_CAP: usize = 64;

fn run_sh(timeout: u64) -> String {
    let (cap, long) = (ANSWER_CAP, LONG_ANSWERS_CAP);
    format!(
        "#!/bin/sh
# hydo fuzz --vm: the calls of the workload steps $1 to $2 - 1. Each call runs in a process of its
# own, in its own folder. Its answer goes to $HYDO_ACKED as <step>.<call>=<answer, commas for
# spaces>, which reaches the host at the run's end or at a power cut. A call with a durability
# check is also listed there when it starts (<step>.<call>), so that the check after a power cut
# knows each call that started.
s=$1; to=$2
# the call that the schedule names (E=<step>.<call>) reports its start, for a fault inside it
ev=$(cat /tmp/fz.event 2>/dev/null)
# ... and runs as /tmp/{CALL_PROCESS}, so that a kill inside the call meets only it
[ -n \"$ev\" ] && ln -sf /fuzz/harness /tmp/{CALL_PROCESS}
while [ \"$s\" -lt \"$to\" ]; do
  d=/fuzz/cases/$s
  if [ -d $d ]; then
    f=$(cat $d/filter)
    c=0; [ -f $d/check ] && c=1
    while read -r k rest; do
      cd /data/fuzz/$s.$k || {{ echo \"$s.$k=E,nofolder\" >> \"$HYDO_ACKED\"; continue; }}
      [ $c = 1 ] && echo \"$s.$k\" >> \"$HYDO_ACKED\"
      echo \"$k $rest\" > /tmp/fz.in
      rm -f /tmp/fz.out
      e=; p=/fuzz/harness
      [ \"$s.$k\" = \"$ev\" ] && {{ e=1; p=/tmp/{CALL_PROCESS}; }}
      HYDO_FUZZ_EVENT=$e HYDO_FUZZ_CASES=/tmp/fz.in HYDO_FUZZ_OUT=/tmp/fz.out timeout -s KILL {timeout} $p \"$f\" --test-threads=1 -q < /dev/null > /dev/null 2>&1
      rc=$?
      a=$(cat /tmp/fz.out 2>/dev/null)
      if [ -n \"$a\" ]; then v=$(echo ${{a#* }} | tr ' ' ,)
      elif [ $rc = 137 ] || [ $rc = 124 ]; then v=H
      else v=E,$rc; fi
      # a long answer goes to the console: a power cut carries $HYDO_ACKED to the recovery boot,
      # and on hvx a recovery boot with about 90 KiB of it did not start (Milestone 292)
      if [ ${{#v}} -gt {cap} ]; then
        cb=$(cat /tmp/fz.cb 2>/dev/null || echo 0)
        if [ $((cb + ${{#v}})) -le {long} ]; then
          echo \"hydo-fuzz: call $s $k ${{a#* }}\" > /dev/console; echo $((cb + ${{#v}} + 20)) > /tmp/fz.cb
        fi
        v=L
      fi
      echo \"$s.$k=$v\" >> \"$HYDO_ACKED\"
    done < $d/calls
    echo \"hydo-fuzz: step $s\" > /dev/console
  fi
  s=$((s + 1))
done
"
    )
}

fn check_sh(timeout: u64) -> String {
    let mark = CHECKS_MARK;
    format!(
        "#!/bin/sh
# hydo fuzz --vm: after the run (in the recovery boot after a power cut), the durability check of
# every call that started, in the call's folder, with the call's input and its result: S <result>
# for a call that returned, N for one that did not. The host judges the answers.
out=
[ -s \"$HYDO_ACKED\" ] || exit 0
grep -v = \"$HYDO_ACKED\" > /tmp/fz.ids
while read -r id; do
  s=${{id%.*}}; k=${{id#*.}}
  d=/fuzz/cases/$s
  [ -f $d/check ] || continue
  args=$(sed -n \"s/^$k //p\" $d/calls)
  # (\"\" makes awk compare the ids as text: as numbers, 0.1 and 0.10 are equal)
  res=$(awk -F= -v id=\"$id\" '$1 \"\" == id \"\" && NF > 1 {{ print $2; exit }}' \"$HYDO_ACKED\")
  case $res in
    R,*) r=S; v=\"S $(echo \"${{res#R,}}\" | tr , ' ')\" ;;
    *) r=N; v=N ;;
  esac
  if ! cd /data/fuzz/$id 2>/dev/null; then out=\"$out $id=$r=E,nofolder\"; continue; fi
  echo \"$k $args $v\" > /tmp/fz.in
  rm -f /tmp/fz.out
  HYDO_FUZZ_CASES=/tmp/fz.in HYDO_FUZZ_OUT=/tmp/fz.out timeout -s KILL {timeout} /fuzz/harness \"$(cat $d/check)\" --test-threads=1 -q < /dev/null > /dev/null 2>&1
  rc=$?
  a=$(cat /tmp/fz.out 2>/dev/null)
  if [ -n \"$a\" ]; then w=$(echo ${{a#* }} | tr ' ' ,)
  elif [ $rc = 137 ] || [ $rc = 124 ]; then w=H
  else w=E,$rc; fi
  out=\"$out $id=$r=$w\"
  [ ${{#out}} -gt 2000 ] && {{ echo \"{mark}$out\" > /dev/console; out=; }}
done < /tmp/fz.ids
[ -n \"$out\" ] && echo \"{mark}$out\" > /dev/console
exit 0
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places() -> Vec<(usize, usize)> {
        (0..8).flat_map(|s| (0..25).map(move |k| (s, k))).collect()
    }

    #[test]
    fn the_first_schedule_has_no_fault_and_the_draw_repeats() {
        let k: Vec<String> = KINDS.iter().map(|s| s.to_string()).collect();
        let a = draw_schedules(1, 11, &k, 8, &places(), true);
        assert_eq!(a[0], "disk=none | run");
        assert_eq!(a.len(), 11);
        assert_eq!(a, draw_schedules(1, 11, &k, 8, &places(), true));
        assert_ne!(a, draw_schedules(2, 11, &k, 8, &places(), true));
        // a host part and a guest part (hydo-cli's tests parse them with the schedule language)
        for s in &a {
            assert!(s.contains(" | run"), "{s}");
        }
    }

    /// Milestone 296: faults inside a call, through the event of the call that E= names, and no
    /// cf= on an engine without it.
    #[test]
    fn faults_inside_a_call_name_the_call_and_the_mac_gets_no_cf() {
        let k: Vec<String> = KINDS.iter().map(|s| s.to_string()).collect();
        let a = draw_schedules(3, 41, &k, 8, &places(), false);
        assert!(a.iter().all(|s| !s.contains("cf=")), "{a:?}");
        let inside: Vec<&String> = a.iter().filter(|s| in_call(s).is_some()).collect();
        for want in [
            ":cut",
            ":clock",
            ":stop-workload-",
            ":stop-workload/hydo-call#2-",
            ":kill-",
        ] {
            assert!(
                inside.iter().any(|s| s.contains(want)),
                "{want}: {inside:?}"
            );
        }
        for s in &inside {
            assert!(s.contains(" A=fuzz.call"), "{s}");
            let ((st, c), kill) = in_call(s).unwrap_or_else(|| unreachable!());
            assert!(st < 8 && c < 25, "{s}");
            assert_eq!(kill, s.ends_with(":kill-workload/fuzz-call"), "{s}");
        }
        assert!(draw_schedules(3, 41, &k, 8, &places(), true)
            .iter()
            .any(|s| s.contains("cf=")));
        assert_eq!(
            in_call("disk=none | run E=3.7 A=fuzz.call+20:clock-300"),
            Some(((3, 7), false))
        );
        assert_eq!(
            with_call("disk=none | run E=3.7 A=fuzz.call+20:clock-300", 0, 4),
            "disk=none | run E=0.4 A=fuzz.call+20:clock-300"
        );
        assert_eq!(
            one_step_schedule("disk=cut dseed=1 | run E=5.9 A=fuzz.call+5:cut", 5),
            "disk=cut dseed=1 | run E=0.0 A=fuzz.call+5:cut"
        );
        assert_eq!(in_call("disk=none | run T=2:30"), None);
    }

    #[test]
    fn a_one_step_schedule_keeps_the_faults_around_the_call() {
        assert_eq!(
            one_step_schedule("disk=cut dseed=1 | run C=6", 3),
            "disk=cut dseed=1 | run C=1"
        );
        assert_eq!(
            one_step_schedule("disk=none | run T=2:-300 P=workload@3~40:500", 3),
            "disk=none | run T=0:-300 P=workload@0~40:500"
        );
        assert_eq!(
            one_step_schedule("disk=none | run P=workload@1~40:500 T=5:30", 3),
            "disk=none | run T=1:30"
        );
    }
}
