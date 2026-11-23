//! The commands that kept their place here after their ports: `hydo compose` (hydo-compose),
//! `hydo errors` (hydo-errors), `hydo guide | skill | init` (hydo-agents), `hydo explore` (the
//! hydo-explore crate, M102) and `hydo report`. (`hydo build` is Rust since M103: src/build.rs.)
//!
//! Each `cmd_*` here is the seam its port replaces: the argument handling, defaults and the
//! lines printed around the Python call are already the Rust CLI's, so a port swaps only the call.

use crate::ctx::{abspath, die, pjoin, Ctx, Platform, OUT};
use crate::hvxenv;
use crate::runs::{find_image, mac_kernel_for};
use std::io::Write;
use std::path::Path;

/// hydo_errors.DEFAULT_FILES / default_file(): the project's reviewed list of expected error kinds.
pub fn known_errors_default() -> Option<String> {
    ["hydo/known-errors.yaml", "known-errors.yaml"]
        .into_iter()
        .find(|f| Path::new(f).is_file())
        .map(str::to_string)
}

// ---------------------------------------------------------------- explore (hydo-explore)

/// `hydo explore`: the explorer (the hydo-explore crate) in this process. Without `--workers` the
/// pool is sized from the machine and the image (hydo-explore), no longer a fixed 2 (Linux) or 4
/// (Mac).
///
/// In a terminal it ends with the problems counted in words and the command that shows them
/// (Milestone 230). When stdout is not a terminal, or with `--json`, it ends as before: the
/// explorer's summary as one JSON line and the report's path, which scripts and tests read.
pub fn cmd_explore(ctx: &Ctx, argv: &[String]) -> i32 {
    use std::io::IsTerminal;
    // Milestone 299: one exploration per combination of the manifest's configs: (src/configs.rs)
    if argv.iter().any(|a| a == "--configs") {
        return crate::configs::cmd(ctx, argv);
    }
    let json = argv.iter().any(|a| a == "--json");
    let args: Vec<String> = argv.iter().filter(|a| *a != "--json").cloned().collect();
    explore_with(ctx, &args, !json && std::io::stdout().is_terminal())
}

