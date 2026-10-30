//! The Rust `hydo` against the two Python CLIs it replaces (hydo/dist/linux/hydo and
//! hydo/dist/mac/hydo, kept as the reference until every port is done): the same exit code, the
//! same stdout and stderr, and the same files written, for every case below that needs no VM.
//! Both run from one release-like root (bin/, a symlinked hydo/, hv/ and skill/hydo-onboard) in the
//! same working directory, one after the other. The Linux behaviour is exercised on any host with
//! HYDO_CLI_PLATFORM=linux. Runs that boot a guest are compared on the machines themselves
//! (docs/HISTORY.md, M101: the quickstarts and the explorer).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap()
}

static N: AtomicU32 = AtomicU32::new(0);
static FIXED_CRASHES: AtomicU32 = AtomicU32::new(0);

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hydo-eq-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        tag
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// A release-like root: bin/{hydo, hydo-py-linux, hydo-py-mac}, hydo -> repo/hydo, hv -> repo/hv,
/// skill/hydo-onboard -> repo/skills/hydo-onboard, guest/ with the closure listings the build
/// patches read.
fn release() -> PathBuf {
    let r = scratch("rel");
    let repo = repo();
    std::fs::create_dir_all(r.join("bin")).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_hydo"), r.join("bin/hydo")).unwrap();
    std::fs::copy(repo.join("hydo/dist/linux/hydo"), r.join("bin/hydo-py-linux")).unwrap();
    std::fs::copy(repo.join("hydo/dist/mac/hydo"), r.join("bin/hydo-py-mac")).unwrap();
    std::os::unix::fs::symlink(repo.join("hydo"), r.join("hydo")).unwrap();
    std::os::unix::fs::symlink(repo.join("hv"), r.join("hv")).unwrap();
    std::fs::create_dir_all(r.join("skill")).unwrap();
    std::os::unix::fs::symlink(repo.join("skills/hydo-onboard"), r.join("skill/hydo-onboard")).unwrap();
    std::fs::create_dir_all(r.join("guest")).unwrap();
    std::fs::write(r.join("guest/store-closures.txt"), "").unwrap();
    std::fs::write(r.join("guest/tc-closure.txt"), "").unwrap();
    std::fs::create_dir_all(r.join("libexec/bin")).unwrap();
    r
}

#[derive(Debug, PartialEq)]
struct Outcome {
    rc: Option<i32>,
    stdout: String,
    stderr: String,
    files: BTreeMap<String, Vec<u8>>,
}

fn snapshot(dir: &Path, base: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(Result::ok).collect();
    v.sort_by_key(|e| e.file_name());
    for e in v {
        let p = e.path();
        let rel = p.strip_prefix(base).unwrap().display().to_string();
        let ft = std::fs::symlink_metadata(&p).unwrap().file_type();
        if ft.is_dir() {
            out.insert(format!("{rel}/"), Vec::new());
            snapshot(&p, base, out);
        } else if ft.is_symlink() {
            out.insert(rel, std::fs::read_link(&p).unwrap().display().to_string().into_bytes());
        } else {
            out.insert(rel, std::fs::read(&p).unwrap());
        }
    }
}

type Setup = fn(&Path);

fn run_one(root: &Path, cwd: &Path, rust: bool, platform: &str, args: &[&str]) -> Outcome {
    let mut c = if rust {
        let mut c = Command::new(root.join("bin/hydo"));
        c.env("HYDO_CLI_PLATFORM", platform);
        c
    } else {
        let mut c = Command::new("python3");
        c.arg(root.join(format!("bin/hydo-py-{platform}")));
        c
    };
    let o = c
        .args(args)
        .current_dir(cwd)
        .env_remove("HYDO_LOCK")
        .env_remove("HVX_DEV")
        .env("HOME", cwd.join("home"))
        .output()
        .unwrap();
    let mut files = BTreeMap::new();
    snapshot(cwd, cwd, &mut files);
    Outcome {
        rc: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: traceback_tail(&String::from_utf8_lossy(&o.stderr)),
        files,
    }
}

/// A Python crash prints a traceback whose frames name the calling file (the old CLI, or the
/// bridge that now calls the same function): compare only the text before it and the exception.
fn traceback_tail(s: &str) -> String {
    match s.find("Traceback (most recent call last):") {
        Some(i) => format!("{}Traceback ... {}", &s[..i], s.trim_end().lines().last().unwrap_or("")),
        None => s.to_string(),
    }
}

