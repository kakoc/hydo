//! `hydo onboard` (Milestone 229) in a scripted environment: each case is a scratch repository
//! with its own HOME and a PATH that holds only the stand-in AI tools the case wants (scripts
//! that do nothing), so what the command finds is exactly what the case set up. No network, no
//! VM: the Compose case passes `--no-resolve`.
//!
//! The cases: a Compose project with a `.claude/` folder, a plain project with the `codex`
//! command, an existing hydo/ folder, and no AI tool at all; plus `--agent`, `--no-skill` and
//! `--help`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

struct Case {
    top: PathBuf,
    repo: PathBuf,
    bin: PathBuf,
}

impl Case {
    fn new(tag: &str) -> Case {
        let top = std::env::temp_dir().join(format!(
            "hydo-onb-cmd-{}-{}-{tag}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&top);
        for d in ["repo", "bin", "home"] {
            std::fs::create_dir_all(top.join(d)).unwrap();
        }
        let top = top.canonicalize().unwrap();
        Case {
            repo: top.join("repo"),
            bin: top.join("bin"),
            top,
        }
    }

    /// A file of the repository.
    fn put(&self, rel: &str, text: &str) {
        let p = self.repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// A stand-in program on the case's PATH.
    fn tool(&self, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = self.bin.join(name);
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `hydo onboard <args>` in the repository: the exit status and stdout plus stderr.
    fn onboard(&self, args: &[&str]) -> (i32, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_hydo"))
            .arg("onboard")
            .args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", &self.bin)
            .env("HOME", self.top.join("home"))
            .env("HYDO_INIT_CLOCK_START", "1767225600")
            .output()
            .unwrap();
        let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&o.stderr));
        (o.status.code().unwrap_or(-1), s)
    }

    fn has(&self, rel: &str) -> bool {
        self.repo.join(rel).exists()
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.top);
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

#[test]
fn a_compose_project_with_claude_code() {
    let c = Case::new("compose");
    c.put(
        "docker-compose.yml",
        "services:\n  broker:\n    image: example/broker:1.0\n    ports: [\"7700:7700\"]\n  worker:\n    image: example/worker:1.0\n",
    );
    c.put("worker/main.py", "print(1)\n");
    c.put("worker/Dockerfile", "FROM scratch\n");
    c.put(".claude/settings.json", "{}\n");
    let (rc, out) = c.onboard(&["--no-resolve"]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("Compose file: docker-compose.yml, 2 services: broker, worker"),
        "{out}"
    );
    assert!(out.contains("Dockerfile: worker/Dockerfile"), "{out}");
    assert!(out.contains("main language: Python (1 file)"), "{out}");
    assert!(out.contains("hydo folder: none yet"), "{out}");
    // hydo compose wrote the services; hydo init's skeleton was not added
    let m = read(&c.repo.join("hydo/hydo.yaml"));
    assert!(
        m.contains("name: broker") && m.contains("name: worker"),
        "{m}"
    );
    assert!(c.has("hydo/fetch-images.sh"));
    assert!(!c.has("hydo/workload/parallel_driver_todo"));
    assert!(read(&c.repo.join(".gitignore")).contains("hydo-out/"));
    assert!(read(&c.repo.join("AGENTS.md")).contains("hydo:begin"));
    // Claude Code gets the plugin folder, whose command is /hydo:onboard
    assert!(c.has(".claude/skills/hydo/.claude-plugin/plugin.json"));
    assert!(c.has(".claude/skills/hydo/skills/onboard/SKILL.md"));
    assert!(!c.has(".agents"), "no other tool was found");
    assert!(
        out.contains("Claude Code (.claude in the repository)"),
        "{out}"
    );
    assert!(
        out.contains("Next: open Claude Code here and run /hydo:onboard"),
        "{out}"
    );
}

#[test]
fn the_commit_advice_comes_only_in_a_git_repository() {
    let c = Case::new("nogit");
    c.tool("codex");
    let (rc, out) = c.onboard(&[]);
    assert_eq!(rc, 0, "{out}");
    assert!(c.has(".agents/skills/onboard/SKILL.md"), "{out}");
    assert!(!out.contains("commit the copied folder"), "{out}");
    let g = Case::new("git");
    g.tool("codex");
    std::fs::create_dir_all(g.repo.join(".git")).unwrap();
    let (rc, out) = g.onboard(&[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("commit the copied folder"), "{out}");
}

#[test]
fn a_plain_project_with_codex() {
    let c = Case::new("plain");
    c.put("cmd/q/main.go", "package main\n");
    c.put("internal/q/q.go", "package q\n");
    c.put("tools/x.py", "x = 1\n");
    c.tool("codex");
    let (rc, out) = c.onboard(&[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("Compose file: none"), "{out}");
    assert!(out.contains("Dockerfile: none"), "{out}");
    assert!(
        out.contains("main language: Go (2 files; then Python 1)"),
        "{out}"
    );
    // hydo init's skeleton, without hydo init's own next step
    assert!(c.has("hydo/hydo.yaml") && c.has("hydo/workload/parallel_driver_todo"));
    assert!(!out.contains("hydo guide skill"), "{out}");
    assert!(c.has(".agents/skills/onboard/SKILL.md"));
    assert!(!c.has(".agents/skills/onboard/.claude-plugin"));
    assert!(!c.has(".claude"));
    assert!(out.contains("Codex (the codex command)"), "{out}");
    assert!(
        out.contains("Next: open Codex here and ask it to use the onboard skill"),
        "{out}"
    );
}

#[test]
fn an_existing_hydo_folder_is_left_alone() {
    let c = Case::new("existing");
    c.put(
        "compose.yaml",
        "services:\n  app:\n    image: example/app:1\n",
    );
    c.put("hydo/hydo.yaml", "# mine\n");
    c.tool("claude");
    let (rc, out) = c.onboard(&["--no-resolve"]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("hydo folder: hydo/ with hydo/hydo.yaml"),
        "{out}"
    );
    assert!(
        out.contains("hydo/ exists, so it is left as it is"),
        "{out}"
    );
    assert_eq!(read(&c.repo.join("hydo/hydo.yaml")), "# mine\n");
    assert!(!c.has("hydo/fetch-images.sh") && !c.has("hydo/workload"));
    assert!(!c.has("AGENTS.md") && !c.has(".gitignore"));
    // the skill is still installed: the AI tool continues from the existing folder
    assert!(c.has(".claude/skills/hydo/skills/onboard/SKILL.md"));
    assert!(out.contains("Claude Code (the claude command)"), "{out}");
}

#[test]
fn without_an_ai_tool_it_names_the_three_parts() {
    let c = Case::new("none");
    c.put("src/main.rs", "fn main() {}\n");
    let (rc, out) = c.onboard(&[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("no AI coding tool found"), "{out}");
    assert!(!c.has(".claude") && !c.has(".agents"));
    for want in [
        "Next: fill in three things in hydo/ (`hydo guide workflow` walks through them):",
        "1. how to start your system",
        "2. a client that uses it: commands in hydo/workload/",
        "3. the rules that must hold: checks in hydo/checks/, or a `type: messages` check",
        "hydo build hydo/hydo.yaml --check",
    ] {
        assert!(out.contains(want), "{want}: {out}");
    }
    // running it again changes nothing and says so
    let before = read(&c.repo.join("hydo/hydo.yaml"));
    let (rc, out) = c.onboard(&[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("left as it is"), "{out}");
    assert_eq!(read(&c.repo.join("hydo/hydo.yaml")), before);
}

#[test]
fn agent_and_no_skill_override_what_is_found() {
    let c = Case::new("override");
    c.tool("claude");
    let (rc, out) = c.onboard(&["--agent", "gemini"]);
    assert_eq!(rc, 0, "{out}");
    assert!(c.has(".agents/skills/onboard/SKILL.md") && !c.has(".claude"));
    assert!(out.contains("Next: open Gemini CLI here"), "{out}");

    let c = Case::new("noskill");
    c.tool("claude");
    let (rc, out) = c.onboard(&["--no-skill"]);
    assert_eq!(rc, 0, "{out}");
    assert!(!c.has(".claude") && !c.has(".agents"));
    assert!(out.contains("Next: fill in three things"), "{out}");

    let (rc, out) = c.onboard(&["--agent", "vim"]);
    assert_eq!(rc, 2);
    assert!(out.contains("unknown agent vim"), "{out}");
}

#[test]
fn help_needs_nothing() {
    let c = Case::new("help");
    let (rc, out) = c.onboard(&["--help"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.starts_with("usage: hydo onboard [DIR]"), "{out}");
    assert!(!c.has("hydo"), "--help changes nothing");
}
