//! `hydo guide | skill | init` (the hydo-agents crate) against the frozen behaviour of
//! hydo/onboard/hydo_agents.py, which it replaced (deleted in M107;
//! `git show 2a58c4b:hydo/onboard/hydo_agents.py`): the exit code, stdout, stderr and every file
//! with its mode, for every guide topic, every `--agent` value, `--user`, `--to`, and `hydo init`
//! fresh and repeated. tests/golden/agents.jsonl holds what the Python gave for each command (82,
//! plus `--path`/`--list`/`skill path`), recorded on 2026-10-30 from commit 2a58c4b with Python
//! 3.15.0, and it was the Rust's output byte for byte then.
//!
//! Each case runs in a freshly made directory with its own HOME. Paths are written as `<CWD>` (its name `<CWDNAME>`),
//! `<ROOT>` (the release-like root with bin/hydo) and `<REPO>`; an output or a file whose bytes
//! are exactly one file of the skill folder (`skills/hydo-onboard/`, or `hydo/onboard/MANIFEST.md`)
//! is recorded as `{"skill": <path>}`, so most of the golden does not depend on the skill's text;
//! other texts up to 2,000 bytes verbatim, longer ones by SHA-256. The golden pins the skill
//! folder's hash: if a row differs and the skill changed since the recording, the failure says so.
//!
//! Two passes: the binary next to a skill folder, and the binary alone (the skill built into it),
//! which must give the same rows (`--path`, `--list` and `skill path` name the folder and are
//! checked with the folder only; without it `--path` says the skill is built in).
//!
//! `HYDO_BLESS=1` rewrites the golden from the Rust output (the Rust is the reference from M107 on;
//! bless only a deliberate change, for example an edited skill, and say so in the commit).

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/agents.jsonl")
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

fn hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
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

/// The skill's files (and MANIFEST.md) by content, and the hash of the whole folder.
struct Skill {
    by_content: BTreeMap<Vec<u8>, String>,
    hash: String,
}

