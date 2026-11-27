//! `hydo update` (Milestone 315) end to end, without the network or a real install: a local HTTP
//! server serves a fake release list, a fake hydo 0.9.0 archive (its `bin/hydo` and `install.sh`
//! are shell scripts that record their arguments) with its SHA256SUMS, and a fake skill release
//! 2.9.0. Fake `brew`, `cargo` and `claude` programs record their arguments too. Each case is a
//! scratch folder with its own HOME, PATH and temporary directory; the hydo under test is this
//! tree's program, run where it is (a checkout build) or copied into the layout of an install
//! (a Homebrew keg, cargo's bin folder, an install.sh tree, a version of the install cache).
//!
//! The cases: each install kind, a wrong checksum (refused, nothing changed), offline and an
//! unreachable release list, `--check`'s exit status, the plan without `--yes`, an install that
//! is current, the skill copies (two Claude Code configuration folders and CLAUDE_CONFIG_DIR, an
//! `.agents/skills/onboard` copy, a `--to` copy in another folder and a stale registry line) and
//! the marketplace installs, and `hydo doctor`'s daily line.

use crate::tmpdir::TmpDir;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const NEW: &str = "0.9.0";

fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

fn own() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../VERSION");
    std::fs::read_to_string(p).unwrap().trim().to_string()
}

// ---------------------------------------------------------------- the release server

#[derive(Default)]
struct State {
    files: HashMap<String, Vec<u8>>,
    /// Answers other than a file: a path's status line and headers (GitHub's limit, a
    /// redirect).
    answers: HashMap<String, (String, Vec<(String, String)>)>,
    log: Vec<String>,
    /// Each request's path and its Authorization header.
    auth: Vec<(String, Option<String>)>,
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
        self.state
            .lock()
            .unwrap()
            .files
            .insert(path.into(), body.to_vec());
    }

    fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    fn file(&self, path: &str) -> Vec<u8> {
        self.state.lock().unwrap().files[path].clone()
    }

    /// Answer `path` with a status line and headers instead of a file (None: the file again).
    fn answer(&self, path: &str, it: Option<(&str, &[(&str, &str)])>) {
        let mut s = self.state.lock().unwrap();
        match it {
            Some((status, headers)) => {
                let h = headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                s.answers.insert(path.into(), (status.into(), h));
            }
            None => {
                s.answers.remove(path);
            }
        }
    }

    /// The Authorization headers of the requests, by path.
    fn auth(&self) -> Vec<(String, Option<String>)> {
        self.state.lock().unwrap().auth.clone()
    }

    /// How many requests asked for the release list (`/api/...`).
    fn api_requests(&self) -> usize {
        self.log().iter().filter(|p| p.starts_with("/api/")).count()
    }
}