/// `hydo explore` with the end for a person (`plain`) or for a program (the summary line).
pub fn explore_with(ctx: &Ctx, argv: &[String], plain: bool) -> i32 {
    // `--merge DIR... --out DIR` (Milestone 304): the parts of one exploration, without an
    // image or a VM
    if argv.iter().any(|a| a == "--merge") {
        let rc = hydo_explore::merge::main_if_asked(argv, plain).unwrap_or(2);
        let i = argv.iter().position(|a| a == "--out");
        let out = i.and_then(|i| argv.get(i + 1)).cloned().unwrap_or_default();
        if rc == 0 && plain {
            if let Some(end) = crate::htmlreport::text::explore_end(Path::new(&out), &out) {
                print!("{end}");
                return rc;
            }
        }
        if rc == 0 {
            println!(
                "hydo: report: {}   (hydo report {out}; hydo report --html {out})",
                pjoin(&out, "report.md")
            );
        }
        return rc;
    }
    // Milestone 328: a stop before the explorer runs a schedule (no image, no engine, the
    // machine's memory) is a refusal, exit status 5 with a JSON line for a program; inside
    // `hydo ci run` (DIE_CODE 2) the status stays that run's
    use std::sync::atomic::Ordering::Relaxed;
    let refusing = crate::ctx::DIE_CODE.load(Relaxed) == 1;
    if refusing {
        crate::ctx::DIE_CODE.store(hydo_explore::REFUSED, Relaxed);
        crate::ctx::REFUSAL_JSON.store(!plain, Relaxed);
    }
    // `--no-diagnose`: no replay that reads the failing service's log of a problem whose runs
    // stopped with an error (Milestone 247; as `hydo run --no-diagnose`)
    let diagnose = !argv.iter().any(|a| a == "--no-diagnose");
    let mut args: Vec<String> = argv
        .iter()
        .filter(|a| *a != "--no-diagnose")
        .cloned()
        .collect();
    let linux = ctx.platform == Platform::Linux;
    let image = find_image(ctx, &mut args);
    if !args.iter().any(|a| a == "--out") {
        args.push("--out".into());
        args.push(pjoin(OUT, "explore"));
    }
    // Milestone 345: an aimed exploration's default seeds are its whole grid (the explorer's)
    if !args.iter().any(|a| {
        a == "--seeds" || a == "--schedule" || a == "--sweep" || a.starts_with("--reactive-at")
    }) {
        args.push("--seeds".into());
        args.push("1-20".into());
    }
    if !args.iter().any(|a| a == "--known-errors") {
        if let Some(kf) = known_errors_default() {
            args.push("--known-errors".into());
            args.push(abspath(Path::new(&kf)).display().to_string());
        }
    }
    let mut full: Vec<String> = Vec::new();
    if linux && ctx.kvm {
        // the KVM engine runs in this process; the bundled debugfs reads the image's manifest
        if let Some((_, path)) = hvxenv::tool_env(ctx).into_iter().find(|(k, _)| k == "PATH") {
            std::env::set_var("PATH", path);
        }
        full.push("--backend".into());
        full.push("kvm".into());
    } else if linux {
        ctx.require_hvx();
        let engine = hvxenv::engine(ctx);
        let probe = hydo_engine::hvx::Hvx {
            engine: engine.clone(),
            hvx_run: None,
            extra_env: Vec::new(),
            env: hvxenv::tool_env(ctx),
            config: ctx.config.env.clone(),
        };
        if let Some(p) = probe.missing_program() {
            die(&format!(
                "{} is not there (reinstall the release, or build it: cargo build --release -p hydo-hvx)",
                p.display()
            ));
        }
        // the bundled tools on PATH (the engine wrapper and the tools the explorer starts
        // inherit it); the release's paths, lock and engine reach the explorer in its
        // configuration (M144: no hydo variable is set in this process's environment)
        if let Some((_, path)) = hvxenv::tool_env(ctx).into_iter().find(|(k, _)| k == "PATH") {
            std::env::set_var("PATH", path);
        }
    } else {
        // the scripted engine of the tests boots no kernel (its stand-in image has none)
        let kernel = match &ctx.config.pool.scripted_engine {
            Some(_) => std::path::PathBuf::from("scripted-kernel"),
            None => mac_kernel_for(ctx, &image, true),
        };
        full.push("--backend".into());
        full.push("hvm".into());
        full.push("--hvm-kernel".into());
        full.push(kernel.display().to_string());
    }
    full.push("--image".into());
    full.push(image.display().to_string());
    full.extend(args.iter().cloned());
    let env = hydo_explore::Env {
        root: ctx.root.clone(),
        cli: true,
        plain_end: plain,
        config: if linux {
            hvxenv::explore_config(ctx)
        } else {
            ctx.config.clone()
        },
        // Milestone 328: a refusal for memory names the sessions that hold guests
        memory_hint: crate::inspect::session::memory_hint(&crate::inspect::session::running(ctx)),
    };
    let rc = hydo_explore::main_with(&full, &env);
    if refusing {
        crate::ctx::DIE_CODE.store(1, Relaxed);
    }
    let i = args.iter().position(|a| a == "--out").unwrap_or(0);
    let out = args.get(i + 1).cloned().unwrap_or_default();
    let report = pjoin(&out, "report.md");
    // the scripted engine of the tests runs no image
    if rc == 0 && diagnose && ctx.config.pool.scripted_engine.is_none() {
        for (tag, why) in explore_replays(ctx, Path::new(&out)) {
            eprintln!("hydo: the replay of {tag} did not read the failing service's log: {why}");
        }
        for line in walk_replays(ctx, Path::new(&out)) {
            eprintln!("hydo: {line}");
        }
    }
    if rc == 0 && plain {
        if let Some(end) = crate::htmlreport::text::explore_end(Path::new(&out), &out) {
            print!("{end}");
            return rc;
        }
    }
    if rc == 0 && Path::new(&report).exists() {
        println!(
            "hydo: report: {report}   (hydo report {out}; hydo repro {out} re-runs every failure)"
        );
    }
    rc
}

/// The most problems of one exploration that [`explore_replays`] runs again.
const REPLAYS_MAX: usize = 4;