fn skill() -> Skill {
    fn walk(d: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut v: Vec<_> = std::fs::read_dir(d)
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        v.sort_by_key(|e| e.file_name());
        for e in v {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == "__pycache__" || name == ".DS_Store" || name.starts_with("._") {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                let rel = p.strip_prefix(base).unwrap().display().to_string();
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    let r = repo();
    let mut files = Vec::new();
    walk(&r.join("skills/hydo-onboard"), &r, &mut files);
    walk(&r.join("hydo/guide"), &r, &mut files);
    let man = "hydo/onboard/MANIFEST.md".to_string();
    files.push((man.clone(), std::fs::read(r.join(&man)).unwrap()));
    let mut h = Sha256::new();
    let mut by_content = BTreeMap::new();
    for (rel, b) in files {
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(hex(&b).as_bytes());
        h.update([0]);
        by_content.entry(b).or_insert(rel);
    }
    Skill {
        by_content,
        hash: h.finalize().iter().map(|x| format!("{x:02x}")).collect(),
    }
}

/// One stream or file content as recorded.
fn val(b: &[u8], sk: &Skill, paths: &[(String, &str)]) -> Value {
    if let Some(rel) = sk.by_content.get(b) {
        if !b.is_empty() {
            return json!({ "skill": rel });
        }
    }
    let mut t = String::from_utf8_lossy(b).into_owned();
    for (p, ph) in paths {
        t = t.replace(p.as_str(), ph);
    }
    if t.len() <= 2000 {
        json!({ "text": t })
    } else {
        json!({ "sha256": hex(t.as_bytes()), "len": t.len() })
    }
}

/// One sequence of commands in a fresh directory `cwd` (`setup` prepares it), each as its
/// recorded row.
fn run_seq(
    root: &Path,
    cwd: &Path,
    cmds: &[Vec<String>],
    setup: fn(&Path),
    sk: &Skill,
) -> Vec<Value> {
    let _ = std::fs::remove_dir_all(cwd);
    std::fs::create_dir_all(cwd.join("home")).unwrap();
    setup(cwd);
    let paths = vec![
        (cwd.display().to_string(), "<CWD>"),
        (root.display().to_string(), "<ROOT>"),
        (repo().display().to_string(), "<REPO>"),
        (
            cwd.file_name().unwrap().to_string_lossy().into_owned(),
            "<CWDNAME>",
        ),
    ];
    let mut res = Vec::new();
    for c in cmds {
        let o = Command::new(root.join("bin/hydo"))
            .args(c)
            .current_dir(cwd)
            .env("HOME", cwd.join("home"))
            // the clock_start hydo init writes is today otherwise (Milestone 196)
            .env("HYDO_INIT_CLOCK_START", "1794096000")
            .output()
            .unwrap();
        let mut files = BTreeMap::new();
        snapshot(cwd, cwd, &mut files);
        let files: Vec<Value> = files
            .iter()
            .map(|(k, (m, b))| json!([k, format!("{m:o}"), val(b, sk, &paths)]))
            .collect();
        res.push(json!({
            "cmd": c,
            "rc": o.status.code(),
            "stdout": val(&o.stdout, sk, &paths),
            "stderr": val(&o.stderr, sk, &paths),
            "files": files,
        }));
    }
    res
}

/// A release-like root for the binary: bin/hydo and, when `with_skill`, skill/hydo-onboard and
/// hydo/onboard/MANIFEST.md (else the built-in copies are used).
///
/// The recorded modes are those of umask 022 and of a checkout made with it (M167): every
/// test starts here, so the process's umask is set to 022 and the skill folder is a copy with
/// the modes git records (directories 0755, files 0755 or 0644), whatever the machine's umask
/// and the checkout's modes (a umask of 002, common for users on Linux, gave other modes).
fn release(with_skill: bool) -> PathBuf {
    // SAFETY: umask only sets the process's file creation mask (every test sets the same).
    unsafe {
        libc::umask(0o022);
    }
    let r = scratch(if with_skill { "rel" } else { "bare" });
    std::fs::create_dir_all(r.join("bin")).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_hydo"), r.join("bin/hydo")).unwrap();
    if with_skill {
        std::fs::create_dir_all(r.join("skill")).unwrap();
        copy_as_checked_out(
            &repo().join("skills/hydo-onboard"),
            &r.join("skill/hydo-onboard"),
        );
        std::os::unix::fs::symlink(repo().join("hydo"), r.join("hydo")).unwrap();
    }
    r
}

/// Copy a tree with the modes git gives a checkout under umask 022.
fn copy_as_checked_out(src: &Path, dst: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().filter_map(Result::ok) {
        let (p, to) = (e.path(), dst.join(e.file_name()));
        if p.is_dir() {
            copy_as_checked_out(&p, &to);
        } else {
            std::fs::copy(&p, &to).unwrap();
            let x = std::fs::metadata(&p).unwrap().permissions().mode() & 0o111 != 0;
            let mode = if x { 0o755 } else { 0o644 };
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(mode)).unwrap();
        }
    }
    std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o755)).unwrap();
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

type Case = (&'static str, fn(&Path), Vec<Vec<String>>);

/// (setup name, setup, commands): each list runs in one directory, in order.
fn cases(topics: &[String]) -> Vec<Case> {
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
        // Milestone 170: the guide holds the facts, the skill the process
        s(&["guide", "skill"]),
        s(&["guide", "overview"]),
        s(&["guide", "references/composer.md"]),
        s(&["guide", "references/install.md"]),
        s(&["guide", "examples/example-redis-composed/README.md"]),
        s(&[
            "guide",
            "examples/example-redis-composed/hydo/workload/parallel_driver_set",
        ]),
        s(&["guide", "init/agents-section.md"]),
    ];
    for t in topics {
        guide.push(s(&["guide", t]));
    }
    let mut v: Vec<Case> = vec![("nothing", nothing, guide)];
    v.push((
        "nothing",
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
            s(&["skill", "update", "--offline", "--to", "out"]),
            s(&["skill", "update", "--bogus"]),
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
            "git_repo",
            git_repo,
            vec![
                s(&["skill", "install", "--agent", a]),
                s(&["skill", "install", &format!("--agent={a}")]),
                s(&["skill", "install", "--agent", a, "--user"]),
            ],
        ));
        v.push((
            "nothing",
            nothing,
            vec![s(&["skill", "install", "--agent", a])],
        ));
    }
    v.push((
        "git_repo",
        git_repo,
        vec![s(&[
            "skill", "install", "--agent", "codex", "--agent", "claude", "--to", "t",
        ])],
    ));
    v.push((
        "foreign_skill",
        foreign_skill,
        vec![
            s(&["skill", "install", "--to", "out"]),
            s(&["skill", "install", "--agent", "claude"]),
            s(&["skill", "install", "--to", "out", "--force"]),
        ],
    ));
    v.push((
        "nothing",
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
        "nothing",
        nothing,
        vec![
            s(&["init", "--dir", "home", "--name", "demo", "--no-gitignore"]),
            s(&["init", "--dir", "home", "--name", "demo", "--no-agents-md"]),
            s(&["init", "--dir", "home", "--name", "demo"]),
        ],
    ));
    v.push((
        "existing_project",
        existing_project,
        vec![s(&["init"]), s(&["init"])],
    ));
    v.push(("markers_one", markers_one, vec![s(&["init"])]));
    v.push((
        "markers_both",
        markers_both,
        vec![s(&["init"]), s(&["init"])],
    ));
    v
}