fn serve(c: TcpStream, st: Arc<Mutex<State>>) {
    let mut w = c.try_clone().unwrap();
    let mut r = BufReader::new(c);
    let mut first = String::new();
    if r.read_line(&mut first).is_err() {
        return;
    }
    let mut auth = None;
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("authorization") {
                auth = Some(v.trim().to_string());
            }
        }
    }
    let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
    let (body, answer) = {
        let mut s = st.lock().unwrap();
        s.log.push(path.clone());
        s.auth.push((path.clone(), auth));
        let key = path.split('?').next().unwrap_or("").to_string();
        (s.files.get(&key).cloned(), s.answers.get(&key).cloned())
    };
    if let Some((status, headers)) = answer {
        let body = b"{\"message\": \"API rate limit exceeded\"}";
        let mut h = String::new();
        for (k, v) in headers {
            h.push_str(&format!("{k}: {v}\r\n"));
        }
        let _ = write!(
            w,
            "HTTP/1.1 {status}\r\n{h}Connection: close\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let _ = w.write_all(body);
        return;
    }
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

fn sha256(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// A tar.gz of `files` (path, mode, text).
fn targz(files: &[(String, u32, String)]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (p, mode, text) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(text.len() as u64);
        h.set_mode(*mode);
        h.set_cksum();
        b.append_data(&mut h, p, text.as_bytes()).unwrap();
    }
    let tar = b.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

/// A shell script that appends `<tag> <its arguments>` to `log`, then runs `more`.
fn script(log: &Path, tag: &str, more: &str) -> String {
    format!(
        "#!/bin/sh\necho \"{tag} $*\" >> '{}'\n{more}\n",
        log.display()
    )
}

/// The fake new hydo: it records its arguments and answers --version.
fn new_hydo(log: &Path) -> String {
    script(
        log,
        "new-hydo",
        &format!("[ \"$1\" = --version ] && echo 'hydo {NEW}'\nexit 0"),
    )
}

/// A skill plugin folder of `version` at `p`.
fn plugin_at(p: &Path, version: &str) {
    let s = p.join("skills/onboard");
    std::fs::create_dir_all(&s).unwrap();
    std::fs::write(
        s.join("SKILL.md"),
        format!(
            "---\nname: onboard\ndescription: d\nmetadata:\n  version: \"{version}\"\n  hydo-min: \"0.1.0\"\n---\n# v{version}\n"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(p.join(".claude-plugin")).unwrap();
    std::fs::write(
        p.join(".claude-plugin/plugin.json"),
        "{\"name\": \"hydo\"}\n",
    )
    .unwrap();
}

/// A skill folder (the form of `.agents/skills/onboard` and `--to`) of `version` at `s`.
fn skill_at(s: &Path, version: &str) {
    std::fs::create_dir_all(s).unwrap();
    std::fs::write(
        s.join("SKILL.md"),
        format!(
            "---\nname: onboard\ndescription: d\nmetadata:\n  version: \"{version}\"\n  hydo-min: \"0.1.0\"\n---\n# v{version}\n"
        ),
    )
    .unwrap();
}

fn version_in(p: &Path) -> String {
    let md = if p.join("SKILL.md").is_file() {
        p.join("SKILL.md")
    } else {
        p.join("skills/onboard/SKILL.md")
    };
    let t = std::fs::read_to_string(md).unwrap();
    t.lines()
        .find_map(|l| l.trim().strip_prefix("version:"))
        .unwrap()
        .trim()
        .trim_matches('"')
        .to_string()
}

static N: AtomicU32 = AtomicU32::new(0);

/// One case: its folder (HOME is `home/`), its fake programs (`bin/`), the log the fakes write,
/// and the server with the fake releases.
struct Case {
    top: TmpDir,
    srv: Server,
}

impl Case {
    /// A case whose server lists hydo `newest`, with its skill, (and an old 0.1.0) and the old
    /// skill release `skill-v2.9.0`.
    fn new(tag: &str, newest: &str) -> Case {
        let top = TmpDir::fresh(std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hydo-update-{}-{}-{tag}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        )));
        for d in ["home", "bin", "tmp", "host", "repo"] {
            std::fs::create_dir_all(top.join(d)).unwrap();
        }
        let c = Case {
            top,
            srv: Server::start(),
        };
        c.publish(newest, true);
        c
    }

    fn home(&self) -> PathBuf {
        self.top.join("home")
    }

    fn log_path(&self) -> PathBuf {
        self.top.join("log")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    /// The release list, hydo `newest`'s archive, its skill archive (Milestone 332: the skill
    /// has hydo's version and ships in hydo's release) and SHA256SUMS (`good`: with the hydo
    /// archive's sha256). The list also names an old `skill-v` release, which nothing may read.
    fn publish(&self, newest: &str, good: bool) {
        let plat = platform();
        let top = format!("hydo-{plat}-{newest}");
        let archive = targz(&[
            (format!("{top}/VERSION"), 0o644, format!("{newest}\n")),
            (format!("{top}/bin/hydo"), 0o755, new_hydo(&self.log_path())),
            (
                format!("{top}/install.sh"),
                0o755,
                script(&self.log_path(), "install.sh", "exit 0"),
            ),
        ]);
        let name = format!("hydo-{plat}-{newest}.tar.gz");
        let sha = if good {
            sha256(&archive)
        } else {
            sha256(b"something else")
        };
        self.srv.put(&format!("/dl/v{newest}/{name}"), &archive);
        // the release has more assets; only this platform's archive may be fetched
        self.srv
            .put(&format!("/dl/v{newest}/{name}-sources.tar.gz"), b"big");
        // the skill archive of the same release
        let src = self.top.join(format!("skill-src-{newest}/hydo"));
        plugin_at(&src, newest);
        let (a, v) = hydo_agents::release::pack(&src).unwrap();
        assert_eq!(v, newest);
        let sname = format!("hydo-skill-{newest}.tar.gz");
        self.srv.put(&format!("/dl/v{newest}/{sname}"), &a);
        self.srv.put(
            &format!("/dl/v{newest}/SHA256SUMS"),
            format!(
                "{sha}  {name}\n{}  hydo-other-{newest}.tar.gz\n{}  {sname}\n",
                sha256(b"x"),
                sha256(&a)
            )
            .as_bytes(),
        );
        let list = serde_json::json!([
            {"tag_name": format!("v{newest}"), "body": "# hydo\n\n## Before you upgrade\n\n- Build your images again: they get new fingerprints.\n\n## New\n\n- things\n"},
            {"tag_name": "skill-v2.9.0"},
            {"tag_name": "v0.1.0", "body": "## Before you upgrade\n\n- old notes\n"},
            {"tag_name": "v99.0.0", "draft": true},
        ]);
        self.srv.put("/api/releases", list.to_string().as_bytes());
    }

    /// A fake program `name` in `dir` that records its arguments and runs `more`.
    fn fake(&self, dir: &Path, name: &str, more: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, script(&self.log_path(), name, more)).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A copy of the hydo under test at `rel` of the case (its `bin/hydo`).
    fn copy_hydo(&self, rel: &str) -> PathBuf {
        let p = self.top.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_hydo"), &p).unwrap();
        p
    }

    /// `hydo update <args>` (or another command) with the program `hydo`: exit status and
    /// stdout plus stderr.
    fn run(&self, hydo: &Path, args: &[&str], extra: &[(&str, &str)]) -> (i32, String) {
        let mut c = Command::new(hydo);
        c.args(args)
            .current_dir(self.top.join("repo"))
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.top.join("bin").display()),
            )
            .env("HOME", self.home())
            .env("TMPDIR", self.top.join("tmp"))
            .env("HYDO_CLI_HOST_ROOT", self.top.join("host"))
            .env("HYDO_RELEASE_API", self.srv.url("/api/releases"))
            .env("HYDO_RELEASE_URL", self.srv.url("/dl"))
            .env("HYDO_UPDATE_CHECK", "0");
        for (k, v) in extra {
            c.env(k, v);
        }
        let o = output_retrying_busy(&mut c);
        let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&o.stderr));
        (o.status.code().unwrap_or(-1), s)
    }

    fn fetched(&self) -> Vec<String> {
        self.srv
            .log()
            .into_iter()
            .filter(|p| p.starts_with("/dl/v"))
            .collect()
    }
}

