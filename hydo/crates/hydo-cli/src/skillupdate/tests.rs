//! `hydo skill update` against fake skill releases on a local HTTP server (no real network):
//! the newest release found and installed, an up-to-date copy left alone, `--check`, a wrong
//! sha256 refused, the offline refusal, a newer local copy kept, which folder each agent gets,
//! and nothing written outside the skill folder.

use super::*;
use crate::fetch::sha256_bytes;
use hydo_agents::SKILL_IN_PLUGIN;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct State {
    files: HashMap<String, Vec<u8>>,
    log: Vec<String>,
}

struct Server {
    port: u16,
    state: Arc<Mutex<State>>,
}

impl Server {
    fn start() -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let state: Arc<Mutex<State>> = Arc::default();
        let st = state.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let st = st.clone();
                std::thread::spawn(move || serve(c, st));
            }
        });
        Server { port, state }
    }

    fn url(&self, p: &str) -> String {
        format!("http://127.0.0.1:{}{p}", self.port)
    }

    fn put(&self, path: &str, body: &[u8]) {
        let mut s = self.state.lock().unwrap();
        s.files.insert(path.into(), body.to_vec());
    }

    fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    fn source(&self) -> Source {
        Source::new(&self.url("/api/releases"), &self.url("/dl"), |b| Options {
            base: b.to_string(),
            proxy: None,
            attempts: 2,
            pause: Duration::ZERO,
            verbose: false,
        })
        .unwrap()
    }
}

fn serve(c: TcpStream, st: Arc<Mutex<State>>) {
    let mut w = c.try_clone().unwrap();
    let mut r = BufReader::new(c);
    let mut first = String::new();
    if r.read_line(&mut first).is_err() {
        return;
    }
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
            break;
        }
    }
    let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
    let body = {
        let mut s = st.lock().unwrap();
        s.log.push(path.clone());
        let key = path.split('?').next().unwrap_or("").to_string();
        s.files.get(&key).cloned()
    };
    let (status, body) = match body {
        Some(b) => ("200 OK", b),
        None => ("404 Not Found", Vec::new()),
    };
    let _ = write!(
        w,
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let _ = w.write_all(&body);
}

static N: AtomicU32 = AtomicU32::new(0);

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hydo-skill-update-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// A skill folder of `version` at `s` (a SKILL.md and one reference).
fn skill_at(s: &Path, version: &str) {
    std::fs::create_dir_all(s.join("references")).unwrap();
    std::fs::write(
        s.join("SKILL.md"),
        format!("---\nname: {SKILL}\ndescription: d\nmetadata:\n  version: \"{version}\"\n  hydo-min: \"0.1.0\"\n---\n# v{version}\n"),
    )
    .unwrap();
    std::fs::write(s.join("references/x.md"), format!("x {version}\n")).unwrap();
}

/// A skill folder of `version` in `dir/onboard`.
fn skill(dir: &Path, version: &str) -> PathBuf {
    let s = dir.join(SKILL);
    skill_at(&s, version);
    s
}

/// A plugin folder of `version` in `dir/hydo` (its manifest and the skill folder).
fn plugin(dir: &Path, version: &str) -> PathBuf {
    let p = dir.join(PLUGIN);
    skill_at(&p.join(SKILL_IN_PLUGIN), version);
    std::fs::create_dir_all(p.join(".claude-plugin")).unwrap();
    std::fs::write(
        p.join(".claude-plugin/plugin.json"),
        "{\"name\": \"hydo\"}\n",
    )
    .unwrap();
    p
}

/// Publish skill `version` on the server (archive and SHA256SUMS), and list it among `tags`.
fn publish(srv: &Server, d: &Path, version: &str, tags: &[(&str, bool, bool)]) -> Vec<u8> {
    let src = plugin(&d.join(format!("src-{version}")), version);
    let (a, v) = hydo_agents::release::pack(&src).unwrap();
    assert_eq!(v, version);
    let name = archive_name(version);
    srv.put(&format!("/dl/skill-v{version}/{name}"), &a);
    srv.put(
        &format!("/dl/skill-v{version}/SHA256SUMS"),
        format!("{}  {name}\n", sha256_bytes(&a)).as_bytes(),
    );
    let list: Vec<serde_json::Value> = tags
        .iter()
        .map(
            |(t, draft, pre)| serde_json::json!({"tag_name": t, "draft": draft, "prerelease": pre}),
        )
        .collect();
    srv.put(
        "/api/releases",
        serde_json::to_string(&list).unwrap().as_bytes(),
    );
    a
}

