//! The AI-facing commands of the `hydo` command (the Rust port of hydo/onboard/hydo_agents.py,
//! which it replaces with the same output, files and exit codes):
//!
//! ```text
//! hydo guide [TOPIC | --list | --path]
//! hydo skill install [--agent claude|codex|cursor|copilot|gemini|all ...] [--to DIR] [--user] [--force]
//! hydo skill agents | path
//! hydo init [--dir DIR] [--name NAME] [--no-agents-md] [--no-gitignore]
//! ```
//!
//! One text for every AI tool: the onboarding skill is an Agent Skills folder
//! (<https://agentskills.io/specification>) at skills/hydo-onboard/ in hydo's repository and at
//! skill/hydo-onboard/ in a release. `hydo guide` prints its SKILL.md, `hydo skill install` copies
//! the folder where each tool looks for skills, and `hydo init` starts a repository's hydo/ folder
//! from the skill's assets/hydo-init/ template and adds a short section to its AGENTS.md.
//!
//! How the skill is found: the folder on disk under the release root (skill/hydo-onboard, then
//! skills/hydo-onboard), exactly as the Python did; when neither exists, the copy built into this
//! binary at compile time (build.rs embeds skills/hydo-onboard/ and hydo/onboard/MANIFEST.md), so a
//! lone `hydo` binary still has the whole skill. The supported tools are Claude Code
//! (.claude/skills/) and Codex (.agents/skills/); the other `--agent` values are kept as they were.

use std::fmt;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hydo_manifest::py;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
}

pub const SKILL: &str = "hydo-onboard";

// Where each tool reads Agent Skills, from the tools' own documentation (read 2026-10-26):
//   Claude Code  .claude/skills/<name>, ~/.claude/skills/<name>   https://code.claude.com/docs/en/skills
//   Codex        .agents/skills (cwd up to the repo root), ~/.agents/skills
//                                                                 https://developers.openai.com/codex/skills
//   Cursor       .agents/skills, .cursor/skills (also .claude/skills); ~/.agents/skills, ~/.cursor/skills
//   Copilot      .github/skills, .claude/skills, .agents/skills; ~/.copilot/skills, ~/.agents/skills
//   Gemini CLI   .gemini/skills or .agents/skills; ~/.gemini/skills or ~/.agents/skills
// .agents/skills is the one directory Codex, Cursor, Copilot and Gemini CLI all read; Claude Code
// reads only .claude/skills. Only Claude Code and Codex are supported (the user, 2026-10-30); the
// other values stay as they were and are not extended.
pub const AGENTS: &[(&str, &str, &str)] = &[
    ("claude", ".claude/skills", "Claude Code"),
    ("codex", ".agents/skills", "OpenAI Codex"),
    ("cursor", ".agents/skills", "Cursor"),
    (
        "copilot",
        ".agents/skills",
        "GitHub Copilot (CLI, cloud agent, VS Code)",
    ),
    ("gemini", ".agents/skills", "Gemini CLI"),
];
const READERS: &[(&str, &str)] = &[
    (
        ".claude/skills",
        "Claude Code (Cursor and GitHub Copilot read it too)",
    ),
    (
        ".agents/skills",
        "Codex, Cursor, GitHub Copilot and Gemini CLI",
    ),
];
const GUIDE_TOPICS: &[(&str, &str)] = &[
    (
        "manifest",
        "the project format explained: hydo.yaml and the hydo/ folder (MANIFEST.md)",
    ),
    (
        "schema",
        "the project format's JSON Schema (format 1, hydo/schema/project-v1.json)",
    ),
];
pub const BEGIN: &str =
    "<!-- hydo:begin (written by `hydo init`; edits between these markers are replaced) -->";
pub const END: &str = "<!-- hydo:end -->";
const BEGIN_RE: &str = r"(?m)^<!-- hydo:begin\b.*-->[ \t]*$";
const END_RE: &str = r"(?m)^<!-- hydo:end -->[ \t]*$";

/// `__pycache__`, `.DS_Store` and AppleDouble `._*` files are never copied.
fn skipped(n: &str) -> bool {
    n == "__pycache__" || n == ".DS_Store" || n.starts_with("._")
}

/// A failure: `hydo: <message>` on stderr, exit status 1.
#[derive(Debug)]
pub struct Fail(pub String);

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Fail {
        Fail(e.to_string())
    }
}

type R<T> = Result<T, Fail>;

/// Python's text reading: UTF-8, universal newlines.
fn text_of(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    if s.contains('\r') {
        s.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        s.into_owned()
    }
}