/// A release tree as install.sh leaves it, at `rel` (its SHA256SUMS and VERSION), with a copy of
/// the hydo under test as its `bin/hydo`.
fn release_tree(c: &Case, rel: &str) -> PathBuf {
    let hydo = c.copy_hydo(&format!("{rel}/bin/hydo"));
    let root = c.top.join(rel);
    std::fs::write(root.join("SHA256SUMS"), "").unwrap();
    std::fs::write(root.join("VERSION"), format!("{}\n", own())).unwrap();
    hydo
}

// ---------------------------------------------------------------- the install kinds

#[test]
fn a_release_archive_install_runs_the_new_install_sh() {
    let c = Case::new("archive", NEW);
    let hydo = release_tree(&c, "home/.hydo");
    let (rc, out) = c.run(&hydo, &["update", "--yes"], &[]);
    assert_eq!(rc, 0, "{out}");
    let prefix = c.home().join(".hydo");
    assert!(
        out.contains(&format!(
            "installed from the release archive into {}",
            prefix.display()
        )),
        "{out}"
    );
    assert!(out.contains(&format!("hydo: {} -> {NEW}", own())), "{out}");
    assert!(
        out.contains(&format!("Before you upgrade to hydo {NEW}"))
            && out.contains("Build your images again"),
        "{out}"
    );
    assert!(
        !out.contains("old notes"),
        "only newer releases' notes: {out}"
    );
    let want = if platform() == "linux" {
        format!("install.sh --prefix {} --no-module", prefix.display())
    } else {
        format!("install.sh --prefix {}", prefix.display())
    };
    assert!(c.log().contains(&want), "{}", c.log());
    // only this platform's archive and the sums were fetched
    let name = format!("hydo-{}-{NEW}.tar.gz", platform());
    assert_eq!(
        c.fetched(),
        [
            format!("/dl/v{NEW}/SHA256SUMS"),
            format!("/dl/v{NEW}/{name}")
        ]
    );
    // the temporary folder is gone
    assert_eq!(std::fs::read_dir(c.top.join("tmp")).unwrap().count(), 0);
}

#[test]
fn a_wrong_checksum_is_refused_and_nothing_changes() {
    let c = Case::new("badsum", NEW);
    c.publish(NEW, false);
    let hydo = release_tree(&c, "home/.hydo");
    let before = std::fs::read(&hydo).unwrap();
    let (rc, out) = c.run(&hydo, &["update", "--yes"], &[]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("sha256") && out.contains("refused"), "{out}");
    assert!(out.contains("FAILED: hydo was not updated"), "{out}");
    assert!(!c.log().contains("install.sh"), "{}", c.log());
    assert_eq!(std::fs::read(&hydo).unwrap(), before);
    assert_eq!(std::fs::read_dir(c.top.join("tmp")).unwrap().count(), 0);
}

#[test]
fn an_install_cache_version_is_set_up_by_the_new_hydo() {
    let c = Case::new("cache", NEW);
    let hydo = release_tree(&c, &format!("home/.hydo/versions/{}", own()));
    let (rc, out) = c.run(&hydo, &["update", "--yes"], &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("installed into the install cache"), "{out}");
    let log = c.log();
    let name = format!("hydo-{}-{NEW}.tar.gz", platform());
    assert!(
        log.lines()
            .any(|l| l.starts_with("new-hydo setup --archive ") && l.ends_with(&name)),
        "{log}"
    );
    assert!(
        log.contains(&format!("new-hydo setup --get {NEW}")),
        "{log}"
    );
}

