//! The Rust `hydo` against recorded answers: for every case below that needs no VM, the exit
//! code, stdout, stderr and every file written must equal `tests/golden/cli.jsonl`.
//!
//! How the answers were recorded: until M107 this test ran the two Python CLIs it replaced
//! (hydo/dist/linux/hydo and hydo/dist/mac/hydo) next to the Rust one and compared them. In M107
//! the Python CLIs' answers were recorded once (commit 2a58c4b, Python 3.15.0 on the Mac; restore
//! the reference with `git show 2a58c4b:hydo/dist/linux/hydo` and `.../mac/hydo`), then the
//! deliberate unifications of M107 (one behaviour on both platforms, docs/HISTORY.md, Milestone
//! 107) were applied to the answers they change, and nothing else. From now on the Rust CLI is
//! the reference: `HYDO_BLESS=1 cargo test -p hydo-cli --test equivalence` rewrites the golden
//! from the Rust output; review its diff like code.
//!
//! Each case runs from one release-like root (bin/hydo, a symlinked hydo/, hv/ and
//! skill/hydo-onboard) in a fresh working directory with HOME inside it. Paths in the output are
//! written as <ROOT>, <CWD> and <REPO>. Outputs longer than 4 KiB and files longer than 256 bytes are kept as
//! the first 16 hex digits of their SHA-256 and their length. The
//! Linux behaviour is exercised on any host with HYDO_CLI_PLATFORM=linux. Runs that boot a guest
//! are compared on the machines themselves (docs/HISTORY.md: the release proofs). In six cases
//! the old Python CLIs crashed with a traceback (a missing flag value, a missing known-errors
//! file); the golden holds the Rust CLI's `hydo:` message there (M101, M104). A golden answer
//! that still holds a traceback accepts any `hydo:` message.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap()
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/cli.jsonl")
}

static N: AtomicU32 = AtomicU32::new(0);
static FIXED_CRASHES: AtomicU32 = AtomicU32::new(0);

