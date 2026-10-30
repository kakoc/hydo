//! `hydo build`: the manifest compiled into a guest image by the hydo-build crate (M103; before
//! that, hydo/onboard/hydo-build through hydo_bridge.py's `build_linux` / `build_mac`). The
//! argument handling, the defaults and the line printed after a build are the Python CLIs'; the
//! release patches (the bundled /nix/store closures and the root owners on Linux, the helper VM
//! for mke2fs and the bundled tc on the Mac) are hydo_build::Release.

use crate::ctx::{abspath, die, take, take_mac, Ctx, Platform, OUT, PROJECT_OUT};
use crate::hvxenv;
use std::path::Path;

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

/// `os.environ.setdefault(k, v)` for the build (the bridge passed these to its subprocess).
fn setdefault(k: &str, v: &Path) {
    if std::env::var_os(k).is_none() {
        std::env::set_var(k, v);
    }
}

fn no_print_flag(rest: &[String]) -> bool {
    !rest
        .iter()
        .any(|f| f == "--check" || f == "--print-shell" || f == "--print-init")
}

pub fn cmd_build(ctx: &Ctx, argv: &[String]) -> i32 {
    match ctx.platform {
        Platform::Linux => build_linux(ctx, argv),
        Platform::Mac => build_mac(ctx, argv),
    }
}

fn run(ctx: &Ctx, release: hydo_build::Release, argv: Vec<String>, rest: &[String]) -> i32 {
    let bctx = hydo_build::Ctx {
        onboard: ctx.onboard(),
        release,
    };
    let rc = hydo_build::main(&bctx, &argv);
    if rc == 0 && no_print_flag(rest) {
        println!("hydo: next:  hydo run \"disk=none | run\"     or     hydo explore --seeds 1-20");
    }
    rc
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
    let guest = ctx.guest();
    setdefault("HYDO_BUSYBOX", &guest.join("busybox"));
    setdefault("HYDO_SEEDRNG", &guest.join("hvx_seedrng"));
    setdefault("HYDO_FCBIN", &guest.join("hydo_fc"));
    setdefault("HYDO_MKE2FS", &ctx.p("tools/bin/mke2fs"));
    setdefault("HYDO_FAKETIME", &ctx.libexec().join("faketime"));
    setdefault("HYDO_MODROOT", &guest.join("modules"));
    if let Some((_, path)) = hvxenv::tool_env(ctx).into_iter().find(|(k, _)| k == "PATH") {
        std::env::set_var("PATH", path);
    }
    // the staged root's new directories and text files get the same modes for everyone
    unsafe { libc::umask(0o022) };
    let mut hb_argv = vec![manifest, "--build-dir".to_string(), out];
    hb_argv.extend(rest.iter().cloned());
    run(
        ctx,
        hydo_build::Release::Linux {
            root: ctx.root.clone(),
        },
        hb_argv,
        &rest,
    )
}

fn build_mac(ctx: &Ctx, argv: &[String]) -> i32 {
    if argv.first().is_none_or(|a| a.starts_with('-')) {
        die("usage: hydo build <hydo.yaml> [--out DIR] [--set KEY=VALUE ...]");
    }
    let manifest = argv[0].clone();
    let mut rest = argv[1..].to_vec();
    let out = take_mac(&mut rest, "--out").unwrap_or_else(|| OUT.to_string());
    let guest = ctx.guest();
    setdefault("HYDO_BUSYBOX_ARM64", &guest.join("busybox"));
    setdefault("HYDO_BUSYBOX", &guest.join("busybox-applets"));
    setdefault("HYDO_MKE2FS", &guest.join("mke2fs"));
    setdefault("HYDO_FAKETIME", &guest.join("mke2fs"));
    setdefault("HYDO_WORLDBIN_ARM64", &guest.join("hydo-world"));
    setdefault("HYDO_READ_BIN_ARM64", &guest.join("hydo-read"));
    setdefault("HYDO_ARM64_STORE", &guest.join("tc"));
    setdefault("HYDO_FCBIN_ARM64", &guest.join("hydo_fc"));
    let mut hb_argv = vec![
        manifest,
        "--arch".to_string(),
        "arm64".to_string(),
        "--build-dir".to_string(),
        out,
    ];
    hb_argv.extend(rest.iter().cloned());
    run(
        ctx,
        hydo_build::Release::Mac {
            root: ctx.root.clone(),
        },
        hb_argv,
        &rest,
    )
}