#[test]
fn a_homebrew_install_is_upgraded_by_brew() {
    let c = Case::new("brew", NEW);
    let hydo = c.copy_hydo(&format!("brew/Cellar/hydo/{}/libexec/bin/hydo", own()));
    let state = c.top.join("brew-updated");
    let bin = c.top.join("brew/bin");
    let new = new_hydo(&c.log_path()).replace('\'', "'\\''");
    c.fake(
        &bin,
        "brew",
        &format!(
            "case \"$1\" in\n\
             info) if [ -f '{s}' ]; then v={NEW}; else v={old}; fi; echo \"{{\\\"formulae\\\":[{{\\\"versions\\\":{{\\\"stable\\\":\\\"$v\\\"}}}}]}}\" ;;\n\
             update) touch '{s}' ;;\n\
             upgrade) printf '%s' '{new}' > '{b}/hydo.new' && chmod 755 '{b}/hydo.new' && mv '{b}/hydo.new' '{b}/hydo' ;;\n\
             esac",
            s = state.display(),
            old = own(),
            b = bin.display(),
        ),
    );
    let (rc, out) = c.run(&hydo, &["update", "--yes"], &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("installed with Homebrew"), "{out}");
    let log = c.log();
    let brew: Vec<&str> = log.lines().filter(|l| l.starts_with("brew ")).collect();
    assert_eq!(
        brew,
        [
            "brew info --json=v2 kakoc/tap/hydo",
            "brew update",
            "brew info --json=v2 kakoc/tap/hydo",
            "brew upgrade kakoc/tap/hydo"
        ],
        "{log}"
    );
    assert!(log.contains("new-hydo doctor"), "{log}");
    assert!(
        c.fetched().is_empty(),
        "Homebrew downloads the release itself"
    );
}

#[test]
fn a_cargo_install_is_installed_again_by_cargo() {
    let c = Case::new("cargo", NEW);
    let hydo = c.copy_hydo("cargo/bin/hydo");
    let root = c.top.join("cargo");
    std::fs::write(
        root.join(".crates2.json"),
        format!("{{\"installs\":{{\"hydo {} (registry+https://github.com/rust-lang/crates.io-index)\":{{}}}}}}", own()),
    )
    .unwrap();
    let new = new_hydo(&c.log_path()).replace('\'', "'\\''");
    let b = root.join("bin");
    c.fake(
        &c.top.join("bin"),
        "cargo",
        &format!(
            "printf '%s' '{new}' > '{b}/hydo.new' && chmod 755 '{b}/hydo.new' && mv '{b}/hydo.new' '{b}/hydo'",
            b = b.display()
        ),
    );
    let (rc, out) = c.run(&hydo, &["update", "--yes"], &[]);
    assert_eq!(rc, 0, "{out}");
    let log = c.log();
    assert!(
        log.contains(&format!(
            "cargo install hydo --locked --version {NEW} --root {}",
            root.display()
        )),
        "{log}"
    );
    let after: Vec<&str> = log.lines().filter(|l| l.starts_with("new-hydo")).collect();
    assert_eq!(after, ["new-hydo setup", "new-hydo doctor"], "{log}");
}

#[test]
fn a_checkout_build_is_left_alone_with_the_commands() {
    let c = Case::new("checkout", NEW);
    let (rc, out) = c.run(
        Path::new(env!("CARGO_BIN_EXE_hydo")),
        &["update", "--yes"],
        &[],
    );
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("built in the checkout"), "{out}");
    assert!(out.contains("hydo is not changed"), "{out}");
    assert!(out.contains("git pull"), "{out}");
    assert!(c.fetched().is_empty());
    assert_eq!(c.log(), "");
}

// ---------------------------------------------------------------- the plan and its exit status

#[test]
fn check_and_the_plan_change_nothing() {
    let c = Case::new("check", NEW);
    let sn = NEW.to_string();
    let hydo = release_tree(&c, "home/.hydo");
    let copy = c.home().join(".agents/skills/onboard");
    skill_at(&copy, "2.0.0");
    let (rc, out) = c.run(&hydo, &["update", "--check"], &[]);
    assert_eq!(rc, 3, "{out}");
    assert!(
        out.contains(&format!("{}: onboard 2.0.0 -> {sn}", copy.display())),
        "{out}"
    );
    // without --yes and without a terminal: the plan, nothing changed, status 3
    let (rc, out) = c.run(&hydo, &["update"], &[]);
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("Run `hydo update --yes`"), "{out}");
    assert_eq!(version_in(&copy), "2.0.0");
    assert_eq!(c.log(), "");
    assert!(c.fetched().is_empty());
}

#[test]
fn a_current_install_is_left_as_it_is() {
    let c = Case::new("current", &own());
    let sn = own();
    let hydo = release_tree(&c, "home/.hydo");
    let copy = c.home().join(".agents/skills/onboard");
    skill_at(&copy, sn.as_str());
    let (rc, out) = c.run(&hydo, &["update", "--check"], &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("the newest release (up to date)"), "{out}");
    assert!(out.contains(&format!("onboard {sn}, up to date")), "{out}");
    let (rc, out) = c.run(&hydo, &["update", "--yes"], &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("everything is up to date"), "{out}");
    assert_eq!(c.log(), "");
}