/// The golden state: the recorded answers by key, the answers produced now (for HYDO_BLESS), and
/// the differences found.
struct State {
    golden: BTreeMap<String, Value>,
    produced: Vec<(String, Value)>,
    diffs: Vec<String>,
    case: usize,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn bless() -> bool {
    std::env::var_os("HYDO_BLESS").is_some() || record_python()
}

/// M107 only, while the Python CLIs were still in the tree: HYDO_RECORD_PYTHON=1 wrote the golden
/// from them (`python3 bin/hydo-py-<platform>`). Kept for the record; the files are gone now.
fn record_python() -> bool {
    std::env::var_os("HYDO_RECORD_PYTHON").is_some()
}

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

/// A release-like root: bin/hydo, hydo -> repo/hydo, hv -> repo/hv, skill/hydo-onboard ->
/// repo/skills/hydo-onboard, guest/ with the closure listings the build patches read.
fn release() -> PathBuf {
    let r = scratch("rel");
    let repo = repo();
    std::fs::create_dir_all(r.join("bin")).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_hydo"), r.join("bin/hydo")).unwrap();
    if record_python() {
        for p in ["linux", "mac"] {
            let src = repo.join(format!("hydo/dist/{p}/hydo"));
            std::fs::copy(src, r.join(format!("bin/hydo-py-{p}"))).unwrap();
        }
    }
    std::os::unix::fs::symlink(repo.join("hydo"), r.join("hydo")).unwrap();
    std::os::unix::fs::symlink(repo.join("hv"), r.join("hv")).unwrap();
    std::fs::create_dir_all(r.join("skill")).unwrap();
    std::os::unix::fs::symlink(
        repo.join("skills/hydo-onboard"),
        r.join("skill/hydo-onboard"),
    )
    .unwrap();
    std::fs::create_dir_all(r.join("guest")).unwrap();
    std::fs::write(r.join("guest/store-closures.txt"), "").unwrap();
    std::fs::write(r.join("guest/tc-closure.txt"), "").unwrap();
    std::fs::create_dir_all(r.join("libexec/bin")).unwrap();
    r
}

fn snapshot(dir: &Path, base: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    v.sort_by_key(|e| e.file_name());
    for e in v {
        let p = e.path();
        let rel = p.strip_prefix(base).unwrap().display().to_string();
        let ft = std::fs::symlink_metadata(&p).unwrap().file_type();
        if ft.is_dir() {
            out.insert(format!("{rel}/"), Vec::new());
            snapshot(&p, base, out);
        } else if ft.is_symlink() {
            out.insert(
                rel,
                std::fs::read_link(&p)
                    .unwrap()
                    .display()
                    .to_string()
                    .into_bytes(),
            );
        } else {
            out.insert(rel, std::fs::read(&p).unwrap());
        }
    }
}

/// Paths as placeholders, longest first (a temporary directory's /private/var and /var forms).
fn normalise(text: &str, root: &Path, cwd: &Path) -> String {
    let mut pairs: Vec<(String, &str)> = Vec::new();
    for (p, name) in [(root, "<ROOT>"), (cwd, "<CWD>")] {
        let s = p.display().to_string();
        if let Some(short) = s.strip_prefix("/private") {
            pairs.push((short.to_string(), name));
        }
        pairs.push((s, name));
    }
    pairs.push((repo().display().to_string(), "<REPO>"));
    pairs.sort_by_key(|(s, _)| std::cmp::Reverse(s.len()));
    let mut t = text.to_string();
    for (s, name) in pairs {
        t = t.replace(&s, name);
    }
    // `hydo errors accept` writes today's date into the known list
    t.replace(&today(), "<TODAY>")
}

/// Today's local date as `date +%F` prints it.
fn today() -> String {
    let o = Command::new("date").arg("+%F").output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A text kept verbatim up to `max` bytes, else as "sha256:<first 16 hex digits> len=<n>".
fn keep(text: &str, max: usize) -> Value {
    if text.len() <= max {
        json!(text)
    } else {
        let h = Sha256::digest(text.as_bytes());
        let hex: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
        json!(format!("sha256:{hex} len={}", text.len()))
    }
}

fn run_one(root: &Path, cwd: &Path, platform: &str, args: &[&str]) -> Value {
    let mut c = if record_python() {
        let mut c = Command::new("python3");
        c.arg(root.join(format!("bin/hydo-py-{platform}")));
        c
    } else {
        let mut c = Command::new(root.join("bin/hydo"));
        c.env("HYDO_CLI_PLATFORM", platform);
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
    let n = |b: &[u8]| normalise(&String::from_utf8_lossy(b), root, cwd);
    let files: serde_json::Map<String, Value> = files
        .iter()
        .map(|(k, v)| (k.clone(), keep(&n(v), 256)))
        .collect();
    json!({
        "rc": o.status.code(),
        "stdout": keep(&n(&o.stdout), 4096),
        "stderr": keep(&traceback_tail(&n(&o.stderr)), 4096),
        "files": files,
    })
}

/// A Python crash printed a traceback whose frames named the calling file: keep only the text
/// before it and the exception line.
fn traceback_tail(s: &str) -> String {
    match s.find("Traceback (most recent call last):") {
        Some(i) => format!(
            "{}Traceback ... {}",
            &s[..i],
            s.trim_end().lines().last().unwrap_or("")
        ),
        None => s.to_string(),
    }
}

/// Compare one answer with the golden one (a recorded Python traceback may have become a
/// `hydo:` message).
fn compare(key: &str, want: &Value, got: &Value, diffs: &mut Vec<String>) {
    for f in ["rc", "stdout", "files"] {
        if want[f] != got[f] {
            diffs.push(format!(
                "{key}: {f} differs\n  golden: {}\n  now:    {}",
                want[f], got[f]
            ));
        }
    }
    let (ws, gs) = (
        want["stderr"].as_str().unwrap_or(""),
        got["stderr"].as_str().unwrap_or(""),
    );
    if ws.contains("Traceback ...") && gs.starts_with("hydo: ") && !gs.contains("Traceback") {
        FIXED_CRASHES.fetch_add(1, Ordering::SeqCst);
    } else if ws != gs {
        diffs.push(format!(
            "{key}: stderr differs\n  golden: {ws:?}\n  now:    {gs:?}"
        ));
    }
}

/// Run one case (several invocations in a row in the same fresh directory, e.g. init twice) and
/// compare each with the golden.
fn same(root: &Path, platform: &str, setup: Setup, steps: &[&[&str]]) -> usize {
    let cwd = scratch("cwd");
    std::fs::create_dir_all(cwd.join("home")).unwrap();
    setup(&cwd);
    let mut g = STATE.lock().unwrap();
    let st = g.as_mut().unwrap();
    st.case += 1;
    for (i, args) in steps.iter().enumerate() {
        let key = format!("{platform} c{:03} s{i} {}", st.case, args.join(" "));
        let got = run_one(root, &cwd, platform, args);
        match st.golden.get(&key) {
            Some(want) if !bless() => compare(&key, want, &got, &mut st.diffs),
            None if !bless() => st.diffs.push(format!("{key}: not in the golden")),
            _ => {}
        }
        st.produced.push((key, got));
    }
    let _ = std::fs::remove_dir_all(&cwd);
    steps.len()
}

type Setup = fn(&Path);

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
    std::fs::create_dir_all(cwd.join("run1")).unwrap();
    std::fs::copy(
        fixtures().join("run-errors/one.errors.json"),
        cwd.join("run1/one.errors.json"),
    )
    .unwrap();
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

/// A project with a hydo/ folder (the test composer's example).
fn with_project(cwd: &Path) {
    let src = repo().join("hydo/onboard/examples/redis-composed/hydo");
    let st = Command::new("cp")
        .arg("-R")
        .arg(&src)
        .arg(cwd.join("hydo"))
        .status()
        .unwrap();
    assert!(st.success());
}

/// `hydo doctor` needs no Python on either platform, says what is missing instead of crashing
/// (a release without libexec/faketime), and ends with its summary line (M107).
#[test]
fn doctor_needs_no_python() {
    let root = release();
    for plat in ["linux", "mac"] {
        let o = Command::new(root.join("bin/hydo"))
            .env("HYDO_CLI_PLATFORM", plat)
            .env("PATH", "/usr/bin:/bin")
            .arg("doctor")
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        assert_eq!(o.status.code(), Some(1), "{plat}: {out}");
        assert!(!out.to_lowercase().contains("python"), "{plat}: {out}");
        assert!(
            out.trim_end()
                .lines()
                .last()
                .is_some_and(|l| l.starts_with("doctor: ") && l.contains(" failed, ")),
            "{plat}: {out}"
        );
        assert!(
            o.stderr.is_empty(),
            "{plat}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        if plat == "linux" {
            assert!(
                out.contains(
                    "FAIL bundled mke2fs runs (under the frozen clock)  -> cannot run libexec/faketime"
                ),
                "{out}"
            );
        } else {
            assert!(out.contains("WARN present: guest/hydo_fc"), "{out}");
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn load_golden() -> BTreeMap<String, Value> {
    let text = std::fs::read_to_string(golden_path()).unwrap_or_default();
    text.lines()
        .filter(|l| l.starts_with('{'))
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("golden line");
            (v["key"].as_str().unwrap().to_string(), v["answer"].clone())
        })
        .collect()
}

fn write_golden(produced: &[(String, Value)]) {
    let mut out = String::from(
        "# The hydo CLI's answers (tests/equivalence.rs): one JSON object per invocation. Recorded from\n\
         # the Python CLIs at 2a58c4b (Python 3.15.0) in M107, with M107's unifications applied; since\n\
         # then written by HYDO_BLESS=1 from the Rust CLI.\n",
    );
    for (k, v) in produced {
        out.push_str(&serde_json::to_string(&json!({"key": k, "answer": v})).unwrap());
        out.push('\n');
    }
    std::fs::create_dir_all(golden_path().parent().unwrap()).unwrap();
    std::fs::write(golden_path(), out).unwrap();
}

#[test]
fn cli_matches_golden() {
    *STATE.lock().unwrap() = Some(State {
        golden: load_golden(),
        produced: Vec::new(),
        diffs: Vec::new(),
        case: 0,
    });
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
        for a in [
            &[][..],
            &["--help"],
            &["help"],
            &["-h"],
            &["bogus"],
            &["module"],
        ] {
            n += same(&root, plat, nothing, &[a]);
        }
        // guide / skill / init (hydo_agents.py through the bridge)
        n += same(
            &root,
            plat,
            nothing,
            &[
                &["guide"],
                &["guide", "--list"],
                &["guide", "composer"],
                &["guide", "manifest"],
            ],
        );
        n += same(
            &root,
            plat,
            nothing,
            &[&["guide", "no-such-topic"], &["guide", "../../etc/passwd"]],
        );
        n += same(
            &root,
            plat,
            nothing,
            &[
                &["skill"],
                &["skill", "agents"],
                &["skill", "install", "--to", "out"],
                &["skill", "install", "--to", "out"],
            ],
        );
        n += same(
            &root,
            plat,
            nothing,
            &[
                &["skill", "install", "--agent", "codex"],
                &["skill", "install", "--agent", "nope"],
            ],
        );
        n += same(
            &root,
            plat,
            nothing,
            &[
                &["init", "--dir", "proj", "--name", "demo"],
                &["init", "--dir", "proj", "--name", "demo"],
            ],
        );
        // report, save, errors
        n += same(
            &root,
            plat,
            nothing,
            &[&["report"], &["report", "elsewhere"]],
        );
        n += same(
            &root,
            plat,
            with_explore,
            &[&["report"], &["report", "hydo-out/explore"]],
        );
        n += same(
            &root,
            plat,
            with_explore,
            &[
                &["save"],
                &["save", "hydo-out/explore", "--to", "regs"],
                &["save", "hydo-out/explore", "--to", "regs"],
                &["save", "hydo-out/explore"],
            ],
        );
        n += same(
            &root,
            plat,
            with_explore,
            &[
                &["errors"],
                &["errors", "help"],
                &["errors", "bogus"],
                &["errors", "list"],
                &["errors", "list", "errs"],
                &["errors", "check"],
            ],
        );
        n += same(
            &root,
            plat,
            with_explore,
            &[
                &[
                    "errors", "accept", "f0c7", "--by", "t", "--from", "errs", "--file", "k.yaml",
                ],
                &[
                    "errors",
                    "accept",
                    "f0c7",
                    "--reason",
                    "expected at boot",
                    "--by",
                    "t",
                    "--from",
                    "errs",
                    "--file",
                    "k.yaml",
                ],
                &["errors", "check", "--file", "k.yaml"],
                &[
                    "errors", "accept", "zzz", "--reason", "x", "--by", "t", "--from", "errs",
                    "--file", "k.yaml",
                ],
            ],
        );
        // errors (the hydo-errors crate): a list written with --with/--during, found by
        // default, accepting again, a run directory's kinds, refusals
        n += same(
            &root,
            plat,
            with_explore,
            &[
                &[
                    "errors",
                    "accept",
                    "f0c7",
                    "--reason",
                    "boot banner",
                    "--by",
                    "t",
                    "--from",
                    "errs",
                    "--file",
                    "hydo/known-errors.yaml",
                    "--with",
                    "C",
                    "--with",
                    "disk=ioerr",
                    "--during",
                    "K=redis",
                ],
                &["errors", "check"],
                &[
                    "errors", "accept", "f0c7", "--reason", "again", "--from", "errs",
                ],
                &[
                    "errors", "accept", "0c3b", "84dc", "--reason", "io", "--by", "t", "--from",
                    "run1",
                ],
                &["errors", "list", "run1"],
                &["errors", "check", "--file", "hydo/known-errors.yaml"],
                &["errors", "accept", "", "--reason", "r", "--from", "run1"],
                &["errors", "accept", "f0c7", "--reason"],
                &["errors", "accept", "f0c7", "--reason", "r", "--from"],
                &["errors", "list", "nowhere"],
                &["errors", "check", "--file", "missing.yaml"],
            ],
        );
        // compose (hydo_compose.py through the bridge)
        n += same(
            &root,
            plat,
            with_compose,
            &[
                &["compose"],
                &["compose", "--help"],
                &[
                    "compose",
                    "docker-compose.yaml",
                    "--out",
                    "h.yaml",
                    "--no-resolve",
                ],
            ],
        );
        // build: argument handling and hydo-build's checks (no image is built here)
        n += same(
            &root,
            plat,
            nothing,
            &[
                &["build"],
                &["build", "--check"],
                &["build", &bad, "--check"],
            ],
        );
        let manifests: Vec<&str> = if plat == "linux" {
            vec![&redis, &composed, &shop, &stack]
        } else {
            vec![&mac_redis, &composed]
        };
        for mf in manifests {
            n += same(
                &root,
                plat,
                nothing,
                &[
                    &["build", mf, "--check"],
                    &["build", mf, "--print-shell"],
                    &["build", mf, "--print-init"],
                    &["build", mf, "--set", "x=1", "--check"],
                ],
            );
        }
        // runs: everything up to the engine
        n += same(
            &root,
            plat,
            with_explore,
            &[
                &["run"],
                &["run", "--image"],
                &["run", "--image", "bogus.img"],
                &["run", "missing.img", "disk=none | run"],
            ],
        );
        n += same(
            &root,
            plat,
            with_explore,
            &[
                &["repro"],
                &["repro", "--regressions"],
                &["explore"],
                &["repro", "rundir"],
            ],
        );
        n += same(
            &root,
            plat,
            with_explore,
            &[
                &["run", "--image", "bogus.img", "a", "b"],
                &["explore", "--image", "bogus.img"],
            ],
        );
    }
    // Linux only: the module verbs that need no root (dry runs and status)
    n += same(
        &root,
        "linux",
        nothing,
        &[
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
        ],
    );
    // M107: one behaviour on both platforms (no Python reference: the Rust CLI's answers)
    for plat in ["linux", "mac"] {
        // `hydo build` with no manifest uses ./hydo/hydo.yaml on both, and an image of a hydo/
        // folder goes to ./.hydo
        n += same(
            &root,
            plat,
            with_project,
            &[
                &["build", "--check"],
                &["build", "--out", "o", "--check"],
                &["build", "hydo/hydo.yaml", "--check"],
            ],
        );
        // a positional image on both (Linux *.img, Mac *.cpio.gz), checked to exist
        n += same(
            &root,
            plat,
            with_explore,
            &[
                &["run", "bogus.cpio.gz"],
                &["run", "missing.cpio.gz", "disk=none | run"],
                &["run", "--out", "o", "--known-errors", "k.yaml"],
                &["repro", "missing.cpio.gz", "--regressions"],
                &["explore", "missing.cpio.gz"],
            ],
        );
    }
    let _ = std::fs::remove_dir_all(&root);
    let st = STATE.lock().unwrap().take().unwrap();
    if bless() {
        write_golden(&st.produced);
        eprintln!(
            "wrote {} answers to {}",
            st.produced.len(),
            golden_path().display()
        );
        return;
    }
    let extra: Vec<&String> = st
        .golden
        .keys()
        .filter(|k| !st.produced.iter().any(|(p, _)| p == *k))
        .collect();
    assert!(
        extra.is_empty(),
        "golden answers no case asks for: {extra:?}"
    );
    eprintln!(
        "cli: {n} invocations gave the golden exit code, stdout, stderr and files ({} where the old \
         Python CLI crashed and the Rust CLI prints a `hydo:` message); {} differences",
        FIXED_CRASHES.load(Ordering::SeqCst),
        st.diffs.len()
    );
    assert!(
        st.diffs.is_empty(),
        "{} differences:\n{}",
        st.diffs.len(),
        st.diffs.join("\n")
    );
}
