//! `cargo hydo` -- build a hydo-test binary twice (for the host, to write the plan; for the
//! guest, static musl, to run the drivers and checks) and call the installed `hydo` engine.
//!
//!     cargo hydo plan    [--bin NAME] [--arch x86_64|aarch64]   -> .hydo/plan.json + .hydo/guest/NAME
//!     cargo hydo build   [same options]                          plan, then `hydo build .hydo/plan.json --out .hydo`
//!     cargo hydo run|explore|repro|save|report [hydo args...]    plan + build, then `hydo <verb> <args>`
//!
//! Install: `cargo install --path sdk/rust-test` (puts cargo-hydo on PATH). The guest target
//! needs `rustup target add <arch>-unknown-linux-musl`; linking uses Rust's self-contained musl
//! and rust-lld, so no C toolchain is needed. --arch defaults to the engine of this machine:
//! aarch64 on macOS (the Mac backend), x86_64 elsewhere. `HYDO` names the engine command
//! (default `hydo` on PATH). `system()` runs only on the host, in `<bin> plan`.
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

fn die(msg: &str) -> ! {
    eprintln!("cargo hydo: {}", msg);
    exit(1)
}

fn run(cmd: &mut Command) {
    eprintln!("cargo hydo: {:?}", cmd);
    match cmd.status() {
        Ok(s) if s.success() => {}
        Ok(s) => die(&format!("{:?} exited {}", cmd, s)),
        Err(e) => die(&format!("{:?}: {}", cmd, e)),
    }
}

/// The cargo that runs us (`cargo hydo` sets CARGO), else `cargo` on PATH.
fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

fn package_name() -> String {
    let t = std::fs::read_to_string("Cargo.toml").unwrap_or_else(|_| die("no Cargo.toml here (run cargo hydo in the test crate)"));
    let mut in_pkg = false;
    for line in t.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_pkg = l == "[package]";
        } else if in_pkg && l.starts_with("name") {
            if let Some(v) = l.split('=').nth(1) {
                return v.trim().trim_matches('"').to_string();
            }
        }
    }
    die("no [package] name in Cargo.toml (pass --bin NAME)")
}

fn take(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    if i + 1 >= args.len() {
        die(&format!("{} needs a value", flag));
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Some(v)
}

/// Build both binaries and write .hydo/plan.json. Returns the plan's path.
fn plan(bin: &str, arch: &str) -> PathBuf {
    let triple = format!("{}-unknown-linux-musl", arch);
    run(Command::new(cargo()).args(["build", "--release", "--bin", bin]));
    let cwd = std::env::current_dir().unwrap();
    // reproducible guest binary: no host paths in it, static, self-contained musl + rust-lld
    let flags = format!(
        "-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static --remap-path-prefix={}=/src --remap-path-prefix={}=/home",
        cwd.display(),
        std::env::var("HOME").unwrap_or_else(|_| "/nonexistent".into())
    );
    run(Command::new(cargo()).args(["build", "--release", "--bin", bin, "--target", &triple]).env("RUSTFLAGS", flags)
        .env("CARGO_TARGET_DIR", "target/hydo-guest"));
    let guest = Path::new(".hydo/guest");
    std::fs::create_dir_all(guest).unwrap_or_else(|e| die(&format!(".hydo/guest: {}", e)));
    let built = Path::new("target/hydo-guest").join(&triple).join("release").join(bin);
    let dst = guest.join(bin);
    std::fs::copy(&built, &dst).unwrap_or_else(|e| die(&format!("{}: {}", built.display(), e)));
    let host = Path::new("target/release").join(bin);
    // HYDO_ARCH: system() may pick per-architecture files (a Redis for x86_64 or for aarch64)
    run(Command::new(&host).args(["plan", "--guest-binary", dst.to_str().unwrap(), "--guest-path", &format!("/bin/{}", bin),
                                  "--out", ".hydo/plan.json"]).env("HYDO_ARCH", arch));
    eprintln!("cargo hydo: wrote .hydo/plan.json (guest binary .hydo/guest/{}, {})", bin, triple);
    sync_regressions();
    PathBuf::from(".hydo/plan.json")
}

/// `hydo save` writes saved regressions to hydo/regressions/ of the crate; hydo-build reads a
/// regressions/ folder next to the plan, so mirror them into .hydo/regressions/ (exactly: a
/// regression deleted from hydo/regressions/ is gone from the next image too).
fn sync_regressions() {
    let dst = Path::new(".hydo/regressions");
    if dst.exists() {
        std::fs::remove_dir_all(dst).unwrap_or_else(|e| die(&format!("{}: {}", dst.display(), e)));
    }
    let src = Path::new("hydo/regressions");
    let mut names: Vec<PathBuf> = match std::fs::read_dir(src) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "schedule").unwrap_or(false)).collect(),
        Err(_) => return,
    };
    names.sort();
    if names.is_empty() {
        return;
    }
    std::fs::create_dir_all(dst).unwrap_or_else(|e| die(&format!("{}: {}", dst.display(), e)));
    for p in &names {
        let to = dst.join(p.file_name().unwrap());
        std::fs::copy(p, &to).unwrap_or_else(|e| die(&format!("{}: {}", p.display(), e)));
    }
    eprintln!("cargo hydo: {} saved regression(s) from hydo/regressions/ go into the plan", names.len());
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|a| a == "hydo").unwrap_or(false) {
        args.remove(0); // `cargo hydo ...` runs `cargo-hydo hydo ...`
    }
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        eprintln!("usage: cargo hydo plan|build [--bin NAME] [--arch x86_64|aarch64]\n       cargo hydo run|explore|repro|save|report [hydo arguments]");
        exit(2);
    }
    let verb = args.remove(0);
    let bin = take(&mut args, "--bin").unwrap_or_else(package_name);
    let arch = take(&mut args, "--arch").unwrap_or_else(|| if cfg!(target_os = "macos") { "aarch64".into() } else { "x86_64".into() });
    if arch != "x86_64" && arch != "aarch64" {
        die("--arch is x86_64 or aarch64");
    }
    let hydo = std::env::var("HYDO").unwrap_or_else(|_| "hydo".into());
    let p = plan(&bin, &arch);
    if verb == "plan" {
        return;
    }
    run(Command::new(&hydo).args(["build", p.to_str().unwrap(), "--out", ".hydo"]));
    if verb == "build" {
        return;
    }
    run(Command::new(&hydo).arg(&verb).args(&args));
}