#[test]
fn offline_and_an_unreachable_list_change_nothing() {
    let c = Case::new("offline", NEW);
    let hydo = release_tree(&c, "home/.hydo");
    let (rc, out) = c.run(&hydo, &["update", "--yes"], &[("HYDO_OFFLINE", "1")]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("the network is off"), "{out}");
    // a port nothing listens on
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let api = format!("http://127.0.0.1:{dead}/api/releases");
    let (rc, out) = c.run(&hydo, &["update", "--check"], &[("HYDO_RELEASE_API", &api)]);
    assert_eq!(rc, 4, "could not check: {out}");
    assert!(out.contains("cannot read the release list"), "{out}");
    assert!(c.srv.log().is_empty());
    assert_eq!(c.log(), "");
}

// ---------------------------------------------------------------- the skill everywhere

#[test]
fn every_copy_of_the_skill_and_every_marketplace_install_is_updated() {
    let c = Case::new("skills", &own());
    let sn = own();
    let home = c.home();
    // the first Claude Code folder: settings and a skill copy, no plugin install
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/settings.json"), "{\"secret\": \"x\"}").unwrap();
    plugin_at(&home.join(".claude/skills/hydo"), "2.0.0");
    // the second: the marketplace install, user scope and project scope, beside another plugin
    let work = home.join(".claude-work");
    std::fs::create_dir_all(work.join("plugins")).unwrap();
    let proj = c.top.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let installed = |v: &str| {
        serde_json::json!({"version": 2, "plugins": {
        "someone-elses@corp": [{"scope": "user", "version": "7.7.7"}],
        "hydo@hydo": [
            {"scope": "user", "version": v},
            {"scope": "project", "version": v, "projectPath": proj.display().to_string()}
        ]}})
        .to_string()
    };
    std::fs::write(
        work.join("plugins/installed_plugins.json"),
        installed("2.0.0"),
    )
    .unwrap();
    // CLAUDE_CONFIG_DIR names a third folder outside the home
    let third = c.top.join("cfg3");
    plugin_at(&third.join("skills/hydo"), "2.3.3");
    // the tools' folder of the home, a --to copy elsewhere and a copy that is gone, registered
    let agents = home.join(".agents/skills/onboard");
    skill_at(&agents, "2.0.0");
    let to = c.top.join("elsewhere/onboard");
    skill_at(&to, "2.1.0");
    let gone = c.top.join("gone/onboard");
    let reg = home.join(".hydo/skill-copies.txt");
    std::fs::create_dir_all(reg.parent().unwrap()).unwrap();
    std::fs::write(
        &reg,
        format!("2.1.0\t{}\n2.0.0\t{}\n", to.display(), gone.display()),
    )
    .unwrap();
    // claude rewrites the folder's list as its update would
    let after = installed(sn.as_str()).replace('\'', "'\\''");
    c.fake(
        &c.top.join("bin"),
        "claude",
        &format!(
            "echo \"  CLAUDE_CONFIG_DIR=$CLAUDE_CONFIG_DIR pwd=$(pwd)\" >> '{log}'\n\
             if [ \"$2\" = update ]; then printf '%s' '{after}' > \"$CLAUDE_CONFIG_DIR/plugins/installed_plugins.json\"; fi",
            log = c.log_path().display()
        ),
    );
    let hydo = Path::new(env!("CARGO_BIN_EXE_hydo"));
    let env = [("CLAUDE_CONFIG_DIR", third.to_str().unwrap())];
    let (rc, out) = c.run(hydo, &["update", "--check"], &env);
    assert_eq!(rc, 3, "{out}");
    for (p, v) in [
        (home.join(".claude/skills/hydo"), "2.0.0"),
        (third.join("skills/hydo"), "2.3.3"),
        (agents.clone(), "2.0.0"),
        (to.clone(), "2.1.0"),
    ] {
        assert!(
            out.contains(&format!("skill {}: onboard {v} -> {sn}", p.display())),
            "{}: {out}",
            p.display()
        );
    }
    assert!(
        out.contains(&format!(
            "Claude Code {}: plugin hydo@hydo 2.0.0 -> {sn}",
            work.display()
        )),
        "{out}"
    );
    assert!(out.contains(&gone.display().to_string()), "{out}");
    // nothing else of the Claude Code folders is shown
    assert!(
        !out.contains("someone-elses") && !out.contains("secret") && !out.contains("7.7.7"),
        "{out}"
    );
    assert_eq!(c.log(), "", "--check runs nothing");

    let (rc, out) = c.run(hydo, &["update", "--yes"], &env);
    assert_eq!(rc, 0, "{out}");
    for p in [
        home.join(".claude/skills/hydo"),
        third.join("skills/hydo"),
        agents.clone(),
        to.clone(),
    ] {
        assert_eq!(version_in(&p), sn.as_str(), "{}", p.display());
    }
    let log = c.log();
    let cfg = format!("CLAUDE_CONFIG_DIR={}", work.display());
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(
        lines,
        [
            "claude plugin marketplace update hydo",
            &format!("  {cfg} pwd={}", c.top.join("repo").display()),
            "claude plugin update hydo@hydo --scope user",
            &format!("  {cfg} pwd={}", c.top.join("repo").display()),
            "claude plugin update hydo@hydo --scope project",
            &format!("  {cfg} pwd={}", proj.display()),
        ],
        "{log}"
    );
    assert!(
        out.contains(&format!(
            "Claude Code {}: plugin hydo@hydo 2.0.0 -> {sn} (scope user)",
            work.display()
        )),
        "{out}"
    );
    assert!(out.contains("Start a new session of your AI tool"), "{out}");
    // the registry: the gone copy's line dropped, every updated copy recorded with its version
    let r = std::fs::read_to_string(&reg).unwrap();
    assert!(!r.contains(&gone.display().to_string()), "{r}");
    for p in [&to, &agents] {
        assert!(r.contains(&format!("{sn}\t{}", p.display())), "{r}");
    }
    // a second run: everything is current
    let (rc, out) = c.run(hydo, &["update", "--check"], &env);
    assert_eq!(rc, 0, "{out}");
}

