//! `hydo build`: the manifest compiled into a guest image by the hydo-build crate (M103). One
//! behaviour on both platforms since M107: with no manifest, ./hydo/hydo.yaml; the image goes to
//! ./.hydo for a manifest in a hydo/ folder and to ./hydo-out otherwise (or `--out DIR`); after
//! a build, the `hydo: next:` line. The release patches (the bundled /nix/store closures and the
//! root owners on Linux, the helper VM for mke2fs and the bundled tc on the Mac) are
//! hydo_build::Release. Only the architecture differs: x86_64 images on Linux, arm64 on the Mac.

use crate::ctx::{abspath, die, take, Ctx, Platform, OUT, PROJECT_OUT};
use crate::hvxenv;
use std::path::Path;

/// Where `hydo build` writes: ./.hydo for a manifest in a hydo/ folder, else ./hydo-out.
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
    let guest = ctx.guest();
    let (release, mut hb_argv) = match ctx.platform {
        Platform::Linux => {
            if rest.iter().any(|a| a == "--arch") {
                die(
                    "this is the x86_64 Linux release; arm64 images are built with the Mac release",
                );
            }
            setdefault("HYDO_BUSYBOX", &guest.join("busybox"));
            setdefault("HYDO_SEEDRNG", &guest.join("hvx_seedrng"));
            setdefault("HYDO_FCBIN", &guest.join("hydo_fc"));
            setdefault("HYDO_AGENT_BIN", &guest.join("hydo-agent"));
            setdefault("HYDO_MKE2FS", &ctx.p("tools/bin/mke2fs"));
            setdefault("HYDO_FAKETIME", &ctx.libexec().join("faketime"));
            setdefault("HYDO_MODROOT", &guest.join("modules"));
            if let Some((_, path)) = hvxenv::tool_env(ctx).into_iter().find(|(k, _)| k == "PATH") {
                std::env::set_var("PATH", path);
            }
            // the staged root's new directories and text files get the same modes for everyone
            // SAFETY: umask only changes this process's file-creation mask.
            unsafe { libc::umask(0o022) };
            (
                hydo_build::Release::Linux {
                    root: ctx.root.clone(),
                },
                vec![manifest, "--build-dir".to_string(), out],
            )
        }
        Platform::Mac => {
            setdefault("HYDO_BUSYBOX_ARM64", &guest.join("busybox"));
            setdefault("HYDO_BUSYBOX", &guest.join("busybox-applets"));
            setdefault("HYDO_MKE2FS", &guest.join("mke2fs"));
            setdefault("HYDO_FAKETIME", &guest.join("mke2fs"));
            setdefault("HYDO_WORLDBIN_ARM64", &guest.join("hydo-world"));
            setdefault("HYDO_READ_BIN_ARM64", &guest.join("hydo-read"));
            setdefault("HYDO_ARM64_STORE", &guest.join("tc"));
            setdefault("HYDO_FCBIN_ARM64", &guest.join("hydo_fc"));
            setdefault("HYDO_AGENT_BIN_ARM64", &guest.join("hydo-agent"));
            (
                hydo_build::Release::Mac {
                    root: ctx.root.clone(),
                },
                vec![
                    manifest,
                    "--arch".to_string(),
                    "arm64".to_string(),
                    "--build-dir".to_string(),
                    out,
                ],
            )
        }
    };
    hb_argv.extend(rest.iter().cloned());
    let bctx = hydo_build::Ctx {
        onboard: ctx.onboard(),
        release,
    };
    let rc = hydo_build::main(&bctx, &hb_argv);
    let agent = rest
        .windows(2)
        .any(|w| w[0] == "--control" && w[1] == "agent");
    if rc == 0 && no_print_flag(&rest) && agent {
        // M121: the engines boot such an image under lock-step control; the controller that
        // drives it (hydo-control) is not part of hydo run/explore yet
        println!("hydo: next:  an agent image runs under a host controller (lock-step control from boot); hydo run and hydo explore do not drive one yet");
    } else if rc == 0 && no_print_flag(&rest) {
        println!("hydo: next:  hydo run \"disk=none | run\"     or     hydo explore --seeds 1-20");
    }
    rc
}