fn run(argv: &[&str], src: Option<&Source>, tmp: &Path) -> (i32, String, String) {
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let rc = run_with(&argv, src, tmp, &mut o, &mut e);
    (
        rc,
        String::from_utf8_lossy(&o).into_owned(),
        String::from_utf8_lossy(&e).into_owned(),
    )
}

fn names(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn the_newest_release_is_the_highest_skill_tag() {
    let l = br#"[{"tag_name":"v0.2.0"},{"tag_name":"skill-v1.2.0"},{"tag_name":"skill-v1.10.0"},
        {"tag_name":"skill-v9.0.0","draft":true},{"tag_name":"skill-v8.0.0","prerelease":true},
        {"tag_name":"skill-vX"},{"tag_name":"skill-v1.9.9"}]"#;
    assert_eq!(newest_in(l).unwrap().as_deref(), Some("1.10.0"));
    assert_eq!(newest_in(br#"[{"tag_name":"v0.1.0"}]"#).unwrap(), None);
    assert!(newest_in(b"{}").is_err());
    assert!(newest_in(b"<html>").is_err());
}

#[test]
fn an_older_copy_is_updated_and_nothing_else_is_touched() {
    let srv = Server::start();
    let d = scratch();
    publish(
        &srv,
        &d,
        "1.1.0",
        &[
            ("skill-v1.0.0", false, false),
            ("skill-v1.1.0", false, false),
            ("v0.1.0", false, false),
        ],
    );
    let skills = d.join("proj/.agents/skills");
    skill(&skills, "1.0.0");
    std::fs::write(skills.join(SKILL).join("stale.md"), "from 1.0.0\n").unwrap();
    std::fs::create_dir_all(skills.join("other-skill")).unwrap();
    std::fs::write(skills.join("other-skill/SKILL.md"), "mine\n").unwrap();
    std::fs::write(skills.join("notes.txt"), "mine\n").unwrap();
    let tmp = d.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let to = skills.display().to_string();
    let src = srv.source();

    let (rc, out, err) = run(&["--check", "--to", &to], Some(&src), &tmp);
    assert_eq!(rc, 3, "{out}{err}");
    assert!(out.contains("1.0.0, release 1.1.0 is newer"), "{out}");
    assert_eq!(version_at(&skills.join(SKILL)).as_deref(), Some("1.0.0"));

    let (rc, out, err) = run(&["--to", &to], Some(&src), &tmp);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(
        out.contains("updated") && out.contains("1.0.0 -> 1.1.0"),
        "{out}"
    );
    let t = skills.join(SKILL);
    assert_eq!(version_at(&t).as_deref(), Some("1.1.0"));
    assert_eq!(
        std::fs::read_to_string(t.join("references/x.md")).unwrap(),
        "x 1.1.0\n"
    );
    assert!(
        !t.join("stale.md").exists(),
        "the folder is the release's, not a merge"
    );
    // the parent holds what it held, and no temporary folder is left anywhere
    assert_eq!(names(&skills), vec!["notes.txt", SKILL, "other-skill"]);
    assert_eq!(
        std::fs::read_to_string(skills.join("notes.txt")).unwrap(),
        "mine\n"
    );
    assert_eq!(
        std::fs::read_to_string(skills.join("other-skill/SKILL.md")).unwrap(),
        "mine\n"
    );
    assert!(names(&tmp).is_empty(), "{:?}", names(&tmp));

    // up to date: nothing fetched but the list
    let before = srv.log().len();
    let (rc, out, _) = run(&["--to", &to], Some(&src), &tmp);
    assert_eq!(rc, 0);
    assert!(out.contains("already at onboard 1.1.0"), "{out}");
    assert_eq!(
        srv.log()[before..],
        ["/api/releases?per_page=100".to_string()]
    );
    let (rc, out, _) = run(&["--check", "--to", &to], Some(&src), &tmp);
    assert_eq!(rc, 0, "{out}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_wrong_sha256_is_refused_and_changes_nothing() {
    let srv = Server::start();
    let d = scratch();
    publish(&srv, &d, "1.1.0", &[("skill-v1.1.0", false, false)]);
    let name = archive_name("1.1.0");
    srv.put(
        "/dl/skill-v1.1.0/SHA256SUMS",
        format!("{}  {name}\n", "0".repeat(64)).as_bytes(),
    );
    let skills = d.join("skills");
    skill(&skills, "1.0.0");
    let tmp = d.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let (rc, out, err) = run(
        &["--to", &skills.display().to_string()],
        Some(&srv.source()),
        &tmp,
    );
    assert_eq!(rc, 1, "{out}{err}");
    assert!(err.contains("sha256") && err.contains("refused"), "{err}");
    assert_eq!(version_at(&skills.join(SKILL)).as_deref(), Some("1.0.0"));
    assert_eq!(names(&skills), vec![SKILL]);
    assert!(names(&tmp).is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn offline_it_refuses_without_touching_anything() {
    let srv = Server::start();
    let d = scratch();
    let skills = d.join("skills");
    skill(&skills, "1.0.0");
    let to = skills.display().to_string();
    let (rc, _, err) = run(&["--to", &to], None, &d);
    assert_eq!(rc, 1);
    assert!(err.contains("needs the network"), "{err}");
    let src = srv.source();
    let (rc, _, err) = run(&["--offline", "--check", "--to", &to], Some(&src), &d);
    assert_eq!(rc, 1);
    assert!(err.contains("needs the network"), "{err}");
    assert!(srv.log().is_empty(), "no request was made");
    assert_eq!(version_at(&skills.join(SKILL)).as_deref(), Some("1.0.0"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_newer_copy_is_kept_unless_a_version_is_asked_for() {
    let srv = Server::start();
    let d = scratch();
    publish(&srv, &d, "1.1.0", &[("skill-v1.1.0", false, false)]);
    let skills = d.join("skills");
    skill(&skills, "2.0.0");
    let to = skills.display().to_string();
    let src = srv.source();
    let (rc, out, _) = run(&["--to", &to], Some(&src), &d);
    assert_eq!(rc, 0);
    assert!(
        out.contains("newer than the newest release 1.1.0; kept"),
        "{out}"
    );
    assert_eq!(version_at(&skills.join(SKILL)).as_deref(), Some("2.0.0"));
    let (rc, out, err) = run(&["--to", &to, "--version", "1.1.0"], Some(&src), &d);
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(version_at(&skills.join(SKILL)).as_deref(), Some("1.1.0"));
    // a version that was not released
    let (rc, _, err) = run(&["--to", &to, "--version", "3.0.0"], Some(&src), &d);
    assert_eq!(rc, 1);
    assert!(err.contains("HTTP 404"), "{err}");
    assert_eq!(version_at(&skills.join(SKILL)).as_deref(), Some("1.1.0"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_release_whose_skill_has_another_version_is_refused() {
    let srv = Server::start();
    let d = scratch();
    let a = publish(&srv, &d, "1.1.0", &[("skill-v1.2.0", false, false)]);
    // the archive of 1.1.0 published as 1.2.0
    let name = archive_name("1.2.0");
    srv.put(&format!("/dl/skill-v1.2.0/{name}"), &a);
    srv.put(
        "/dl/skill-v1.2.0/SHA256SUMS",
        format!("{}  {name}\n", sha256_bytes(&a)).as_bytes(),
    );
    let skills = d.join("skills");
    skill(&skills, "1.0.0");
    let (rc, _, err) = run(
        &["--to", &skills.display().to_string()],
        Some(&srv.source()),
        &d,
    );
    assert_eq!(rc, 1);
    assert!(err.contains("not 1.2.0"), "{err}");
    assert_eq!(version_at(&skills.join(SKILL)).as_deref(), Some("1.0.0"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn foreign_folders_links_and_plain_http_are_refused() {
    let srv = Server::start();
    let d = scratch();
    publish(&srv, &d, "1.1.0", &[("skill-v1.1.0", false, false)]);
    let src = srv.source();
    // a folder of the same name that is another skill
    let other = d.join("a");
    std::fs::create_dir_all(other.join(SKILL)).unwrap();
    std::fs::write(
        other.join(SKILL).join("SKILL.md"),
        "---\nname: other\n---\n",
    )
    .unwrap();
    let (rc, _, err) = run(&["--to", &other.display().to_string()], Some(&src), &d);
    assert_eq!(rc, 1);
    assert!(err.contains("not hydo's onboard skill"), "{err}");
    // a symbolic link
    let link = d.join("b");
    std::fs::create_dir_all(&link).unwrap();
    std::os::unix::fs::symlink(d.join("nowhere"), link.join(SKILL)).unwrap();
    let (rc, _, err) = run(&["--to", &link.display().to_string()], Some(&src), &d);
    assert_eq!(rc, 1);
    assert!(err.contains("symbolic link"), "{err}");
    assert!(srv.log().is_empty(), "refused before any request");
    // a release list or a download over plain HTTP to another host
    assert!(Source::new("http://example.org/r", &srv.url("/dl"), Options::standard).is_err());
    assert!(Source::new(&srv.url("/api"), "http://example.org/dl", Options::standard).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn each_agent_gets_its_folder() {
    // the folders `--agent` names (relative to the repository; --user: the home directory)
    let w = Where::parse(&["--agent".into(), "claude,codex,gemini".into()], "x").unwrap();
    let t = w.targets().unwrap();
    let top = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    assert_eq!(
        t,
        vec![
            top.join(".agents/skills").join(SKILL),
            top.join(".claude/skills").join(PLUGIN)
        ]
    );
    for (a, dir) in [
        ("claude", ".claude/skills"),
        ("codex", ".agents/skills"),
        ("cursor", ".agents/skills"),
        ("copilot", ".agents/skills"),
        ("gemini", ".agents/skills"),
    ] {
        let w = Where::parse(&["--agent".into(), a.into()], "x").unwrap();
        assert_eq!(
            w.targets().unwrap(),
            vec![top.join(dir).join(hydo_agents::folder_for(dir))],
            "{a}"
        );
    }
    assert!(Where::parse(&["--agent".into(), "vim".into()], "x").is_err());
    assert!(Where::parse(&["--to".into(), "x".into(), "--user".into()], "x").is_err());
    // hydo's own repository links these folders to its source: never a target of an update
    let (rc, _, err) = run(&["--agent", "claude"], None, &std::env::temp_dir());
    assert_eq!(rc, 1);
    assert!(err.contains("symbolic link"), "{err}");
}

#[test]
fn a_claude_code_copy_gets_the_plugin_folder_and_others_the_skill() {
    let d = scratch();
    let src = plugin(&d.join("src"), "1.1.0");
    // an older plugin copy in .claude/skills/hydo, an older skill copy in .agents/skills/onboard
    let claude = plugin(&d.join("proj/.claude/skills"), "1.0.0");
    let agents = skill(&d.join("proj/.agents/skills"), "1.0.0");
    assert_eq!(
        hydo_agents::replace_dir(&src, &claude, false).unwrap(),
        "updated"
    );
    assert_eq!(
        hydo_agents::replace_dir(&src, &agents, false).unwrap(),
        "updated"
    );
    assert_eq!(version_at(&claude).as_deref(), Some("1.1.0"));
    assert_eq!(version_at(&agents).as_deref(), Some("1.1.0"));
    assert!(claude.join(".claude-plugin/plugin.json").is_file());
    assert!(claude.join(SKILL_IN_PLUGIN).join("SKILL.md").is_file());
    assert!(agents.join("SKILL.md").is_file() && !agents.join(".claude-plugin").exists());
    // a folder named hydo that is not the plugin is kept
    let other = d.join("other/hydo");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("README"), "mine\n").unwrap();
    assert!(hydo_agents::replace_dir(&src, &other, false).is_err());
    let _ = std::fs::remove_dir_all(&d);
}