#[test]
fn skill_install_and_update_register_their_copies() {
    let c = Case::new("register", &own());
    let sn = own();
    let hydo = Path::new(env!("CARGO_BIN_EXE_hydo"));
    let to = c.top.join("custom");
    let (rc, out) = c.run(
        hydo,
        &["skill", "install", "--to", to.to_str().unwrap()],
        &[],
    );
    assert_eq!(rc, 0, "{out}");
    let reg = std::fs::read_to_string(c.home().join(".hydo/skill-copies.txt")).unwrap();
    assert!(
        reg.contains(&format!("\t{}\n", to.join("onboard").display())),
        "{reg}"
    );
    // a copy made before the registry: `hydo skill update` in its project registers it
    let old = c.top.join("repo/.agents/skills/onboard");
    skill_at(&old, "2.0.0");
    let (rc, out) = c.run(hydo, &["skill", "update"], &[]);
    assert_eq!(rc, 0, "{out}");
    let reg = std::fs::read_to_string(c.home().join(".hydo/skill-copies.txt")).unwrap();
    assert!(reg.contains(&format!("{sn}\t{}", old.display())), "{reg}");
}

// ---------------------------------------------------------------- hydo doctor's line

#[test]
fn the_doctor_names_a_newer_release_once_a_day() {
    let c = Case::new("doctor", NEW);
    let hydo = Path::new(env!("CARGO_BIN_EXE_hydo"));
    let on = [("HYDO_UPDATE_CHECK", "1")];
    let line = format!("note: hydo {NEW} is out (this is hydo {})", own());
    let (_, out) = c.run(hydo, &["doctor"], &on);
    assert!(out.contains(&line), "{out}");
    let (_, out) = c.run(hydo, &["doctor"], &on);
    assert!(out.contains(&line), "{out}");
    let looks = c
        .srv
        .log()
        .iter()
        .filter(|p| p.starts_with("/api/"))
        .count();
    assert_eq!(looks, 1, "one look a day");
    assert!(c.home().join(".hydo/update-check.json").is_file());
    // off, and offline: no line, no look
    let (_, out) = c.run(hydo, &["doctor"], &[("HYDO_UPDATE_CHECK", "0")]);
    assert!(!out.contains("is out"), "{out}");
    std::fs::remove_file(c.home().join(".hydo/update-check.json")).unwrap();
    let (_, out) = c.run(
        hydo,
        &["doctor"],
        &[("HYDO_UPDATE_CHECK", "1"), ("HYDO_OFFLINE", "1")],
    );
    assert!(!out.contains("is out"), "{out}");
    assert_eq!(
        c.srv
            .log()
            .iter()
            .filter(|p| p.starts_with("/api/"))
            .count(),
        1
    );
}

// ---------------------------------------------------------------- GitHub's API (Milestone 322)

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Every file under `dir` that holds `needle`.
fn files_holding(dir: &Path, needle: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if md.is_dir() {
            files_holding(&p, needle, out);
        } else if md.is_file() {
            let b = std::fs::read(&p).unwrap_or_default();
            if b.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                out.push(p);
            }
        }
    }
}

