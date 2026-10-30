//! The commands whose logic is still Python: `hydo explore` (explore.py), `hydo build`
//! (hydo-build), `hydo compose` (hydo_compose.py), `hydo errors` (hydo_errors.py), and `hydo
//! guide | skill | init` (hydo_agents.py), plus `hydo report`, which is Rust.
//!
//! Each `cmd_*` here is the seam its port replaces: the argument handling, defaults and the
//! lines printed around the Python call are already the Rust CLI's, so a port swaps only the call.

use crate::bridge;
use crate::ctx::{abspath, die, pjoin, python, take, take_mac, Ctx, Platform, OUT, PROJECT_OUT};
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

// ---------------------------------------------------------------- compose (hydo_compose.py)

pub fn cmd_compose(ctx: &Ctx, argv: &[String]) -> i32 {
    if argv.is_empty() || argv[0] == "-h" || argv[0] == "--help" {
        die("usage: hydo compose <docker-compose.yaml> [--out hydo.yaml] [--images-dir DIR] [--env-file F] \
             [--profile P] [--use-env] [--no-resolve] [--lock FILE]");
    }
    bridge::rc(ctx, "compose", json!({"argv": argv}), &[])
}

// ---------------------------------------------------------------- guide, skill, init (the hydo-agents crate)

pub fn cmd_agents(ctx: &Ctx, cmd: &str, argv: &[String]) -> i32 {
    hydo_agents::run(cmd, argv, &ctx.root)
}

// ---------------------------------------------------------------- build (hydo-build)

/// Where `hydo build` writes: ./.hydo for a manifest in a hydo/ folder, else ./hydo-out (Linux).
fn project_out(manifest: &str) -> &'static str {
    let abs = abspath(Path::new(manifest));
    let parent = abs
        .parent()
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().into_owned());
    if parent.as_deref() == Some("hydo") {
        PROJECT_OUT
    } else {
        OUT
    }
}

fn setdefault(env: &mut Vec<(String, String)>, k: &str, v: &Path) {
    if std::env::var_os(k).is_none() {
        env.push((k.to_string(), v.display().to_string()));
    }
}

pub fn cmd_build(ctx: &Ctx, argv: &[String]) -> i32 {
    match ctx.platform {
        Platform::Linux => build_linux(ctx, argv),
        Platform::Mac => build_mac(ctx, argv),
    }
}

/// `hydo build <manifest> --check [--set K=V ...]` answered in Rust (hydo-manifest) when the
/// manifest is accepted: the same warnings and summary lines hydo-build prints. Everything else
/// (another flag, or a manifest that is refused, whose message order hydo-build owns) still goes
/// to hydo-build. Returns None when hydo-build must answer.
fn check_in_rust(ctx: &Ctx, manifest: &str, rest: &[String], arch: &str) -> Option<i32> {
    let mut sets = Vec::new();
    let mut check = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--check" => check = true,
            "--set" if i + 1 < rest.len() && rest[i + 1].contains('=') => {
                let (k, v) = rest[i + 1].split_once('=').unwrap_or_default();
                sets.push((k.to_string(), v.to_string()));
                i += 1;
            }
            _ => return None,
        }
        i += 1;
    }
    if !check || manifest == "-h" || manifest == "--help" {
        return None;
    }
    let opts = hydo_manifest::LoadOptions {
        arch: arch.to_string(),
        sets,
        onboard_dir: ctx.onboard(),
    };
    let loaded = hydo_manifest::load(Path::new(manifest), &opts).ok()?;
    for w in &loaded.warnings {
        eprintln!("hydo-build: warning: {w}");
    }
    for line in hydo_manifest::check_summary(manifest, &loaded) {
        println!("{line}");
    }
    Some(0)
}

fn no_print_flag(rest: &[String]) -> bool {
    !rest
        .iter()
        .any(|f| f == "--check" || f == "--print-shell" || f == "--print-init")
}