fn read(p: &Path) -> R<String> {
    std::fs::read(p)
        .map(|b| text_of(&b))
        .map_err(|e| io_fail(e, p))
}

/// Python's OSError text: `[Errno 2] No such file or directory: '/x'`.
fn io_fail(e: std::io::Error, p: &Path) -> Fail {
    let n = e.raw_os_error().unwrap_or(0);
    let msg = e.to_string();
    let msg = msg.split(" (os error").next().unwrap_or(&msg).to_string();
    Fail(format!(
        "[Errno {n}] {msg}: {}",
        py::repr_str(&p.display().to_string())
    ))
}

// ---------------------------------------------------------------- where the skill is

/// The onboarding skill: the folder on disk, or the copy built into this binary.
#[derive(Clone, Debug)]
pub enum Skill {
    Disk(PathBuf),
    Embedded,
}

impl Skill {
    /// The skill folder: `<root>/skill/hydo-onboard` (a release) or `<root>/skills/hydo-onboard`
    /// (hydo's repository); else the built-in copy.
    pub fn find(root: &Path) -> Skill {
        for d in [
            root.join("skill").join(SKILL),
            root.join("skills").join(SKILL),
        ] {
            if d.join("SKILL.md").is_file() {
                return Skill::Disk(d);
            }
        }
        Skill::Embedded
    }

    /// What `--path` prints and messages name.
    fn shown(&self) -> String {
        match self {
            Skill::Disk(d) => d.display().to_string(),
            Skill::Embedded => "built into this hydo binary".into(),
        }
    }

    fn read(&self, rel: &str) -> R<String> {
        match self {
            Skill::Disk(d) => read(&d.join(rel)),
            Skill::Embedded => embedded::FILES
                .iter()
                .find(|(p, _, _)| *p == rel)
                .map(|(_, _, b)| text_of(b))
                .ok_or_else(|| Fail(format!("{rel}: missing from the built-in skill"))),
        }
    }

    /// `os.walk` of a directory of the skill (top-down, directories sorted): (relative
    /// directory, its directories, its files as (name, mode, bytes)).
    #[allow(clippy::type_complexity)]
    fn walk(&self, rel: &str) -> R<Vec<(String, Vec<(String, u32, Vec<u8>)>)>> {
        let mut out = Vec::new();
        self.walk_into(rel, ".", &mut out)?;
        Ok(out)
    }

    #[allow(clippy::type_complexity)]
    fn walk_into(
        &self,
        top: &str,
        rel: &str,
        out: &mut Vec<(String, Vec<(String, u32, Vec<u8>)>)>,
    ) -> R<()> {
        let here = if rel == "." {
            top.to_string()
        } else {
            format!("{top}/{rel}")
        };
        if let Skill::Disk(d) = self {
            if !d.join(&here).is_dir() {
                return Ok(()); // os.walk yields nothing for a directory it cannot list
            }
        }
        let (dirs, files) = self.list(&here)?;
        out.push((rel.to_string(), files));
        for d in dirs {
            let r = if rel == "." { d } else { format!("{rel}/{d}") };
            self.walk_into(top, &r, out)?;
        }
        Ok(())
    }

    /// The sorted subdirectories and the files (name, mode, bytes) of one directory.
    #[allow(clippy::type_complexity)]
    fn list(&self, rel: &str) -> R<(Vec<String>, Vec<(String, u32, Vec<u8>)>)> {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        match self {
            Skill::Disk(d) => {
                let p = d.join(rel);
                let mut names: Vec<String> = std::fs::read_dir(&p)
                    .map_err(|e| io_fail(e, &p))?
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                for n in names {
                    let q = p.join(&n);
                    match std::fs::metadata(&q) {
                        Ok(m) if m.is_dir() && !q.is_symlink() => dirs.push(n),
                        Ok(m) if m.is_dir() => {}
                        Ok(m) => {
                            let b = std::fs::read(&q).map_err(|e| io_fail(e, &q))?;
                            files.push((n, m.permissions().mode() & 0o7777, b));
                        }
                        Err(_) => {}
                    }
                }
            }
            Skill::Embedded => {
                let pre = format!("{rel}/");
                for (p, mode, b) in embedded::FILES {
                    let Some(rest) = p.strip_prefix(&pre) else {
                        continue;
                    };
                    match rest.split_once('/') {
                        Some((dname, _)) => {
                            if !dirs.iter().any(|x| x == dname) {
                                dirs.push(dname.to_string());
                            }
                        }
                        None => files.push((rest.to_string(), *mode, b.to_vec())),
                    }
                }
                dirs.sort();
                files.sort_by(|a, b| a.0.cmp(&b.0));
            }
        }
        Ok((dirs, files))
    }