#[test]
fn at_github_s_limit_the_commands_say_so_and_change_nothing() {
    let c = Case::new("limit", NEW);
    let hydo = release_tree(&c, "home/.hydo");
    let copy = c.home().join(".agents/skills/onboard");
    skill_at(&copy, "2.0.0");
    let reset = (unix_now() + 37 * 60 + 30).to_string();
    let limit: &[(&str, &str)] = &[
        ("x-ratelimit-limit", "60"),
        ("x-ratelimit-remaining", "0"),
        ("x-ratelimit-reset", &reset),
    ];
    c.srv
        .answer("/api/releases", Some(("403 rate limit exceeded", limit)));
    let said = |out: &str| {
        assert!(
            out.contains(
                "GitHub's limit for requests without a login (60 an hour from one address) was \
                 reached"
            ),
            "{out}"
        );
        assert!(out.contains("the limit resets in 38 minutes"), "{out}");
        assert!(
            out.contains("Set GH_TOKEN to a GitHub token, or log in with `gh auth login`"),
            "{out}"
        );
        assert!(!out.contains("HTTP 403"), "{out}");
        assert_eq!(out.lines().count(), 1, "one line: {out}");
    };
    for args in [&["update", "--check"][..], &["update", "--yes"]] {
        let (rc, out) = c.run(&hydo, args, &[]);
        assert_eq!(rc, 4, "could not check: {out}");
        said(&out);
    }
    let (rc, out) = c.run(&hydo, &["skill", "update", "--check"], &[]);
    assert_eq!(rc, 4, "{out}");
    said(&out);
    assert!(out.starts_with("hydo: GitHub's limit"), "{out}");
    assert_eq!(
        c.srv.api_requests(),
        3,
        "one request per command, no retries"
    );
    // a 429 is the same, and it is not retried either
    c.srv
        .answer("/api/releases", Some(("429 Too Many Requests", limit)));
    let (rc, out) = c.run(&hydo, &["skill", "update"], &[]);
    assert_eq!(rc, 4, "{out}");
    said(&out);
    assert_eq!(c.srv.api_requests(), 4);
    // nothing changed
    assert_eq!(version_in(&copy), "2.0.0");
    assert_eq!(c.log(), "");
    assert!(c.fetched().is_empty());
    // hydo doctor says nothing about it and does not look before the limit resets, also when
    // the list is readable again
    let on = [("HYDO_UPDATE_CHECK", "1")];
    let (_, out) = c.run(&hydo, &["doctor"], &on);
    assert!(
        !out.contains("is out") && !out.contains("GitHub's limit"),
        "{out}"
    );
    c.srv.answer("/api/releases", None);
    let (_, out) = c.run(&hydo, &["doctor"], &on);
    assert!(!out.contains("is out"), "{out}");
    assert_eq!(c.srv.api_requests(), 4, "the doctor waits for the reset");
    // a command looks again, and its answer serves the doctor
    let (rc, out) = c.run(&hydo, &["update", "--check"], &[]);
    assert_eq!(rc, 3, "{out}");
    let (_, out) = c.run(&hydo, &["doctor"], &on);
    assert!(out.contains(&format!("note: hydo {NEW} is out")), "{out}");
    assert_eq!(c.srv.api_requests(), 5);
}

#[test]
fn a_token_goes_to_the_api_host_only_and_is_never_shown() {
    let c = Case::new("token", NEW);
    let sn = NEW.to_string();
    let hydo = release_tree(&c, "home/.hydo");
    let copy = c.home().join(".agents/skills/onboard");
    skill_at(&copy, "2.0.0");
    // the API on a server of its own; the downloads stay on the case's server
    let api = Server::start();
    api.put("/api/releases", &c.srv.file("/api/releases"));
    let list_url = api.url("/api/releases");
    let origin = api.url("");
    let token = "ghp_m322notarealtoken0123456789";
    let env: [(&str, &str); 3] = [
        ("HYDO_RELEASE_API", &list_url),
        ("HYDO_GITHUB_API_STANDIN", &origin),
        ("GH_TOKEN", token),
    ];
    let mut outs = Vec::new();
    let (rc, out) = c.run(&hydo, &["update", "--yes"], &env);
    assert_eq!(rc, 0, "{out}");
    outs.push(out);
    assert_eq!(version_in(&copy), sn.as_str());
    let bearer = Some(format!("Bearer {token}"));
    assert_eq!(
        api.auth(),
        [("/api/releases?per_page=100".to_string(), bearer.clone())]
    );
    assert!(!c.srv.auth().is_empty());
    assert!(
        c.srv.auth().iter().all(|(_, a)| a.is_none()),
        "no token to the download host: {:?}",
        c.srv.auth()
    );
    // a redirect to another host does not carry it
    let moved_to = c.srv.url("/api/releases");
    api.answer(
        "/api/moved",
        Some(("302 Found", &[("Location", &moved_to)])),
    );
    let moved_url = api.url("/api/moved");
    let moved = [("HYDO_RELEASE_API", moved_url.as_str()), env[1], env[2]];
    let (rc, out) = c.run(&hydo, &["update", "--check"], &moved);
    assert_eq!(rc, 3, "{out}");
    outs.push(out);
    assert_eq!(api.auth().last().unwrap().1, bearer);
    assert_eq!(
        c.srv.auth().last().unwrap(),
        &("/api/releases".to_string(), None)
    );
    // without the stand-in, the fake API is just another host: no token
    std::fs::remove_file(c.home().join(".hydo/update-check.json")).unwrap();
    let (rc, out) = c.run(&hydo, &["update", "--check"], &[env[0], env[2]]);
    assert_eq!(rc, 3, "{out}");
    outs.push(out);
    assert_eq!(api.auth().last().unwrap().1, None);
    // the token is in no output and in no file of the case
    for o in &outs {
        assert!(!o.contains(token), "{o}");
    }
    let mut found = Vec::new();
    files_holding(&c.top, token, &mut found);
    assert!(found.is_empty(), "the token was written: {found:?}");
}