/// A minimal schedule's walk with recording on (Milestone 258): its `I=` value with `,rec`, or
/// None for a schedule without a walk (or one that records already). With the step of the
/// failure, the replay records that step, `,rec<step>` (Milestone 267); without it, the walker
/// records the last step that takes an action.
pub fn with_recording(sched: &str, step: Option<u64>) -> Option<String> {
    let (host, guest) = sched.split_once('|')?;
    let mut found = false;
    let words: Vec<String> = guest
        .split_whitespace()
        .map(|w| match w.strip_prefix("I=") {
            Some(v) if !v.split(',').any(|f| f.starts_with("rec")) => {
                found = true;
                match step {
                    Some(s) => format!("{w},rec{s}"),
                    None => format!("{w},rec"),
                }
            }
            _ => w.to_string(),
        })
        .collect();
    found.then(|| format!("{}| {}", host, words.join(" ")))
}

/// A schedule's walk without recording (Milestone 270): its `I=` value without `rec` or
/// `rec<step>`, or None for a schedule whose walk does not record. A recording replay repeats
/// the actions of this schedule's run.
pub fn without_recording(sched: &str) -> Option<String> {
    let (host, guest) = sched.split_once('|')?;
    let mut found = false;
    let words: Vec<String> = guest
        .split_whitespace()
        .map(|w| match w.strip_prefix("I=") {
            Some(v) if v.split(',').any(|f| f.starts_with("rec")) => {
                found = true;
                let kept: Vec<&str> = v.split(',').filter(|f| !f.starts_with("rec")).collect();
                format!("I={}", kept.join(","))
            }
            _ => w.to_string(),
        })
        .collect();
    found.then(|| format!("{}| {}", host, words.join(" ")))
}

/// What a recording replay's line adds when the replay could not repeat every action of the run
/// it replays (Milestone 270): each action that it left out, with the walker's reason.
pub fn left_out_note(console: &str) -> String {
    let left = hydo_manifest::browser::walk_left_out(console);
    if left.is_empty() {
        return String::new();
    }
    let n = left.len();
    let each: Vec<String> = left
        .iter()
        .take(3)
        .map(|(at, what)| format!("action {at}: {what}"))
        .collect();
    format!(
        "; it could not repeat {n} action{} of the failing run and left {} out ({}{})",
        if n == 1 { "" } else { "s" },
        if n == 1 { "it" } else { "them" },
        each.join("; "),
        if n > 3 { "; ..." } else { "" }
    )
}

/// The recording replays of an exploration's minimal schedules (Milestone 258): for each, the
/// index of its shrink in `doc.shrinks`, its schedule with recording on, and its folder under
/// `replays/` (`rec-<k>`). [`walk_replays`] runs them, and `hydo report` links each problem's
/// (Milestone 301).
pub fn recordings(doc: &hydo_results::Exploration) -> Vec<(usize, String, String)> {
    doc.shrinks
        .iter()
        .enumerate()
        .filter_map(|(i, s)| with_recording(&s.minimal, failing_step(&s.verdict)).map(|r| (i, r)))
        .take(REPLAYS_MAX)
        .enumerate()
        .map(|(k, (i, r))| (i, r, format!("rec-{}", k + 1)))
        .collect()
}