    /// The topics: references/<name>.md and its first non-empty line.
    fn topics(&self) -> R<Vec<(String, String)>> {
        let mut t = Vec::new();
        let names: Vec<(String, Vec<u8>)> = match self {
            Skill::Disk(d) => {
                let r = d.join("references");
                if !r.is_dir() {
                    return Ok(t);
                }
                let mut names: Vec<String> = std::fs::read_dir(&r)
                    .map_err(|e| io_fail(e, &r))?
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|f| f.ends_with(".md") && !skipped(f))
                    .collect();
                names.sort();
                let mut v = Vec::new();
                for n in names {
                    let p = r.join(&n);
                    v.push((n, std::fs::read(&p).map_err(|e| io_fail(e, &p))?));
                }
                v
            }
            Skill::Embedded => embedded::FILES
                .iter()
                .filter_map(|(p, _, b)| {
                    let n = p.strip_prefix("references/")?;
                    (!n.contains('/') && n.ends_with(".md") && !skipped(n))
                        .then(|| (n.to_string(), b.to_vec()))
                })
                .collect(),
        };
        for (n, b) in names {
            let text = text_of(&b);
            let first = text
                .split_inclusive('\n')
                .find(|l| !py::strip(l).is_empty())
                .map(|l| l.trim_matches(|c| c == '#' || c == ' ' || c == '\n'))
                .unwrap_or("");
            t.push((n[..n.len() - 3].to_string(), first.to_string()));
        }
        Ok(t)
    }

    /// A file inside the skill folder named by the user (`hydo guide FILE`), if it is one.
    fn file(&self, a: &str) -> R<Option<String>> {
        match self {
            Skill::Disk(d) => {
                let Ok(p) = d.join(a).canonicalize() else {
                    return Ok(None);
                };
                let base = d.canonicalize().unwrap_or_else(|_| d.clone());
                if p.starts_with(&base) && p != base && p.is_file() {
                    return read(&p).map(Some);
                }
                Ok(None)
            }
            Skill::Embedded => {
                if a.starts_with('/') {
                    return Ok(None);
                }
                let n = py::normpath(a);
                if n == "." || n == ".." || n.starts_with("../") {
                    return Ok(None);
                }
                Ok(embedded::FILES
                    .iter()
                    .find(|(p, _, _)| *p == n)
                    .map(|(_, _, b)| text_of(b)))
            }
        }
    }

    /// Whether `dst` is this very folder.
    fn is_at(&self, dst: &Path) -> bool {
        match self {
            Skill::Disk(d) => match (dst.canonicalize(), d.canonicalize()) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            },
            Skill::Embedded => false,
        }
    }

    /// `shutil.copytree(src, dst, ignore=skipped)`.
    fn copy_to(&self, dst: &Path) -> R<()> {
        match self {
            Skill::Disk(d) => copytree(d, dst),
            Skill::Embedded => {
                std::fs::create_dir_all(dst).map_err(|e| io_fail(e, dst))?;
                for (p, mode, b) in embedded::FILES {
                    let t = dst.join(p);
                    if let Some(parent) = t.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| io_fail(e, parent))?;
                    }
                    std::fs::write(&t, b).map_err(|e| io_fail(e, &t))?;
                    std::fs::set_permissions(&t, std::fs::Permissions::from_mode(*mode))
                        .map_err(|e| io_fail(e, &t))?;
                }
                Ok(())
            }
        }
    }
}

fn copytree(src: &Path, dst: &Path) -> R<()> {
    std::fs::create_dir_all(dst).map_err(|e| io_fail(e, dst))?;
    let mut names: Vec<String> = std::fs::read_dir(src)
        .map_err(|e| io_fail(e, src))?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !skipped(n))
        .collect();
    names.sort();
    for n in names {
        let (s, d) = (src.join(&n), dst.join(&n));
        if s.is_dir() {
            copytree(&s, &d)?;
        } else {
            std::fs::copy(&s, &d).map_err(|e| io_fail(e, &s))?;
        }
    }
    if let Ok(m) = std::fs::metadata(src) {
        let _ = std::fs::set_permissions(dst, m.permissions());
    }
    Ok(())
}

