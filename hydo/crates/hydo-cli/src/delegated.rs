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
pub fn cmd_explore(ctx: &Ctx, argv: &[String]) -> i32 {
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
pub fn cmd_report(_ctx: &Ctx, argv: &[String]) -> i32 {
    let d = argv
        .first()
        .cloned()
        .unwrap_or_else(|| pjoin(OUT, "explore"));
    let r =
        hydo_results::read_dir(Path::new(&d)).unwrap_or_else(|e| die(&format!("no report: {e}")));
    let text = match r {
        hydo_results::Results::Run { run, .. } => hydo_explore::md::run_report(&run),
        hydo_results::Results::Exploration { doc: Some(doc), .. } => hydo_explore::md::report(&doc),
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
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(text.as_bytes());
    0
}

// ---------------------------------------------------------------- errors (the hydo-errors crate)

pub fn cmd_errors(_ctx: &Ctx, argv: &[String]) -> i32 {
    hydo_errors::cli(argv, &pjoin(OUT, "explore"))
}

// ---------------------------------------------------------------- compose (the hydo-compose crate)

pub fn cmd_compose(_ctx: &Ctx, argv: &[String]) -> i32 {
    if argv.is_empty() || argv[0] == "-h" || argv[0] == "--help" {
        die("usage: hydo compose <docker-compose.yaml> [--out hydo.yaml] [--images-dir DIR] [--env-file F] \
             [--profile P] [--use-env] [--no-resolve] [--lock FILE]");
    }
    hydo_compose::main(argv)
}

// ---------------------------------------------------------------- guide, skill, init (the hydo-agents crate)

pub fn cmd_agents(ctx: &Ctx, cmd: &str, argv: &[String]) -> i32 {
    hydo_agents::run(cmd, argv, &ctx.root)
}