/// The step of a walk in which a verdict's first violation happened (Milestone 267): the walker
/// names the test of step `s` `walk step <s>`, and a page's finding says during which test it
/// happened.
pub fn failing_step(verdict: &str) -> Option<u64> {
    let at = verdict.find("walk step ")? + "walk step ".len();
    let digits: String = verdict[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// The last step of `hydo explore` for a walk through the pages (Milestone 258): each minimal
/// schedule with a walk (at most [`REPLAYS_MAX`]) runs once more with recording on (`I=...,rec`),
/// into `replays/rec-<k>/` of the exploration's folder. Its `screens/` gets a video and a
/// Playwright trace of the step that failed (Milestone 267; of the last step with an action when
/// the verdict names no step), and the walk as `repro.spec.ts`. Recording costs guest time, so
/// the replay is a run of its own with its own fingerprint: the line says whether it ended with
/// the minimal schedule's verdict. The exploration's own files do not change.
pub fn walk_replays(ctx: &Ctx, dir: &Path) -> Vec<String> {
    let Ok(hydo_results::Results::Exploration { doc: Some(doc), .. }) = hydo_results::read_dir(dir)
    else {
        return Vec::new();
    };
    let image = Path::new(&doc.image.path);
    let mut opts = crate::runs::RunOpts::of_eargs(doc.engine.args.as_deref().unwrap_or(&[]));
    if opts.vcpus.is_none() {
        opts.vcpus = doc.engine.vcpus.filter(|n| *n > 1).map(|n| n as u32);
    }
    let mut lines = Vec::new();
    for (i, rec, folder) in recordings(&doc) {
        let s = &doc.shrinks[i];
        let out = dir.join(crate::htmlreport::REPLAYS).join(folder);
        let _ = std::fs::create_dir_all(&out);
        let shown = out.display();
        // Milestone 270: the recording repeats the actions of the minimal schedule's confirmation
        // run, which is the run of the same schedule without rec
        let script = s.confirmations.iter().find_map(|c| {
            let tag = c.tag.as_deref()?;
            let console =
                crate::ctx::read_text(&dir.join("consoles").join(format!("{tag}.log"))).ok()?;
            hydo_manifest::browser::walk_script(&console)
        });
        let ran = crate::runs::try_run_to_dir_scripted(
            ctx,
            image,
            doc.image.sha256.clone(),
            &rec,
            &out,
            &opts,
            script,
        );
        lines.push(match ran {
            Err(w) => format!("the recording of {} did not run: {w}", s.minimal),
            Ok(None) => format!("the recording of {} gave no result ({shown})", s.minimal),
            Ok(Some(run)) => {
                let same = run.verdict.text == s.verdict
                    || hydo_explore::results::signature(&run.verdict.text)
                        == hydo_explore::results::signature(&s.verdict);
                let console = crate::ctx::read_text(&out.join("one.log")).unwrap_or_default();
                if same {
                    format!(
                        "the walk of {} ended with the same verdict with recording on: video, trace and repro.spec.ts in {shown}/screens{}",
                        s.minimal,
                        left_out_note(&console)
                    )
                } else {
                    // Milestone 301: say plainly that the video is not of the failure
                    format!(
                        "the walk of {} did not fail with recording on, so its video in {shown}/screens shows no failure: it ended {} (recording slows the browser, and a failure that depends on the walk's timing can pass in the recorded run; the minimal schedule without recording still fails){}",
                        s.minimal,
                        run.verdict.text.trim_start_matches("HYDO-ORACLE: "),
                        left_out_note(&console)
                    )
                }
            }
        });
    }
    lines
}

/// The last step of `hydo explore` (Milestone 247): a service's own log is in no file of an
/// exploration, so for each problem whose run stopped with an error (at most [`REPLAYS_MAX`]),
/// its schedule runs once more from a fresh boot, as `hydo run` runs it, into `replays/<tag>/` of
/// the exploration's folder, with `hydo run`'s diagnosis, whose replay reads the failing
/// service's log (`logs/<service>.log` there). `hydo report` quotes the log's last lines under the
/// problem when that run ended with the same failure. The exploration's own files do not change.
/// Returns the runs that gave no diagnosis, with the reason.
pub fn explore_replays(ctx: &Ctx, dir: &Path) -> Vec<(String, String)> {
    let Ok(hydo_results::Results::Exploration {
        doc: Some(doc),
        events,
        ..
    }) = hydo_results::read_dir(dir)
    else {
        return Vec::new();
    };
    let shown = dir.display().to_string();
    let replays: Vec<(String, String)> =
        crate::htmlreport::exploration_story(dir, &doc, events.as_ref(), &shown)
            .items
            .iter()
            .filter_map(|i| i.replay.clone())
            .take(REPLAYS_MAX)
            .filter_map(|tag| {
                let f = doc
                    .failures
                    .iter()
                    .find(|f| f.run.tag.as_deref() == Some(tag.as_str()))?;
                Some((tag, f.run.schedule.clone()))
            })
            .collect();
    let image = Path::new(&doc.image.path);
    let mut opts = crate::runs::RunOpts::of_eargs(doc.engine.args.as_deref().unwrap_or(&[]));
    if opts.vcpus.is_none() {
        opts.vcpus = doc.engine.vcpus.filter(|n| *n > 1).map(|n| n as u32);
    }
    let mut failed = Vec::new();
    for (tag, sched) in replays {
        let out = dir.join(crate::htmlreport::REPLAYS).join(&tag);
        let _ = std::fs::create_dir_all(&out);
        let why = match crate::runs::try_run_to_dir(
            ctx,
            image,
            doc.image.sha256.clone(),
            &sched,
            &out,
            &opts,
        ) {
            Err(w) => Some(w),
            Ok(None) => Some(format!("no result (engine.log in {})", out.display())),
            Ok(Some(mut run)) if run.verdict.kind != "ok" => {
                match crate::inspect::diagnose::for_run(ctx, &out, true) {
                    Some(d) => {
                        run.diagnosis = Some(d);
                        hydo_results::write_document(&out.join(hydo_results::RUN_DOC), &run)
                            .err()
                            .map(|e| e.to_string())
                    }
                    None => Some("no diagnosis".into()),
                }
            }
            Ok(Some(run)) => Some(format!("it ended with {}", run.verdict.text)),
        };
        if let Some(w) = why {
            failed.push((tag, w));
        }
    }
    failed
}

// ---------------------------------------------------------------- report (Rust)

/// `hydo report [DIR]`: the report of an exploration (report.md, made from its exploration.json
/// by the results reader) or of a run (its result.json); while an exploration still runs, how
/// far its event stream has got.
/// For a run directory's report: what each check said the last time it held, read from the
/// run's consoles (Milestone 239: a passing run's report named no check's last line, so the
/// user had to open the run directory to see what a check judged). Empty when nothing held
/// with details.
fn held_lines(dir: &Path) -> String {
    let mut text = String::new();
    for f in ["one.log", "one.recovery.log"] {
        if let Ok(b) = std::fs::read(dir.join(f)) {
            text.push_str(&String::from_utf8_lossy(&b));
        }
    }
    let held = hydo_explore::results::last_held(&text);
    if held.is_empty() {
        return String::new();
    }
    let w = held.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let mut o = String::from("\nwhat each check said the last time it held:\n");
    for (k, v) in held {
        o.push_str(&format!("  {k:<w$}  {v}\n"));
    }
    o
}

/// The error paths of a run or exploration directory (Milestone 306): an exploration's, or one
/// run's with the plan next to its image.
pub fn paths_of_dir(d: &Path) -> Result<hydo_results::PathCoverage, String> {
    let none = "it recorded no error paths: build the image with `error_paths: true` in a \
                `contracts:` entry of hydo.yaml (hydo guide error-paths)";
    match hydo_results::read_dir(d).map_err(|e| format!("{}: {e}", d.display()))? {
        hydo_results::Results::Exploration { doc: Some(doc), .. } => doc
            .paths
            .ok_or_else(|| format!("{}: the exploration: {none}", d.display())),
        hydo_results::Results::Exploration { .. } => {
            Err(format!("{}: the exploration has not finished", d.display()))
        }
        hydo_results::Results::Run { run, .. } => {
            let rp = run
                .paths
                .clone()
                .ok_or_else(|| format!("{}: the run: {none}", d.display()))?;
            let img = run
                .image
                .as_ref()
                .map(|i| i.resolved.clone().unwrap_or_else(|| i.path.clone()))
                .ok_or_else(|| format!("{}: the run names no image", d.display()))?;
            let plan = match hydo_contracts::plan::plan_of_image(Path::new(&img)) {
                Some(Ok(p)) => p,
                Some(Err(e)) => return Err(e),
                None => return Err(format!("{img}: no plan next to the image")),
            };
            Ok(hydo_explore::paths::of_run(&plan, &run.schedule, rp))
        }
    }
}

/// `hydo report --paths [DIR] [--against DIR2]` (Milestone 306).
fn report_paths(argv: &[String]) -> i32 {
    let mut d = None;
    let mut against = None;
    let mut it = argv.iter().filter(|a| *a != "--paths");
    while let Some(a) = it.next() {
        if a == "--against" {
            against = it.next().cloned();
        } else if d.is_none() {
            d = Some(a.clone());
        }
    }
    let d = d.unwrap_or_else(|| pjoin(OUT, "explore"));
    let c = paths_of_dir(Path::new(&d)).unwrap_or_else(|e| die(&e));
    let other = against.map(|a| paths_of_dir(Path::new(&a)).unwrap_or_else(|e| die(&e)));
    let mut o = std::io::stdout().lock();
    for l in hydo_explore::paths::text(&c, other.as_ref()) {
        let _ = writeln!(o, "{l}");
    }
    0
}

pub fn cmd_report(_ctx: &Ctx, argv: &[String]) -> i32 {
    // `--paths`: which error paths ran (Milestone 306)
    if argv.iter().any(|a| a == "--paths") {
        return report_paths(argv);
    }
    // `--html`: one web page instead (src/htmlreport.rs)
    if argv.iter().any(|a| a == "--html") {
        let rest: Vec<String> = argv.iter().filter(|a| *a != "--html").cloned().collect();
        return crate::htmlreport::cmd(&rest);
    }
    // `--details`: the technical report (report.md's text) instead of the problems in words
    let details = argv.iter().any(|a| a == "--details");
    // Milestone 345: several explorations of one image, their problems as one list
    let dirs: Vec<String> = argv
        .iter()
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect();
    if dirs.len() > 1 {
        if details {
            die("hydo report --details takes one directory; give the explorations one by one");
        }
        let rounds = crate::htmlreport::merged::read(&dirs)
            .unwrap_or_else(|e| die(&format!("no report: {e}")));
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(crate::htmlreport::merged::text(&rounds).as_bytes());
        return 0;
    }
    let d = argv
        .iter()
        .find(|a| *a != "--details")
        .cloned()
        .unwrap_or_else(|| pjoin(OUT, "explore"));
    // `hydo compare-versions`' answer (Milestone 298): a directory of its own
    if let Some((_, doc)) = hydo_results::compare_of(Path::new(&d)) {
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(crate::compare::report_lines(&doc).as_bytes());
        return 0;
    }
    // `hydo explore --configs`' answer (Milestone 299): the table of its combinations
    if let Some(doc) = hydo_results::configs_of(Path::new(&d)) {
        print!("{}", crate::configs::text(&doc, &d));
        return 0;
    }
    // `hydo versions`' answer (M221): next to the report of the run or exploration it came from,
    // or alone for a directory with only versions.json
    let versions = hydo_results::versions_of(Path::new(&d));
    let r = match hydo_results::read_dir(Path::new(&d)) {
        Ok(r) => r,
        Err(e) => match &versions {
            Some((p, v)) if p.parent() == Some(Path::new(&d)) => {
                let mut o = std::io::stdout().lock();
                for f in &v.findings {
                    let _ = o.write_all(crate::versions::text(f).as_bytes());
                    let _ = o.write_all(b"\n");
                }
                return 0;
            }
            _ => die(&format!("no report: {e}")),
        },
    };
    let mut text = match r {
        hydo_results::Results::Run { run, .. } if details => {
            hydo_explore::md::run_report(&run) + &held_lines(Path::new(&d))
        }
        // the plain summary of the run's web page (Milestone 247)
        hydo_results::Results::Run { run, .. } => {
            crate::htmlreport::text::run(Path::new(&d), &run, &d)
        }
        hydo_results::Results::Exploration { doc: Some(doc), .. } if details => {
            hydo_explore::md::report(&doc)
        }
        hydo_results::Results::Exploration {
            doc: Some(doc),
            events,
            ..
        } => crate::htmlreport::text::exploration(Path::new(&d), &doc, events.as_ref(), &d),
        hydo_results::Results::Exploration {
            doc: None,
            events: Some(ev),
            ..
        } => {
            let first: Vec<_> = ev
                .runs()
                .filter(|r| r.role.as_deref() == Some(hydo_results::role::FIRST))
                .collect();
            format!(
                "hydo: {d}: the exploration has not finished: {} schedules run, {} failing so far\n",
                first.len(),
                first.iter().filter(|r| r.verdict.kind != "ok").count()
            )
        }
        hydo_results::Results::Exploration { .. } => die(&format!("no report in {d}")),
    };
    if let Some((p, v)) = &versions {
        let md = text.starts_with('#');
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&crate::versions::report_lines(v, p, md));
    }
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(text.as_bytes());
    0
}

// ---------------------------------------------------------------- errors (the hydo-errors crate)

pub fn cmd_errors(_ctx: &Ctx, argv: &[String]) -> i32 {
    hydo_errors::cli(argv, &pjoin(OUT, "explore"))
}

// ---------------------------------------------------------------- compose (the hydo-compose crate)

pub fn cmd_compose(ctx: &Ctx, argv: &[String]) -> i32 {
    if argv.is_empty() || argv[0] == "-h" || argv[0] == "--help" {
        die("usage: hydo compose <docker-compose.yaml> [--out hydo.yaml] [--images-dir DIR] [--env-file F] \
             [--profile P] [--use-env] [--no-resolve] [--lock FILE] [--no-build] [--docker CLI]");
    }
    hydo_compose::main_clock(argv, ctx.config.cli.init_clock_start)
}

// ---------------------------------------------------------------- guide, skill, init (the hydo-agents crate)

pub fn cmd_agents(ctx: &Ctx, cmd: &str, argv: &[String]) -> i32 {
    hydo_agents::run_with(cmd, argv, &ctx.root, ctx.config.cli.init_clock_start)
}

#[cfg(test)]
mod tests {
    use super::{failing_step, left_out_note, with_recording, without_recording};

    /// Milestone 267: the recording replay records the step whose test the violation names.
    #[test]
    fn a_recording_replay_records_the_step_that_failed() {
        let v = "VIOLATION [browser.no_page_errors] The page /memos/<id> left a promise rejection \
                 unhandled during the test \"walk step 2\": TypeError";
        assert_eq!(failing_step(v), Some(2));
        assert_eq!(
            failing_step("VIOLATION [api.order_placed_once] took effect 3 times"),
            None
        );
        let s = "disk=none dseed=1 | run T=1:-2592000 I=fc8214f2a80db679,x3.0-11";
        assert_eq!(
            with_recording(s, Some(2)).as_deref(),
            Some("disk=none dseed=1 | run T=1:-2592000 I=fc8214f2a80db679,x3.0-11,rec2")
        );
        assert_eq!(
            with_recording(s, None).as_deref(),
            Some("disk=none dseed=1 | run T=1:-2592000 I=fc8214f2a80db679,x3.0-11,rec")
        );
        assert_eq!(with_recording("disk=none | run I=1,rec3", Some(1)), None);
        assert_eq!(with_recording("disk=none | run K=a@1+1", Some(1)), None);
    }

    /// Milestone 270: a recording replays the actions of its schedule without `rec`, and its
    /// line names each action it could not repeat.
    #[test]
    fn a_recording_replay_repeats_the_walk_without_recording() {
        for (rec, plain) in [
            (
                "disk=none dseed=1 | run X=memos@4~3000 I=8690c91dedf1a78f,rec4",
                "disk=none dseed=1 | run X=memos@4~3000 I=8690c91dedf1a78f",
            ),
            (
                "disk=none | run I=a0d28305209409f6,n7,x0.0-6,rec T=1:5",
                "disk=none | run I=a0d28305209409f6,n7,x0.0-6 T=1:5",
            ),
        ] {
            assert_eq!(without_recording(rec).as_deref(), Some(plain));
            let back = with_recording(plain, None).unwrap();
            assert_eq!(without_recording(&back).as_deref(), Some(plain));
        }
        assert_eq!(without_recording("disk=none | run I=1"), None);
        assert_eq!(without_recording("disk=none | run K=a@1+1"), None);
        assert_eq!(
            left_out_note("hydo-web: walk 4.7: click button \"Copy link\"\n"),
            ""
        );
        let console =
            "hydo-web: walk 4.7: click button \"Copy link\": not on the page within 10 s\n\
                       hydo-web: walk-act 4.7 {\"do\":\"click\",\"missing\":true}\n";
        assert_eq!(
            left_out_note(console),
            "; it could not repeat 1 action of the failing run and left it out (action 4.7: \
             click button \"Copy link\": not on the page within 10 s)"
        );
    }
}
