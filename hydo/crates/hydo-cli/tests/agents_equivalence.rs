//! `hydo guide | skill | init` (the hydo-agents crate) against hydo/onboard/hydo_agents.py, which it
//! replaces: the same exit code, stdout, stderr and files, for every guide topic, every `--agent`
//! value, `--user`, `--to`, and `hydo init` fresh and repeated. Each case runs the Python and then
//! the Rust in the same freshly made directory (absolute paths appear in the output). A second
//! pass runs the Rust from a root without a skill folder (the skill built into the binary) and
//! compares it with the Python reading the folder on disk, for every case whose output does not
//! name the folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap()
}

static N: AtomicU32 = AtomicU32::new(0);

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hydo-agents-eq-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        tag
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn snapshot(dir: &Path, base: &Path, out: &mut BTreeMap<String, (u32, Vec<u8>)>) {
    use std::os::unix::fs::PermissionsExt;
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    v.sort_by_key(|e| e.file_name());
    for e in v {
        let p = e.path();
        let rel = p.strip_prefix(base).unwrap().display().to_string();
        if rel == ".git" {
            continue; // made by the setup, not by the command
        }
        let md = std::fs::symlink_metadata(&p).unwrap();
        let mode = md.permissions().mode() & 0o777;
        if md.is_dir() {
            out.insert(format!("{rel}/"), (0, Vec::new()));
            snapshot(&p, base, out);
        } else if md.file_type().is_symlink() {
            let t = std::fs::read_link(&p).unwrap().display().to_string();
            out.insert(rel, (0, t.into_bytes()));
        } else {
            out.insert(rel, (mode, std::fs::read(&p).unwrap()));
        }
    }
}

#[derive(Debug, PartialEq)]
struct Outcome {
    rc: Option<i32>,
    stdout: String,
    stderr: String,
    files: BTreeMap<String, (u32, Vec<u8>)>,
}

/// One sequence of commands in a fresh directory `cwd` (with a git repository when `git`, and a
/// HOME inside it); `setup` prepares the directory first.
fn run_seq(
    rust: bool,
    root: &Path,
    cwd: &Path,
    cmds: &[&[&str]],
    setup: fn(&Path),
) -> Vec<Outcome> {
    let _ = std::fs::remove_dir_all(cwd);
    std::fs::create_dir_all(cwd.join("home")).unwrap();
    setup(cwd);
    let mut res = Vec::new();
    for c in cmds {
        let mut cmd = if rust {
            let mut x = Command::new(root.join("bin/hydo"));
            x.args(*c);
            x
        } else {
            let mut x = Command::new("python3");
            x.arg("-c")
                .arg("import sys; sys.path.insert(0, sys.argv[1]); import hydo_agents; sys.exit(hydo_agents.run(sys.argv[2], sys.argv[4:], sys.argv[3]))")
                .arg(repo().join("hydo/onboard"))
                .arg(c[0])
                .arg(root)
                .args(&c[1..]);
            x
        };
        let o = cmd
            .current_dir(cwd)
            .env("HOME", cwd.join("home"))
            .output()
            .unwrap();
        let mut files = BTreeMap::new();
        snapshot(cwd, cwd, &mut files);
        res.push(Outcome {
            rc: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            files,
        });
    }
    res
}

/// A release-like root for the Rust binary: bin/hydo and, when `with_skill`, skill/hydo-onboard
/// and hydo/onboard/MANIFEST.md (else the built-in copies are used).
fn release(with_skill: bool) -> PathBuf {
    let r = scratch(if with_skill { "rel" } else { "bare" });
    std::fs::create_dir_all(r.join("bin")).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_hydo"), r.join("bin/hydo")).unwrap();
    if with_skill {
        std::fs::create_dir_all(r.join("skill")).unwrap();
        std::os::unix::fs::symlink(
            repo().join("skills/hydo-onboard"),
            r.join("skill/hydo-onboard"),
        )
        .unwrap();
        std::os::unix::fs::symlink(repo().join("hydo"), r.join("hydo")).unwrap();
    }
    r
}

fn nothing(_: &Path) {}

fn git_repo(d: &Path) {
    let ok = Command::new("git")
        .args(["init", "-q"])
        .current_dir(d)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "git init");
}