/// MANIFEST.md: `<root>/hydo/onboard/MANIFEST.md`, else the built-in copy when the skill is the
/// built-in one too.
fn manifest_md(root: &Path, sk: &Skill) -> R<String> {
    let p = root.join("hydo").join("onboard").join("MANIFEST.md");
    if p.is_file() {
        return read(&p);
    }
    if matches!(sk, Skill::Embedded) {
        return Ok(text_of(embedded::MANIFEST_MD));
    }
    Err(Fail(format!(
        "MANIFEST.md is missing ({}); reinstall hydo",
        p.display()
    )))
}

// ---------------------------------------------------------------- output

/// What the commands write: stdout, and failures as `Fail`.
pub struct Out<'a> {
    w: &'a mut dyn Write,
}

impl Out<'_> {
    fn put(&mut self, s: &str) -> R<()> {
        self.w.write_all(s.as_bytes())?;
        Ok(())
    }
}

fn ljust(s: &str, n: usize) -> String {
    let c = s.chars().count();
    if c >= n {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(n - c))
    }
}

// ---------------------------------------------------------------- hydo guide

pub fn guide(argv: &[String], root: &Path, out: &mut Out) -> R<i32> {
    let sk = Skill::find(root);
    let Some(a) = argv.first() else {
        out.put(&sk.read("SKILL.md")?)?;
        return Ok(0);
    };
    let a = a.as_str();
    if a == "-h" || a == "--help" {
        out.put("usage: hydo guide [TOPIC | FILE | --list | --path]\n  no argument: the onboarding skill's SKILL.md (give it to any AI tool: \"run `hydo guide` and follow it\")\n  TOPIC: one of `hydo guide --list`;  FILE: a path inside the skill folder, e.g. assets/example-miniflux.yaml\n")?;
        return Ok(0);
    }
    if a == "--path" {
        return match &sk {
            Skill::Disk(_) => {
                out.put(&format!("{}\n", sk.shown()))?;
                Ok(0)
            }
            Skill::Embedded => Err(embedded_path()),
        };
    }
    let t = sk.topics()?;
    if a == "--list" {
        out.put(&format!(
            "{} {}\n",
            ljust("hydo guide", 29),
            "the onboarding skill (SKILL.md)"
        ))?;
        let mut all: Vec<(String, String)> = t.clone();
        for (k, v) in GUIDE_TOPICS {
            all.retain(|(x, _)| x != k);
            all.push((k.to_string(), v.to_string()));
        }
        all.sort();
        for (k, v) in all {
            out.put(&format!(
                "{} {}\n",
                ljust(&format!("hydo guide {k}"), 29),
                v
            ))?;
        }
        out.put(&format!(
            "{} {}\n",
            ljust("hydo guide <file>", 29),
            format_args!("any file of the skill folder ({})", sk.shown())
        ))?;
        return Ok(0);
    }
    if a == "manifest" {
        out.put(&manifest_md(root, &sk)?)?;
        return Ok(0);
    }
    if a == "schema" {
        out.put(&hydo_manifest::model::schema_text())?;
        return Ok(0);
    }
    let name = if a.ends_with(".md") && !a.contains('/') {
        &a[..a.len() - 3]
    } else {
        a
    };
    if t.iter().any(|(k, _)| k == name) {
        out.put(&sk.read(&format!("references/{name}.md"))?)?;
        return Ok(0);
    }
    if let Some(text) = sk.file(a)? {
        out.put(&text)?;
        return Ok(0);
    }
    Err(Fail(format!(
        "no guide topic {} (hydo guide --list)",
        py::repr_str(a)
    )))
}

fn embedded_path() -> Fail {
    Fail("the onboarding skill is built into this hydo binary (no skill folder next to it); `hydo skill install --to DIR` writes a copy".into())
}

// ---------------------------------------------------------------- hydo skill

/// The repository's top directory (`git rev-parse --show-toplevel`), else `start` itself.
fn repo_root(start: &Path) -> PathBuf {
    if let Ok(o) = std::process::Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .stderr(std::process::Stdio::null())
        .output()
    {
        let s = String::from_utf8_lossy(&o.stdout);
        if o.status.success() && !py::strip(&s).is_empty() {
            return PathBuf::from(py::strip(&s));
        }
    }
    PathBuf::from(py::abspath(&start.display().to_string()))
}

fn is_hydo_skill(d: &Path) -> bool {
    let Ok(b) = std::fs::read(d.join("SKILL.md")) else {
        return false;
    };
    let head: String = text_of(&b).chars().take(2048).collect();
    regex::Regex::new(&format!(r"(?m)^name:\s*{SKILL}\s*$"))
        .map(|r| r.is_match(&head))
        .unwrap_or(false)
}

