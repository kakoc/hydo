//! The commands whose logic is still Python: `hydo explore` (explore.py), `hydo compose`
//! (hydo_compose.py), `hydo errors` (hydo_errors.py), and `hydo guide | skill | init`
//! (hydo_agents.py), plus `hydo report`, which is Rust. (`hydo build` is Rust since M103:
//! src/build.rs.)
//!
//! Each `cmd_*` here is the seam its port replaces: the argument handling, defaults and the
//! lines printed around the Python call are already the Rust CLI's, so a port swaps only the call.

use crate::bridge;
use crate::ctx::{abspath, die, pjoin, python, Ctx, Platform, OUT};
use crate::hvxenv;
use crate::runs::{default_image_mac, find_image_linux, mac_kernel_for};
use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// hydo_errors.DEFAULT_FILES / default_file(): the project's reviewed list of expected error kinds.
pub fn known_errors_default() -> Option<String> {
    ["hydo/known-errors.yaml", "known-errors.yaml"]
        .into_iter()
        .find(|f| Path::new(f).is_file())
        .map(str::to_string)
}

// ---------------------------------------------------------------- explore (explore.py)

pub fn cmd_explore(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    let linux = ctx.platform == Platform::Linux;
    let image = if linux {
        find_image_linux(&mut args)
    } else {
        default_image_mac(&mut args)
    };
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
    if !args.iter().any(|a| a == "--workers") {
        args.push("--workers".into());
        args.push(if linux { "2" } else { "4" }.into());
    }
    if !args.iter().any(|a| a == "--known-errors") {
        if let Some(kf) = known_errors_default() {
            args.push("--known-errors".into());
            args.push(abspath(Path::new(&kf)).display().to_string());
        }
    }
    let explore_py = ctx.explore_dir().join("explore.py");
    let mut c = Command::new(python());
    c.arg(&explore_py);
    if linux {
        let engine = hvxenv::engine(ctx);
        if !crate::ctx::access(&engine, libc::X_OK) {
            die(&format!(
                "{} is not built (run install.sh again)",
                engine.display()
            ));
        }
        for (k, v) in hvxenv::tool_env(ctx) {
            c.env(k, v);
        }
    } else {
        let kernel = mac_kernel_for(ctx, &image, true);
        c.arg("--backend")
            .arg("hvm")
            .arg("--hvm-kernel")
            .arg(kernel);
    }
    c.arg("--image").arg(&image).args(&args);
    let rc = c
        .status()
        .map(|s| s.code().unwrap_or(1))
        .unwrap_or_else(|e| {
            die(&format!("cannot run {}: {e}", python()));
        });
    let i = args.iter().position(|a| a == "--out").unwrap_or(0);
    let out = args.get(i + 1).cloned().unwrap_or_default();
    let report = pjoin(&out, "report.md");
    if linux {
        if rc == 0 && Path::new(&report).exists() {
            println!("hydo: report: {report}   (hydo report {out}; hydo repro {out} re-runs every failure)");
        }
    } else if rc == 0 {
        println!("hydo: report: {report}   (hydo report {out})");
    }
    rc
}

// ---------------------------------------------------------------- report (Rust)

pub fn cmd_report(_ctx: &Ctx, argv: &[String]) -> i32 {
    let d = argv
        .first()
        .cloned()
        .unwrap_or_else(|| pjoin(OUT, "explore"));
    let p = pjoin(&d, "report.md");
    if !Path::new(&p).exists() {
        die(&format!("no report at {p}"));
    }
    let text = crate::ctx::read_text(Path::new(&p)).unwrap_or_else(|e| die(&format!("{p}: {e}")));
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(text.as_bytes());
    0
}

// ---------------------------------------------------------------- errors (hydo_errors.py)

pub fn cmd_errors(ctx: &Ctx, argv: &[String]) -> i32 {
    bridge::rc(
        ctx,
        "errors_cli",
        json!({"argv": argv, "default_dir": pjoin(OUT, "explore")}),
        &[],
    )
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