fn existing_project(d: &Path) {
    std::fs::write(d.join("AGENTS.md"), "# Mine\n\nkeep this\n").unwrap();
    std::fs::write(d.join(".gitignore"), "target\n/.hydo\n").unwrap();
    std::fs::write(d.join("CLAUDE.md"), "notes\n").unwrap();
    std::fs::write(d.join("compose.yaml"), "services: {}\n").unwrap();
    std::fs::create_dir_all(d.join("hydo/workload")).unwrap();
    std::fs::write(d.join("hydo/workload/mine"), "#!/bin/sh\n").unwrap();
    std::fs::write(d.join("hydo/hydo.yaml"), "name: mine\n").unwrap();
}

fn markers_one(d: &Path) {
    std::fs::write(d.join("AGENTS.md"), "x\n<!-- hydo:end -->\n").unwrap();
}

fn markers_both(d: &Path) {
    std::fs::write(
        d.join("AGENTS.md"),
        "top\n<!-- hydo:begin old -->\nold text\n<!-- hydo:end -->  \nbottom",
    )
    .unwrap();
}

fn foreign_skill(d: &Path) {
    std::fs::create_dir_all(d.join("out/hydo-onboard")).unwrap();
    std::fs::write(
        d.join("out/hydo-onboard/SKILL.md"),
        "---\nname: other\n---\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.join(".claude/skills")).unwrap();
    std::os::unix::fs::symlink("/nonexistent", d.join(".claude/skills/hydo-onboard")).unwrap();
}

/// (setup, commands): each list runs in one directory, in order.
#[allow(clippy::type_complexity)]
fn cases(topics: &[String]) -> Vec<(fn(&Path), Vec<Vec<String>>)> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut guide = vec![
        s(&["guide"]),
        s(&["guide", "--help"]),
        s(&["guide", "-h"]),
        s(&["guide", "manifest"]),
        s(&["guide", "nope"]),
        s(&["guide", "../../etc/passwd"]),
        s(&["guide", "/etc/passwd"]),
        s(&["guide", "references"]),
        s(&["guide", "assets/example-miniflux.yaml"]),
        s(&["guide", "assets/hydo-init/hydo/hydo.yaml"]),
        s(&["guide", "references/../SKILL.md"]),
        s(&["guide", "composer.md"]),
        s(&["guide", "SKILL.md"]),
    ];
    for t in topics {
        guide.push(s(&["guide", t]));
    }
    let mut v: Vec<(fn(&Path), Vec<Vec<String>>)> = vec![(nothing, guide)];
    v.push((
        nothing,
        vec![
            s(&["skill"]),
            s(&["skill", "--help"]),
            s(&["skill", "agents"]),
            s(&["skill", "bogus"]),
            s(&["skill", "install"]),
            s(&["skill", "install", "--agent"]),
            s(&["skill", "install", "--agent", "nope,claude"]),
            s(&["skill", "install", "--to", "x", "--user"]),
            s(&["skill", "install", "--to", "out"]),
            s(&["skill", "install", "--to", "out"]),
            s(&["skill", "install", "--to", "~/elsewhere"]),
            s(&["skill", "install", "--bogus"]),
        ],
    ));
    for a in [
        "claude",
        "codex",
        "cursor",
        "copilot",
        "gemini",
        "all",
        "claude,codex",
    ] {
        v.push((
            git_repo,
            vec![
                s(&["skill", "install", "--agent", a]),
                s(&["skill", "install", &format!("--agent={a}")]),
                s(&["skill", "install", "--agent", a, "--user"]),
            ],
        ));
        v.push((nothing, vec![s(&["skill", "install", "--agent", a])]));
    }
    v.push((
        git_repo,
        vec![s(&[
            "skill", "install", "--agent", "codex", "--agent", "claude", "--to", "t",
        ])],
    ));
    v.push((
        foreign_skill,
        vec![
            s(&["skill", "install", "--to", "out"]),
            s(&["skill", "install", "--agent", "claude"]),
            s(&["skill", "install", "--to", "out", "--force"]),
        ],
    ));
    v.push((
        nothing,
        vec![
            s(&["init"]),
            s(&["init"]),
            s(&["init", "--help"]),
            s(&["init", "--dir"]),
            s(&["init", "--dir", "nowhere"]),
            s(&["init", "--name", "Bad Name"]),
            s(&["init", "--name", ""]),
        ],
    ));
    v.push((
        nothing,
        vec![
            s(&["init", "--dir", "home", "--name", "demo", "--no-gitignore"]),
            s(&["init", "--dir", "home", "--name", "demo", "--no-agents-md"]),
            s(&["init", "--dir", "home", "--name", "demo"]),
        ],
    ));
    v.push((existing_project, vec![s(&["init"]), s(&["init"])]));
    v.push((markers_one, vec![s(&["init"])]));
    v.push((markers_both, vec![s(&["init"]), s(&["init"])]));
    v
}