fn copy_skill(sk: &Skill, dst: &Path, force: bool, out: &mut Out) -> R<()> {
    if dst.is_symlink() {
        let to = std::fs::read_link(dst).unwrap_or_default();
        return Err(Fail(format!(
            "{} is a symbolic link (to {}); not replacing it. Remove it or install with --to elsewhere",
            dst.display(),
            to.display()
        )));
    }
    let verb = if dst.exists() {
        if sk.is_at(dst) {
            return Err(Fail(format!("{} is the skill's own folder", dst.display())));
        }
        if !is_hydo_skill(dst) && !force {
            return Err(Fail(format!(
                "{} exists and is not the {SKILL} skill; not replacing it (use --force)",
                dst.display()
            )));
        }
        if dst.is_dir() {
            std::fs::remove_dir_all(dst).map_err(|e| io_fail(e, dst))?;
        } else {
            return Err(Fail(format!(
                "[Errno 20] Not a directory: {}",
                py::repr_str(&dst.display().to_string())
            )));
        }
        "updated"
    } else {
        "installed"
    };
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p).map_err(|e| io_fail(e, p))?;
    }
    sk.copy_to(dst)?;
    out.put(&format!("hydo: {verb} {}\n", dst.display()))
}

/// `os.path.expanduser` for `~` and `~/...`.
fn expanduser(p: &str) -> String {
    if p == "~" || p.starts_with("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let home = if home.len() > 1 {
            home.trim_end_matches('/').to_string()
        } else {
            home
        };
        if p == "~" {
            return home;
        }
        return format!("{}{}", if home == "/" { "" } else { &home }, &p[1..]);
    }
    p.to_string()
}

fn cwd() -> R<PathBuf> {
    Ok(std::env::current_dir()?)
}

