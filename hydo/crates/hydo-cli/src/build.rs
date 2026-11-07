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

/// `os.environ.setdefault(k, v)` for the build (the bridge passed these to its subprocess):
/// the release's path where the input's variable is not set.
fn setdefault(i: &mut hydo_config::build::Input, v: &Path) {
    i.set_default(v.to_string_lossy());
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
    let mut inputs = ctx.config.build.clone();
    let (release, mut hb_argv) = match ctx.platform {
        Platform::Linux => {
            if rest.iter().any(|a| a == "--arch") {
                die(
                    "this is the x86_64 Linux release; arm64 images are built with the Mac release",
                );
            }
            setdefault(&mut inputs.busybox, &guest.join("busybox"));
            setdefault(&mut inputs.seedrng, &guest.join("hvx_seedrng"));
            setdefault(&mut inputs.fcbin, &guest.join("hydo_fc"));
            setdefault(&mut inputs.agent_bin, &guest.join("hydo-agent"));
            setdefault(&mut inputs.mke2fs, &ctx.p("tools/bin/mke2fs"));
            setdefault(&mut inputs.faketime, &ctx.libexec().join("faketime"));
            setdefault(&mut inputs.modroot, &guest.join("modules"));
            setdefault(&mut inputs.worldbin, &guest.join("hydo-world"));
            setdefault(&mut inputs.worldbin_tls, &guest.join("hydo-world-tls"));
            setdefault(&mut inputs.read_bin, &guest.join("hydo-read"));
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
            setdefault(&mut inputs.busybox_arm64, &guest.join("busybox"));
            setdefault(&mut inputs.busybox, &guest.join("busybox-applets"));
            setdefault(&mut inputs.mke2fs, &guest.join("mke2fs"));
            setdefault(&mut inputs.faketime, &guest.join("mke2fs"));
            setdefault(&mut inputs.worldbin_arm64, &guest.join("hydo-world"));
            setdefault(
                &mut inputs.worldbin_tls_arm64,
                &guest.join("hydo-world-tls"),
            );
            setdefault(&mut inputs.read_bin_arm64, &guest.join("hydo-read"));
            setdefault(&mut inputs.arm64_store, &guest.join("tc"));
            setdefault(&mut inputs.fcbin_arm64, &guest.join("hydo_fc"));
            setdefault(&mut inputs.agent_bin_arm64, &guest.join("hydo-agent"));
            // the data disk is formatted in a helper VM by the Mac engine (`hydo-hvm hvm run`),
            // signed with the hypervisor entitlement first (cargo builds it unsigned); a missing
            // engine is reported by the build when it runs it
            let engine = hydo_engine::hvm::program(&ctx.root, &ctx.config.hvm);
            if engine.is_file() {
                let _ = hydo_engine::hvm::ensure_signed(&engine);
            }
            (
                hydo_build::Release::Mac {
                    root: ctx.root.clone(),
                    engine,
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
        inputs,
    };
    let rc = hydo_build::main(&bctx, &hb_argv);
    // every image is an agent image (M136): hydo run, repro and explore drive it through the host
    // controller (the image's /etc/hydo/control decides; older shell images still run)
    if rc == 0 && no_print_flag(&rest) {
        println!("hydo: next:  hydo run \"disk=none | run\"     or     hydo explore --seeds 1-20");
    }
    rc
}