/// assert_eq with a short report: the differing streams and file names only.
fn same(a: &Outcome, b: &Outcome, what: &str) {
    if a == b {
        return;
    }
    let mut d = Vec::new();
    if a.rc != b.rc {
        d.push(format!("rc {:?} != {:?}", a.rc, b.rc));
    }
    if a.stdout != b.stdout {
        d.push(format!(
            "stdout:\n--- python\n{}\n--- rust\n{}",
            a.stdout, b.stdout
        ));
    }
    if a.stderr != b.stderr {
        d.push(format!(
            "stderr:\n--- python\n{}\n--- rust\n{}",
            a.stderr, b.stderr
        ));
    }
    for k in a.files.keys().chain(b.files.keys()) {
        let (x, y) = (a.files.get(k), b.files.get(k));
        if x != y {
            d.push(format!(
                "file {k}: python {:?} rust {:?}",
                x.map(|v| (format!("{:o}", v.0), v.1.len())),
                y.map(|v| (format!("{:o}", v.0), v.1.len()))
            ));
        }
    }
    d.dedup();
    panic!("{what}:\n{}", d.join("\n"));
}

fn topics() -> Vec<String> {
    let mut t: Vec<String> = std::fs::read_dir(repo().join("skills/hydo-onboard/references"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .map(|n| n[..n.len() - 3].to_string())
        .collect();
    t.sort();
    t
}

fn compare(rust_root: &Path, py_root: &Path, path_free_only: bool) -> usize {
    let topics = topics();
    assert!(topics.len() >= 5);
    let mut n = 0;
    for (i, (setup, cmds)) in cases(&topics).into_iter().enumerate() {
        let cmds: Vec<Vec<String>> = cmds
            .into_iter()
            .filter(|c| {
                // these name the skill folder itself
                !(path_free_only
                    && (c.iter().any(|x| x == "--path" || x == "--list")
                        || c[..] == ["skill".to_string(), "path".to_string()]))
            })
            .collect();
        let refs: Vec<Vec<&str>> = cmds
            .iter()
            .map(|c| c.iter().map(String::as_str).collect())
            .collect();
        let refs: Vec<&[&str]> = refs.iter().map(|c| &c[..]).collect();
        let cwd = scratch(&format!("case{i}"));
        let py = run_seq(false, py_root, &cwd, &refs, setup);
        let rs = run_seq(true, rust_root, &cwd, &refs, setup);
        for (k, (a, b)) in py.iter().zip(&rs).enumerate() {
            same(a, b, &format!("case {i}, command {:?}", cmds[k]));
            n += 1;
        }
        let _ = std::fs::remove_dir_all(&cwd);
    }
    n
}

#[test]
fn agents_equal_python() {
    let root = release(true);
    // the same root for both: the Python reads skill/hydo-onboard and hydo/onboard/MANIFEST.md
    let n = compare(&root, &root, false);
    eprintln!("agents: {n} commands identical to hydo_agents.py");
    // `--path`, `--list` and `skill path` name the folder; with the folder they are compared above
}

#[test]
fn built_in_skill_equals_the_folder() {
    let bare = release(false);
    let full = release(true);
    let n = compare(&bare, &full, true);
    eprintln!("agents (built-in skill): {n} commands identical to hydo_agents.py on the folder");
}

#[test]
fn path_and_list() {
    let root = release(true);
    let cwd = scratch("paths");
    let cmds: &[&[&str]] = &[
        &["guide", "--path"],
        &["guide", "--list"],
        &["skill", "path"],
    ];
    let py = run_seq(false, &root, &cwd, cmds, nothing);
    let rs = run_seq(true, &root, &cwd, cmds, nothing);
    for (a, b) in py.iter().zip(&rs) {
        same(a, b, "paths");
    }
    // the built-in skill has no folder: --path says so instead
    let bare = release(false);
    let rs = run_seq(true, &bare, &cwd, &[&["guide", "--path"]], nothing);
    assert_eq!(rs[0].rc, Some(1));
    assert!(rs[0].stderr.contains("built into this hydo binary"));
}