fn topics() -> Vec<String> {
    let mut t: Vec<String> = std::fs::read_dir(repo().join("hydo/guide"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md") && n != "overview.md")
        .map(|n| n[..n.len() - 3].to_string())
        .collect();
    t.sort();
    t
}

/// Every row of one pass, keyed "<case>.<command>" (`paths`: also the folder-naming commands).
fn rows(root: &Path, paths: bool, sk: &Skill) -> Vec<(String, Value)> {
    let topics = topics();
    assert!(topics.len() >= 5);
    let mut out = Vec::new();
    for (i, (sname, setup, cmds)) in cases(&topics).into_iter().enumerate() {
        let cwd = scratch(&format!("case{i}"));
        for (k, r) in run_seq(root, &cwd, &cmds, setup, sk)
            .into_iter()
            .enumerate()
        {
            let mut r = r;
            r["setup"] = json!(sname);
            out.push((format!("{i}.{k}"), r));
        }
        let _ = std::fs::remove_dir_all(&cwd);
    }
    if paths {
        let cwd = scratch("paths");
        let cmds: Vec<Vec<String>> = [
            ["guide", "--path"],
            ["guide", "--list"],
            ["skill", "path"],
            ["skill", "version"],
        ]
        .iter()
        .map(|c| c.iter().map(|x| x.to_string()).collect())
        .collect();
        for (k, r) in run_seq(root, &cwd, &cmds, nothing, sk)
            .into_iter()
            .enumerate()
        {
            let mut r = r;
            r["setup"] = json!("nothing");
            out.push((format!("paths.{k}"), r));
        }
        let _ = std::fs::remove_dir_all(&cwd);
    }
    out
}

fn recorded() -> (String, BTreeMap<String, Value>) {
    let text = std::fs::read_to_string(golden()).expect("tests/golden/agents.jsonl");
    let mut hash = String::new();
    let mut m = BTreeMap::new();
    for l in text.lines() {
        if let Some(h) = l.strip_prefix("# skill sha256: ") {
            hash = h.to_string();
        } else if !l.starts_with('#') && !l.is_empty() {
            let v: Value = serde_json::from_str(l).unwrap();
            m.insert(v["id"].as_str().unwrap().to_string(), v);
        }
    }
    (hash, m)
}

fn compare(got: &[(String, Value)], what: &str, sk: &Skill) -> usize {
    let (hash, want) = recorded();
    let mut bad = Vec::new();
    for (id, r) in got {
        let mut r = r.clone();
        r["id"] = json!(id);
        match want.get(id) {
            Some(w) if *w == r => {}
            w => bad.push(format!(
                "{id} {}:\n  recorded: {}\n  rust:     {r}",
                r["cmd"],
                w.map(|x| x.to_string()).unwrap_or_else(|| "(none)".into())
            )),
        }
    }
    if !bad.is_empty() {
        let hint = if hash != sk.hash {
            format!(
                "\nthe skill folder changed since the recording (sha256 {hash} -> {}); if the \
                 change is deliberate, re-record with HYDO_BLESS=1 and review the diff",
                sk.hash
            )
        } else {
            String::new()
        };
        panic!(
            "{what}: {} of {} commands differ from hydo_agents.py's recorded behaviour:\n{}{hint}",
            bad.len(),
            got.len(),
            bad.join("\n")
        );
    }
    got.len()
}

fn bless(got: &[(String, Value)], sk: &Skill) {
    let mut s = String::from(
        "# hydo guide | skill | init as hydo/onboard/hydo_agents.py answered them (recorded on 2026-10-30, M107,\n\
         # from commit 2a58c4b with Python 3.15.0; `git show 2a58c4b:hydo/onboard/hydo_agents.py`), one\n\
         # command per line (tests/agents_equivalence.rs explains the form). Re-recorded from the Rust with\n\
         # HYDO_BLESS=1 after a deliberate change.\n",
    );
    s.push_str(&format!("# skill sha256: {}\n", sk.hash));
    for (id, r) in got {
        let mut r = r.clone();
        r["id"] = json!(id);
        s.push_str(&r.to_string());
        s.push('\n');
    }
    std::fs::write(golden(), s).unwrap();
}

#[test]
fn agents_as_recorded() {
    let sk = skill();
    let root = release(true);
    let got = rows(&root, true, &sk);
    if std::env::var_os("HYDO_BLESS").is_some() {
        bless(&got, &sk);
        return;
    }
    let n = compare(&got, "with the skill folder", &sk);
    let (_, want) = recorded();
    assert_eq!(n, want.len(), "the golden has other commands");
    eprintln!("agents: {n} commands identical to hydo_agents.py's recorded behaviour");
}

#[test]
fn built_in_skill_as_recorded() {
    let sk = skill();
    let bare = release(false);
    let got = rows(&bare, false, &sk);
    if std::env::var_os("HYDO_BLESS").is_some() {
        return;
    }
    let n = compare(&got, "with the built-in skill", &sk);
    eprintln!("agents (built-in skill): {n} commands identical to hydo_agents.py on the folder");
    // the built-in skill has no folder: --path says so instead
    let cwd = scratch("paths-bare");
    let cmd = vec![vec!["guide".to_string(), "--path".to_string()]];
    let r = run_seq(&bare, &cwd, &cmd, nothing, &sk);
    assert_eq!(r[0]["rc"], json!(1));
    assert!(r[0]["stderr"]["text"]
        .as_str()
        .unwrap()
        .contains("built into this hydo binary"));
    let _ = std::fs::remove_dir_all(&cwd);
}

/// test_hydo_agents.py's test_new_agents_md_and_check: the `hydo/` skeleton that `hydo init`
/// writes is accepted by `hydo build --check`.
#[test]
fn init_skeleton_builds() {
    let root = release(true);
    let cwd = scratch("initcheck");
    let run = |args: &[&str]| {
        Command::new(root.join("bin/hydo"))
            .args(args)
            .current_dir(&cwd)
            .env("HOME", &cwd)
            .output()
            .unwrap()
    };
    let o = run(&["init"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let agents = std::fs::read_to_string(cwd.join("AGENTS.md")).unwrap();
    assert!(agents.starts_with("# Instructions for coding agents\n"));
    // Milestone 196: the guest's clock starts today (00:00 UTC), not at the format's 2026-01-01
    let m = std::fs::read_to_string(cwd.join("hydo/hydo.yaml")).unwrap();
    let today = hydo_manifest::new_clock_start(None);
    assert!(
        m.contains(&format!(
            "  clock_start: {today}         # {} 00:00 UTC",
            hydo_manifest::utc_date(today)
        )),
        "{m}"
    );
    let o = run(&["build", "hydo/hydo.yaml", "--check"]);
    assert!(
        o.status.success(),
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let _ = std::fs::remove_dir_all(&cwd);
}