/// Run one case with the Python CLI and with the Rust CLI in the same fresh directory and
/// compare. `steps`: several invocations in a row in the same directory (e.g. init twice).
fn same(root: &Path, platform: &str, setup: Setup, steps: &[&[&str]]) -> usize {
    let cwd = scratch("cwd");
    let mut outs = [Vec::new(), Vec::new()];
    for (k, rust) in [false, true].into_iter().enumerate() {
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(cwd.join("home")).unwrap();
        setup(&cwd);
        for args in steps {
            outs[k].push(run_one(root, &cwd, rust, platform, args));
        }
    }
    for (i, (py, rs)) in outs[0].iter().zip(outs[1].iter()).enumerate() {
        assert_eq!(py.rc, rs.rc, "{platform} {:?}: exit code\npy stderr: {}\nrs stderr: {}", steps[i], py.stderr, rs.stderr);
        assert_eq!(py.stdout, rs.stdout, "{platform} {:?}: stdout", steps[i]);
        if py.stderr.starts_with("Traceback ...") && rs.stderr.starts_with("hydo: ") && !rs.stderr.contains("Traceback") {
            // the Python CLI crashed here (a missing flag value); the Rust CLI says what is wrong
            eprintln!("{platform} {:?}: Python crashed ({}), Rust: {}", steps[i], py.stderr.trim(), rs.stderr.trim());
            FIXED_CRASHES.fetch_add(1, Ordering::SeqCst);
        } else {
            assert_eq!(py.stderr, rs.stderr, "{platform} {:?}: stderr", steps[i]);
        }
        let names = |o: &Outcome| o.files.keys().cloned().collect::<Vec<_>>();
        assert_eq!(names(py), names(rs), "{platform} {:?}: files written", steps[i]);
        for (k, v) in &py.files {
            assert!(rs.files[k] == *v, "{platform} {:?}: content of {k}", steps[i]);
        }
    }
    let _ = std::fs::remove_dir_all(&cwd);
    steps.len()
}

fn nothing(_: &Path) {}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn with_explore(cwd: &Path) {
    let d = cwd.join("hydo-out/explore");
    std::fs::create_dir_all(&d).unwrap();
    for f in ["report.json", "report.md"] {
        std::fs::copy(fixtures().join("explore-composed").join(f), d.join(f)).unwrap();
    }
    let e = cwd.join("errs");
    std::fs::create_dir_all(&e).unwrap();
    for f in ["errors.json", "report.json"] {
        std::fs::copy(fixtures().join("explore-errors").join(f), e.join(f)).unwrap();
    }
    std::fs::create_dir_all(cwd.join("rundir")).unwrap();
    std::fs::write(cwd.join("bogus.img"), b"not an image").unwrap();
}

fn with_compose(cwd: &Path) {
    let src = repo().join("hydo/onboard/tests/compose/simple");
    for e in std::fs::read_dir(&src).unwrap().filter_map(Result::ok) {
        if e.file_type().unwrap().is_file() {
            std::fs::copy(e.path(), cwd.join(e.file_name())).unwrap();
        }
    }
}

fn python_available() -> bool {
    Command::new("python3").arg("-c").arg("import sys; sys.exit(sys.version_info < (3, 9))").status().map(|s| s.success()).unwrap_or(false)
}

