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
    let json = argv.iter().any(|a| a == "--json");
    let args: Vec<String> = argv.iter().filter(|a| *a != "--json").cloned().collect();
    explore_with(ctx, &args, !json && std::io::stdout().is_terminal())
}

/// `hydo explore` with the end for a person (`plain`) or for a program (the summary line).
pub fn explore_with(ctx: &Ctx, argv: &[String], plain: bool) -> i32 {
    let mut args = argv.to_vec();
    let linux = ctx.platform == Platform::Linux;
    let image = find_image(ctx, &mut args);
    if !args.iter().any(|a| a == "--out") {
        args.push("--out".into());
        args.push(pjoin(OUT, "explore"));
    }
    if !args
        .iter()
        .any(|a| a == "--seeds" || a == "--schedule" || a == "--sweep")
    {
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
        let kernel = mac_kernel_for(ctx, &image, true);
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
    };
    let rc = hydo_explore::main_with(&full, &env);
    let i = args.iter().position(|a| a == "--out").unwrap_or(0);
    let out = args.get(i + 1).cloned().unwrap_or_default();
    let report = pjoin(&out, "report.md");
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

pub fn cmd_report(_ctx: &Ctx, argv: &[String]) -> i32 {
    // `--html`: one web page instead (src/htmlreport.rs)
    if argv.iter().any(|a| a == "--html") {
        let rest: Vec<String> = argv.iter().filter(|a| *a != "--html").cloned().collect();
        return crate::htmlreport::cmd(&rest);
    }
    // `--details`: the technical report (report.md's text) instead of the problems in words
    let details = argv.iter().any(|a| a == "--details");
    let d = argv
        .iter()
        .find(|a| *a != "--details")
        .cloned()
        .unwrap_or_else(|| pjoin(OUT, "explore"));
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
        hydo_results::Results::Run { run, .. } => {
            hydo_explore::md::run_report(&run) + &held_lines(Path::new(&d))
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