pub fn skill(argv: &[String], root: &Path, out: &mut Out) -> R<i32> {
    let mut a: Vec<String> = argv.to_vec();
    if a.is_empty() || a[0] == "-h" || a[0] == "--help" {
        out.put(&format!(
            "usage: hydo skill install [--agent NAME ...] [--to DIR] [--user] [--force]\n       hydo skill agents        where each AI tool reads skills\n       hydo skill path          the skill folder of this installation\n  --agent claude|codex|cursor|copilot|gemini|all (repeatable, or comma-separated): copy the skill\n          into that tool's skill directory of this repository (--user: of your home directory)\n  --to DIR: copy it to DIR/{SKILL} instead (any other tool, or a custom location)\n  An earlier copy of the skill is replaced; another folder of the same name is kept unless --force.\n  Alternative for many tools at once: the Vercel `skills` CLI, `npx skills add <this skill folder>`\n"
        ))?;
        return Ok(if a.is_empty() { 2 } else { 0 });
    }
    let sk = Skill::find(root);
    let cmd = a.remove(0);
    if cmd == "path" {
        return match &sk {
            Skill::Disk(_) => {
                out.put(&format!("{}\n", sk.shown()))?;
                Ok(0)
            }
            Skill::Embedded => Err(embedded_path()),
        };
    }
    if cmd == "agents" {
        out.put("agent     project directory          user directory\n")?;
        for (k, d, who) in AGENTS {
            let p = format!("{d}/{SKILL}");
            out.put(&format!(
                "{} {} ~/{} {}\n",
                ljust(k, 9),
                ljust(&p, 26),
                ljust(&p, 22),
                who
            ))?;
        }
        out.put("all       both directories above\n")?;
        out.put("(Cursor and GitHub Copilot also read .claude/skills; with both installed they may list the skill twice.)\n")?;
        return Ok(0);
    }
    if cmd != "install" {
        return Err(Fail(format!(
            "unknown `hydo skill {cmd}` (hydo skill --help)"
        )));
    }
    let (mut agents, mut to, mut user, mut force) = (Vec::<String>::new(), None, false, false);
    while !a.is_empty() {
        let x = a.remove(0);
        if x == "--agent" && !a.is_empty() {
            let v = a.remove(0);
            agents.extend(v.split(',').filter(|y| !y.is_empty()).map(str::to_string));
        } else if let Some(v) = x.strip_prefix("--agent=") {
            agents.extend(v.split(',').filter(|y| !y.is_empty()).map(str::to_string));
        } else if x == "--to" && !a.is_empty() {
            to = Some(a.remove(0));
        } else if x == "--user" {
            user = true;
        } else if x == "--force" {
            force = true;
        } else {
            return Err(Fail(format!(
                "unknown argument {} (hydo skill --help)",
                py::repr_str(&x)
            )));
        }
    }
    if agents.is_empty() && to.is_none() {
        return Err(Fail("say where: --agent claude|codex|cursor|copilot|gemini|all, or --to DIR (hydo skill --help)".into()));
    }
    if agents.iter().any(|x| x == "all") {
        agents = AGENTS.iter().map(|(k, _, _)| k.to_string()).collect();
    }
    let bad: Vec<&str> = agents
        .iter()
        .filter(|x| !AGENTS.iter().any(|(k, _, _)| k == x))
        .map(String::as_str)
        .collect();
    if !bad.is_empty() {
        return Err(Fail(format!(
            "unknown agent {} (known: {}, all)",
            bad.join(", "),
            AGENTS
                .iter()
                .map(|(k, _, _)| *k)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if to.is_some() && user {
        return Err(Fail("--user and --to exclude each other".into()));
    }
    let mut targets = Vec::new();
    if let Some(t) = &to {
        targets.push(PathBuf::from(py::abspath(&expanduser(t))).join(SKILL));
    }
    let base = if user {
        PathBuf::from(expanduser("~"))
    } else {
        repo_root(&cwd()?)
    };
    let mut dirs: Vec<&str> = agents
        .iter()
        .filter_map(|x| AGENTS.iter().find(|(k, _, _)| k == x).map(|(_, d, _)| *d))
        .collect();
    dirs.sort();
    dirs.dedup();
    for d in dirs {
        targets.push(base.join(d).join(SKILL));
    }
    for t in &targets {
        copy_skill(&sk, t, force, out)?;
        let ts = t.display().to_string();
        if let Some((_, who)) = READERS
            .iter()
            .find(|(k, _)| ts.ends_with(&format!("{k}/{SKILL}")))
        {
            out.put(&format!("      read by {who}\n"))?;
        }
    }
    if !user && to.is_none() {
        out.put("hydo: commit the copied folder so that everyone working in the repository gets the skill\n")?;
    }
    Ok(0)
}

// ---------------------------------------------------------------- hydo init

/// The default project name: the directory's name, lowercase letters, digits and '-'.
pub fn project_name(d: &Path) -> String {
    let base = py::basename(&py::abspath(&d.display().to_string())).to_lowercase();
    let mut n = String::new();
    let mut in_bad = false;
    for c in base.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            n.push(c);
            in_bad = false;
        } else if !in_bad {
            n.push('-');
            in_bad = true;
        }
    }
    let n = n.trim_matches('-');
    let mut m = String::new();
    for c in n.chars() {
        if !(c == '-' && m.ends_with('-')) {
            m.push(c);
        }
    }
    if m.is_empty() {
        "app".into()
    } else if !m.starts_with(|c: char| c.is_alphabetic()) {
        format!("app-{m}")
    } else {
        m
    }
}

fn agents_block(sk: &Skill) -> R<String> {
    let text = sk.read("assets/hydo-init/agents-section.md")?;
    Ok(format!("{BEGIN}\n{}\n{END}\n", text.trim_matches('\n')))
}

/// Insert or replace the hydo section between the markers; never touch other content.
/// Returns "created", "added", "updated" or "unchanged".
pub fn update_agents_md(path: &Path, block: &str) -> R<&'static str> {
    if !path.exists() {
        std::fs::write(path, format!("# Instructions for coding agents\n\n{block}"))
            .map_err(|e| io_fail(e, path))?;
        return Ok("created");
    }
    let old = read(path)?;
    let b = regex::Regex::new(BEGIN_RE).unwrap().find(&old);
    let e = regex::Regex::new(END_RE).unwrap().find(&old);
    if let (Some(b), Some(e)) = (b, e) {
        if e.start() > b.start() {
            let mut new = format!(
                "{}{}{}",
                &old[..b.start()],
                block.trim_end_matches('\n'),
                &old[e.end()..]
            );
            if !new.ends_with('\n') {
                new.push('\n');
            }
            if new == old {
                return Ok("unchanged");
            }
            std::fs::write(path, new).map_err(|e| io_fail(e, path))?;
            return Ok("updated");
        }
    }
    if b.is_some() || e.is_some() {
        return Err(Fail(format!(
            "{} has only one of the hydo markers; fix it by hand (both or neither)",
            path.display()
        )));
    }
    let sep = if old.ends_with("\n\n") {
        ""
    } else if old.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    std::fs::write(path, format!("{old}{sep}{block}")).map_err(|e| io_fail(e, path))?;
    Ok("added")
}

/// Python's `str.splitlines()`.
fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let cs: Vec<(usize, char)> = s.char_indices().collect();
    let mut i = 0;
    while i < cs.len() {
        let (pos, c) = cs[i];
        let br = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if br {
            out.push(&s[start..pos]);
            let mut next = pos + c.len_utf8();
            if c == '\r' && i + 1 < cs.len() && cs[i + 1].1 == '\n' {
                next += 1;
                i += 1;
            }
            start = next;
        }
        i += 1;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

pub fn update_gitignore(path: &Path, lines: &[&str]) -> R<Vec<String>> {
    let old = if path.exists() {
        read(path)?
    } else {
        String::new()
    };
    let norm: Vec<String> = splitlines(&old)
        .iter()
        .map(|l| {
            py::strip(l)
                .trim_start_matches('/')
                .trim_end_matches('/')
                .to_string()
        })
        .collect();
    let add: Vec<String> = lines
        .iter()
        .filter(|l| !norm.iter().any(|n| n == l.trim_matches('/')))
        .map(|l| l.to_string())
        .collect();
    if add.is_empty() {
        return Ok(add);
    }
    let mut text = String::new();
    if !old.is_empty() && !old.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("# hydo (hydo init): the build cache and fetched images\n");
    for l in &add {
        text.push_str(l);
        text.push('\n');
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| io_fail(e, path))?;
    f.write_all(text.as_bytes()).map_err(|e| io_fail(e, path))?;
    Ok(add)
}

/// Python's `os.path.relpath(p, start)` for two absolute normalised paths.
fn relpath(p: &str, start: &str) -> String {
    let a: Vec<&str> = p.split('/').filter(|x| !x.is_empty()).collect();
    let b: Vec<&str> = start.split('/').filter(|x| !x.is_empty()).collect();
    let n = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<&str> = vec![".."; b.len() - n];
    parts.extend(&a[n..]);
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

pub fn init(argv: &[String], root: &Path, out: &mut Out) -> R<i32> {
    let mut a: Vec<String> = argv.to_vec();
    let mut d = cwd()?.display().to_string();
    let (mut name, mut do_agents, mut do_ignore) = (None::<String>, true, true);
    while !a.is_empty() {
        let x = a.remove(0);
        if x == "-h" || x == "--help" {
            out.put("usage: hydo init [--dir DIR] [--name NAME] [--no-agents-md] [--no-gitignore]\n  creates DIR/hydo/ (hydo.yaml skeleton, workload/, checks/, regressions/) where missing,\n  adds .hydo/ and hydo/images/ to .gitignore, and a hydo section to AGENTS.md (between markers).\n  Existing files are never overwritten; running it again changes nothing.\n")?;
            return Ok(0);
        }
        if x == "--dir" && !a.is_empty() {
            d = a.remove(0);
        } else if x == "--name" && !a.is_empty() {
            name = Some(a.remove(0));
        } else if x == "--no-agents-md" {
            do_agents = false;
        } else if x == "--no-gitignore" {
            do_ignore = false;
        } else {
            return Err(Fail(format!(
                "unknown argument {} (hydo init --help)",
                py::repr_str(&x)
            )));
        }
    }
    let d = py::abspath(&d);
    let dp = PathBuf::from(&d);
    if !dp.is_dir() {
        return Err(Fail(format!("no such directory: {d}")));
    }
    let name = match name {
        Some(n) if !n.is_empty() => n,
        _ => project_name(&dp),
    };
    let ok = regex::Regex::new(r"^[a-z][a-z0-9-]*$").unwrap();
    if !ok.is_match(&name) {
        return Err(Fail(format!(
            "--name: lowercase letters, digits and '-', starting with a letter (got {})",
            py::repr_str(&name)
        )));
    }
    let sk = Skill::find(root);
    let (mut made, mut kept) = (Vec::new(), Vec::new());
    for (rel, files) in sk.walk("assets/hydo-init/hydo")? {
        let dst_dir = py::normpath(&format!("{d}/hydo/{rel}"));
        let ddp = PathBuf::from(&dst_dir);
        std::fs::create_dir_all(&ddp).map_err(|e| io_fail(e, &ddp))?;
        for (f, mode, bytes) in files {
            if skipped(&f) {
                continue;
            }
            let dst = ddp.join(&f);
            let relp = relpath(&format!("{dst_dir}/{f}"), &d);
            if dst.symlink_metadata().is_ok() {
                kept.push(relp);
                continue;
            }
            if (rel == "workload" || rel == "checks") && ddp.is_dir() {
                let busy = std::fs::read_dir(&ddp)
                    .map_err(|e| io_fail(e, &ddp))?
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .any(|n| !n.starts_with('.') && !n.starts_with("helper_"));
                if busy {
                    kept.push(format!("{relp} (not added: the folder already has files)"));
                    continue;
                }
            }
            let text = text_of(&bytes).replace("@NAME@", &name);
            std::fs::write(&dst, text).map_err(|e| io_fail(e, &dst))?;
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode))
                .map_err(|e| io_fail(e, &dst))?;
            made.push(relp);
        }
    }
    for p in &made {
        out.put(&format!("hydo: created {p}\n"))?;
    }
    for p in &kept {
        out.put(&format!("hydo: kept    {p}\n"))?;
    }
    if do_ignore {
        for l in update_gitignore(&dp.join(".gitignore"), &[".hydo/", "hydo/images/"])? {
            out.put(&format!("hydo: added {l} to .gitignore\n"))?;
        }
    }
    if do_agents {
        let r = update_agents_md(&dp.join("AGENTS.md"), &agents_block(&sk)?)?;
        out.put(&format!("hydo: AGENTS.md: hydo section {r}\n"))?;
        for c in ["CLAUDE.md", ".claude/CLAUDE.md"] {
            let p = dp.join(c);
            if p.is_file() && !read(&p)?.contains("@AGENTS.md") {
                out.put(&format!("hydo: note: Claude Code reads {c} instead of AGENTS.md; add the line `@AGENTS.md` to it (https://code.claude.com/docs/en/memory#agents-md)\n"))?;
            }
        }
    }
    let compose = [
        "docker-compose.yaml",
        "docker-compose.yml",
        "compose.yaml",
        "compose.yml",
    ]
    .into_iter()
    .find(|f| dp.join(f).is_file());
    out.put(
        "hydo: next: give your AI tool this instruction: \"run `hydo guide` and follow it\"\n",
    )?;
    if let Some(c) = compose {
        out.put(&format!(
            "hydo:       this repository has {c}: hydo compose {c} --out hydo/hydo.yaml --images-dir images\n"
        ))?;
    }
    out.put("hydo:       check the folder with: hydo build hydo/hydo.yaml --check\n")?;
    Ok(0)
}

/// Entry point for the CLI (`hydo guide | skill | init`): the exit status; prints
/// `hydo: <message>` on stderr on a failure. `root` is the release root (or the repository).
pub fn run(cmd: &str, argv: &[String], root: &Path) -> i32 {
    let mut stdout = std::io::stdout().lock();
    let mut out = Out { w: &mut stdout };
    run_to(cmd, argv, root, &mut out)
}

/// [`run`] writing to any stream (tests).
pub fn run_to(cmd: &str, argv: &[String], root: &Path, out: &mut Out) -> i32 {
    let r = match cmd {
        "guide" => guide(argv, root, out),
        "skill" => skill(argv, root, out),
        "init" => init(argv, root, out),
        _ => Err(Fail(format!("unknown command {cmd}"))),
    };
    let _ = out.w.flush();
    match r {
        Ok(rc) => rc,
        Err(e) if e.0.contains("Broken pipe") => 0, // `hydo guide | head`: the reader stopped early
        Err(e) => {
            eprintln!("hydo: {e}");
            1
        }
    }
}

impl<'a> Out<'a> {
    pub fn new(w: &'a mut dyn Write) -> Out<'a> {
        Out { w }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(project_name(Path::new("/x/My Project_2")), "my-project-2");
        assert_eq!(project_name(Path::new("/x/2fast")), "app-2fast");
        assert_eq!(project_name(Path::new("/x/___")), "app");
        assert_eq!(relpath("/a/b/hydo/x", "/a/b"), "hydo/x");
    }

    #[test]
    fn embedded_has_the_skill() {
        assert!(embedded::FILES.iter().any(|(p, _, _)| *p == "SKILL.md"));
        assert!(embedded::FILES.iter().any(|(p, m, _)| *p
            == "assets/hydo-init/hydo/workload/parallel_driver_todo"
            && m & 0o111 != 0));
        assert!(!embedded::MANIFEST_MD.is_empty());
    }

    #[test]
    fn lines() {
        assert_eq!(splitlines("a\r\nb\rc\n\nd"), vec!["a", "b", "c", "", "d"]);
        assert_eq!(splitlines("a\n"), vec!["a"]);
    }
}