#[test]
fn cli_matches_python() {
    if !python_available() {
        eprintln!("SKIPPED: python3 >= 3.9 is needed to run the Python CLIs");
        return;
    }
    let root = release();
    let repo = repo();
    let m = |rel: &str| repo.join(rel).display().to_string();
    let redis = m("hydo/onboard/examples/redis/hydo.yaml");
    let composed = m("hydo/onboard/examples/redis-composed/hydo/hydo.yaml");
    let shop = m("hydo/onboard/examples/shop/hydo.yaml");
    let stack = m("hydo/onboard/examples/stack/hydo.yaml");
    let mac_redis = m("hydo/onboard/examples-mac/redis/hydo.yaml");
    let bad = m("hydo/onboard/MANIFEST.md");
    let mut n = 0;
    for plat in ["linux", "mac"] {
        // help and dispatch
        for a in [&[][..], &["--help"], &["help"], &["-h"], &["bogus"], &["module"]] {
            n += same(&root, plat, nothing, &[a]);
        }
        // guide / skill / init (hydo_agents.py through the bridge)
        n += same(&root, plat, nothing, &[&["guide"], &["guide", "--list"], &["guide", "composer"], &["guide", "manifest"]]);
        n += same(&root, plat, nothing, &[&["guide", "no-such-topic"], &["guide", "../../etc/passwd"]]);
        n += same(&root, plat, nothing, &[&["skill"], &["skill", "agents"], &["skill", "install", "--to", "out"], &["skill", "install", "--to", "out"]]);
        n += same(&root, plat, nothing, &[&["skill", "install", "--agent", "codex"], &["skill", "install", "--agent", "nope"]]);
        n += same(&root, plat, nothing, &[&["init", "--dir", "proj", "--name", "demo"], &["init", "--dir", "proj", "--name", "demo"]]);
        // report, save, errors
        n += same(&root, plat, nothing, &[&["report"], &["report", "elsewhere"]]);
        n += same(&root, plat, with_explore, &[&["report"], &["report", "hydo-out/explore"]]);
        n += same(&root, plat, with_explore, &[&["save"], &["save", "hydo-out/explore", "--to", "regs"], &["save", "hydo-out/explore", "--to", "regs"], &["save", "hydo-out/explore"]]);
        n += same(&root, plat, with_explore, &[&["errors"], &["errors", "help"], &["errors", "bogus"], &["errors", "list"], &["errors", "list", "errs"], &["errors", "check"]]);
        n += same(&root, plat, with_explore, &[
            &["errors", "accept", "f0c7", "--by", "t", "--from", "errs", "--file", "k.yaml"],
            &["errors", "accept", "f0c7", "--reason", "expected at boot", "--by", "t", "--from", "errs", "--file", "k.yaml"],
            &["errors", "check", "--file", "k.yaml"],
            &["errors", "accept", "zzz", "--reason", "x", "--by", "t", "--from", "errs", "--file", "k.yaml"],
        ]);
        // compose (hydo_compose.py through the bridge)
        n += same(&root, plat, with_compose, &[&["compose"], &["compose", "--help"], &["compose", "docker-compose.yaml", "--out", "h.yaml", "--no-resolve"]]);
        // build: argument handling and hydo-build's checks (no image is built here)
        n += same(&root, plat, nothing, &[&["build"], &["build", "--check"], &["build", &bad, "--check"]]);
        let manifests: Vec<&str> = if plat == "linux" {
            vec![&redis, &composed, &shop, &stack]
        } else {
            vec![&mac_redis, &composed]
        };
        for mf in manifests {
            n += same(&root, plat, nothing, &[&["build", mf, "--check"], &["build", mf, "--print-shell"], &["build", mf, "--print-init"], &["build", mf, "--set", "x=1", "--check"]]);
        }
        // runs: everything up to the engine
        n += same(&root, plat, with_explore, &[&["run"], &["run", "--image"], &["run", "--image", "bogus.img"], &["run", "missing.img", "disk=none | run"]]);
        n += same(&root, plat, with_explore, &[&["repro"], &["repro", "--regressions"], &["explore"], &["repro", "rundir"]]);
        n += same(&root, plat, with_explore, &[&["run", "--image", "bogus.img", "a", "b"], &["explore", "--image", "bogus.img"]]);
    }
    // Linux only: the module verbs that need no root (dry runs and status)
    n += same(&root, "linux", nothing, &[
        &["module", "status"],
        &["module", "status", "--name", "side"],
        &["module", "udev"],
        &["module", "udev", "status"],
        &["module", "load", "--dry-run", "--name", "side"],
        &["module", "load", "--dry-run"],
        &["module", "unload", "--dry-run"],
        &["module", "bogus"],
        &["module", "build", "--name", "Bad"],
        &["module", "udev", "install", "--dry-run", "--name", "Bad"],
        &["module", "udev", "install", "--dry-run", "--group", "B@d"],
        &["module", "udev", "install", "--dry-run", "--mode", "0777"],
        &["module", "udev", "remove", "--dry-run"],
        &["module", "status", "extra"],
        &["module", "load", "--name"],
    ]);
    if cfg!(target_os = "macos") {
        n += same(&root, "mac", nothing, &[&["doctor"]]);
    }
    if cfg!(target_os = "linux") {
        n += same(&root, "linux", nothing, &[&["doctor"]]);
    }
    eprintln!(
        "equivalence: {n} invocations gave the same exit code, stdout and files; the same stderr in all but {} \
         where the Python CLI crashed with a traceback and the Rust CLI prints a `hydo:` message",
        FIXED_CRASHES.load(Ordering::SeqCst)
    );
    let _ = std::fs::remove_dir_all(&root);
}