fn build_linux(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut argv = argv.to_vec();
    if argv.first().is_none_or(|a| a.starts_with('-')) && Path::new("hydo/hydo.yaml").is_file() {
        argv.insert(0, "hydo/hydo.yaml".into());
    }
    if argv.first().is_none_or(|a| a.starts_with('-')) {
        die("usage: hydo build <hydo.yaml> [--out DIR] [--set KEY=VALUE ...]   (no manifest: ./hydo/hydo.yaml)");
    }
    let manifest = argv[0].clone();
    let mut rest = argv[1..].to_vec();
    let out = take(&mut rest, "--out").unwrap_or_else(|| project_out(&manifest).to_string());
    if rest.iter().any(|a| a == "--arch") {
        die("this is the x86_64 Linux release; arm64 images are built with the Mac release");
    }
    if let Some(rc) = check_in_rust(ctx, &manifest, &rest, "x86_64") {
        return rc;
    }
    let guest = ctx.guest();
    let mut env = Vec::new();
    setdefault(&mut env, "HYDO_BUSYBOX", &guest.join("busybox"));
    setdefault(&mut env, "HYDO_SEEDRNG", &guest.join("hvx_seedrng"));
    setdefault(&mut env, "HYDO_FCBIN", &guest.join("hydo_fc"));
    setdefault(&mut env, "HYDO_MKE2FS", &ctx.p("tools/bin/mke2fs"));
    setdefault(&mut env, "HYDO_FAKETIME", &ctx.libexec().join("faketime"));
    setdefault(&mut env, "HYDO_MODROOT", &guest.join("modules"));
    let path = hvxenv::tool_env(ctx).into_iter().find(|(k, _)| k == "PATH");
    env.extend(path);
    let mut hb_argv = vec![manifest, "--build-dir".to_string(), out];
    hb_argv.extend(rest.iter().cloned());
    let rc = bridge::rc(
        ctx,
        "build_linux",
        json!({"root": ctx.root.display().to_string(), "argv": hb_argv}),
        &env,
    );
    if rc == 0 && no_print_flag(&rest) {
        println!("hydo: next:  hydo run \"disk=none | run\"     or     hydo explore --seeds 1-20");
    }
    rc
}

fn build_mac(ctx: &Ctx, argv: &[String]) -> i32 {
    if argv.first().is_none_or(|a| a.starts_with('-')) {
        die("usage: hydo build <hydo.yaml> [--out DIR] [--set KEY=VALUE ...]");
    }
    let manifest = argv[0].clone();
    let mut rest = argv[1..].to_vec();
    let out = take_mac(&mut rest, "--out").unwrap_or_else(|| OUT.to_string());
    if let Some(rc) = check_in_rust(ctx, &manifest, &rest, "arm64") {
        return rc;
    }
    let guest = ctx.guest();
    let mut env = Vec::new();
    setdefault(&mut env, "HYDO_BUSYBOX_ARM64", &guest.join("busybox"));
    setdefault(&mut env, "HYDO_BUSYBOX", &guest.join("busybox-applets"));
    setdefault(&mut env, "HYDO_MKE2FS", &guest.join("mke2fs"));
    setdefault(&mut env, "HYDO_FAKETIME", &guest.join("mke2fs"));
    setdefault(&mut env, "HYDO_WORLDBIN_ARM64", &guest.join("hydo-world"));
    setdefault(&mut env, "HYDO_READ_BIN_ARM64", &guest.join("hydo-read"));
    setdefault(&mut env, "HYDO_ARM64_STORE", &guest.join("tc"));
    setdefault(&mut env, "HYDO_FCBIN_ARM64", &guest.join("hydo_fc"));
    let mut hb_argv = vec![
        manifest,
        "--arch".to_string(),
        "arm64".to_string(),
        "--build-dir".to_string(),
        out,
    ];
    hb_argv.extend(rest.iter().cloned());
    let rc = bridge::rc(
        ctx,
        "build_mac",
        json!({"root": ctx.root.display().to_string(), "argv": hb_argv}),
        &env,
    );
    if rc == 0 && no_print_flag(&rest) {
        println!("hydo: next:  hydo run \"disk=none | run\"     or     hydo explore --seeds 1-20");
    }
    rc
}