#[test]
fn gh_auth_token_gives_the_token_when_no_variable_does() {
    let c = Case::new("gh", NEW);
    let hydo = release_tree(&c, "home/.hydo");
    let token = "gho_m322fromgh0123456789";
    c.fake(&c.top.join("bin"), "gh", &format!("echo {token}"));
    // macOS checks a new program at its first start, which took seconds beside the other
    // cases, longer than hydo waits for gh: start it once first
    let warm = output_retrying_busy(&mut Command::new(c.top.join("bin/gh")));
    assert!(warm.status.success());
    std::fs::remove_file(c.log_path()).unwrap();
    let origin = c.srv.url("");
    let standin = ("HYDO_GITHUB_API_STANDIN", origin.as_str());
    let (rc, out) = c.run(&hydo, &["update", "--check"], &[standin]);
    assert_eq!(rc, 3, "{out}");
    assert!(!out.contains(token), "{out}");
    assert_eq!(c.log(), "gh auth token --hostname github.com\n");
    assert_eq!(
        c.srv.auth().last().unwrap().1,
        Some(format!("Bearer {token}"))
    );
    // GH_TOKEN comes first, and gh is not asked
    std::fs::remove_file(c.home().join(".hydo/update-check.json")).unwrap();
    let (rc, out) = c.run(
        &hydo,
        &["update", "--check"],
        &[standin, ("GH_TOKEN", "ghp_fromenv")],
    );
    assert_eq!(rc, 3, "{out}");
    assert_eq!(
        c.srv.auth().last().unwrap().1.as_deref(),
        Some("Bearer ghp_fromenv")
    );
    // a request that may not have a token does not ask gh either
    std::fs::remove_file(c.home().join(".hydo/update-check.json")).unwrap();
    let (rc, out) = c.run(&hydo, &["update", "--check"], &[]);
    assert_eq!(rc, 3, "{out}");
    assert_eq!(c.srv.auth().last().unwrap().1, None);
    assert_eq!(c.log().lines().count(), 1, "{}", c.log());
    let mut found = Vec::new();
    files_holding(&c.top, token, &mut found);
    found.retain(|p| *p != c.top.join("bin/gh"));
    assert!(found.is_empty(), "the token was written: {found:?}");
}

#[test]
fn one_request_for_the_release_list_per_command() {
    let c = Case::new("requests", NEW);
    let sn = NEW.to_string();
    let hydo = release_tree(&c, "home/.hydo");
    let copy = c.home().join(".agents/skills/onboard");
    skill_at(&copy, "2.0.0");
    // hydo and the skill from one list
    let (rc, out) = c.run(&hydo, &["update", "--yes"], &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(version_in(&copy), sn.as_str());
    assert_eq!(c.srv.api_requests(), 1);
    // the next commands within minutes take the kept answer, and say so
    let (rc, out) = c.run(&hydo, &["skill", "update", "--check"], &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("hydo: the release list as read"), "{out}");
    let (rc, out) = c.run(&hydo, &["update", "--check"], &[]);
    assert_eq!(rc, 3, "{out}");
    assert!(
        out.contains("hydo update: the release list as read"),
        "{out}"
    );
    assert_eq!(c.srv.api_requests(), 1);
    // without the kept answer, one request again
    std::fs::remove_file(c.home().join(".hydo/update-check.json")).unwrap();
    let (rc, out) = c.run(&hydo, &["skill", "update"], &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.srv.api_requests(), 2);
}

/// Runs a program that a test has just written. On Linux another test's child, forked while
/// the new file was still open for writing, can hold that handle for a moment, and exec then
/// fails with ETXTBSY ("Text file busy"). Try again for up to 5 s instead of failing.
fn output_retrying_busy(c: &mut Command) -> std::process::Output {
    const ETXTBSY: i32 = 26;
    let mut tries = 0;
    loop {
        match c.output() {
            Err(e) if e.raw_os_error() == Some(ETXTBSY) && tries < 100 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            r => return r.unwrap(),
        }
    }
}
