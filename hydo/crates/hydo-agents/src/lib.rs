#![doc = include_str!("../README.md")]
//!
//! ## Notes for hydo's developers
//!
//! The AI-facing commands of the `hydo` command (the Rust port of hydo/onboard/hydo_agents.py,
//! which it replaces with the same output, files and exit codes):
//!
//! ```text
//! hydo guide [TOPIC | --list | --path]
//! hydo skill install [--agent claude|codex|cursor|copilot|gemini|all ...] [--to DIR] [--user] [--force]
//! hydo skill agents | path | version
//! hydo init [--dir DIR] [--name NAME] [--no-agents-md] [--no-gitignore]
//! ```
//!
//! (`hydo skill update`, which needs the network, is in hydo-cli's src/skillupdate.rs; it uses
//! [`Where`], [`replace_dir`] and [`release`] from here.)
//!
//! Since Milestone 170 the process and the facts are apart. The onboarding skill is an Agent
//! Skills folder (<https://agentskills.io/specification>) at skills/hydo-onboard/ in hydo's
//! repository and at skill/hydo-onboard/ in a release: the process only, with its own version
//! (`metadata.version`) and its own releases (`skill-v<version>`, [`release`]). The facts of this
//! hydo (the project format's keys, commands, options, fault kinds, limits, examples, `hydo
//! init`'s template) are the guide, hydo/guide/ in the repository, built into the binary and
//! printed by `hydo guide <topic>`; so they always match the hydo that prints them. `hydo guide`
//! alone prints this hydo's version, how to get the skill and the topics; `hydo guide skill`
//! prints the skill's SKILL.md (for a tool without skills). The skill's old file paths
//! (`references/<topic>.md`, `assets/...`) still answer.
//!
//! How the skill is found: the folder on disk under the release root (skill/hydo-onboard, then
//! skills/hydo-onboard), exactly as the Python did; when neither exists, the copy built into this
//! binary at compile time (build.rs embeds skills/hydo-onboard/, hydo/guide/ and
//! hydo/onboard/MANIFEST.md), so a lone `hydo` binary still has the whole skill. The guide is always
//! the built-in one. The supported tools are Claude Code (.claude/skills/, or the plugin of the
//! marketplace .claude-plugin/marketplace.json) and Codex (.agents/skills/); the other `--agent`
//! values are kept as they were.

use std::fmt;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hydo_manifest::py;

pub mod release;

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
    (
        "skill",
        "the onboarding skill's SKILL.md (the process), for a tool without skills",
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

    /// The skill's version: `metadata.version` in SKILL.md's frontmatter.
    pub fn version(&self) -> Option<String> {
        self.read("SKILL.md").ok().and_then(|t| skill_version(&t))
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

/// One directory of a built-in tree: its sorted subdirectories and its files (name, mode, bytes).
#[allow(clippy::type_complexity)]
fn list_embedded(
    tree: &[(&str, u32, &[u8])],
    rel: &str,
) -> (Vec<String>, Vec<(String, u32, Vec<u8>)>) {
    let (mut dirs, mut files) = (Vec::<String>::new(), Vec::new());
    let pre = format!("{rel}/");
    for (p, mode, b) in tree {
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
    (dirs, files)
}

/// `os.walk` of a directory of a built-in tree (top-down, directories sorted).
#[allow(clippy::type_complexity)]
fn walk_embedded(
    tree: &[(&str, u32, &[u8])],
    top: &str,
) -> Vec<(String, Vec<(String, u32, Vec<u8>)>)> {
    let mut out = Vec::new();
    let mut todo = vec![".".to_string()];
    while let Some(rel) = todo.pop() {
        let here = if rel == "." {
            top.to_string()
        } else {
            format!("{top}/{rel}")
        };
        let (dirs, files) = list_embedded(tree, &here);
        out.push((rel.clone(), files));
        for d in dirs.into_iter().rev() {
            todo.push(if rel == "." { d } else { format!("{rel}/{d}") });
        }
    }
    out
}

/// The version in a SKILL.md: `metadata:` → `version:` of its frontmatter.
pub fn skill_version(skill_md: &str) -> Option<String> {
    let body = skill_md.strip_prefix("---\n")?;
    let fm = &body[..body.find("\n---\n")?];
    let mut in_meta = false;
    for l in fm.lines() {
        if !l.starts_with(' ') {
            in_meta = l.trim_end() == "metadata:";
            continue;
        }
        if in_meta {
            if let Some(v) = l.trim().strip_prefix("version:") {
                let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
                return (!v.is_empty()).then(|| v.to_string());
            }
        }
    }
    None
}

/// Whether version `a` is older than `b` (dotted numbers; a part that is not a number counts as 0).
pub fn version_lt(a: &str, b: &str) -> bool {
    let p = |v: &str| -> Vec<u64> { v.split('.').map(|x| x.parse().unwrap_or(0)).collect() };
    let (mut x, mut y) = (p(a), p(b));
    let n = x.len().max(y.len());
    x.resize(n, 0);
    y.resize(n, 0);
    x < y
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

// ---------------------------------------------------------------- the guide (facts)

/// The guide built into this binary (hydo/guide/ of the repository): the bytes of one file.
fn guide_bytes(rel: &str) -> Option<&'static [u8]> {
    embedded::GUIDE
        .iter()
        .find(|(p, _, _)| *p == rel)
        .map(|(_, _, b)| *b)
}

/// The guide's topics: its top-level `<name>.md` files (but the overview) and each one's first
/// non-empty line.
pub fn topics() -> Vec<(String, String)> {
    let mut t: Vec<(String, String)> = embedded::GUIDE
        .iter()
        .filter_map(|(p, _, b)| {
            let n = p.strip_suffix(".md")?;
            if n.contains('/') || n == "overview" || skipped(p) {
                return None;
            }
            let text = text_of(b);
            let first = text
                .split_inclusive('\n')
                .find(|l| !py::strip(l).is_empty())
                .map(|l| l.trim_matches(|c| c == '#' || c == ' ' || c == '\n'))
                .unwrap_or("")
                .to_string();
            Some((n.to_string(), first))
        })
        .collect();
    t.sort();
    t
}

/// A path inside the guide named by the user (`hydo guide examples/x`), also in the skill's old
/// spelling from before Milestone 170 (`references/<topic>.md`, `assets/example-*`,
/// `assets/hydo-init/*`).
fn guide_path(a: &str) -> Option<&'static [u8]> {
    if a.starts_with('/') {
        return None;
    }
    let n = py::normpath(a);
    if n == "." || n == ".." || n.starts_with("../") {
        return None;
    }
    let mut cands = vec![n.clone()];
    if let Some(r) = n.strip_prefix("references/") {
        cands.push(r.to_string());
    }
    if let Some(r) = n.strip_prefix("assets/hydo-init/") {
        cands.push(format!("init/{r}"));
    } else if let Some(r) = n.strip_prefix("assets/") {
        cands.push(format!("examples/{r}"));
    }
    cands.iter().find_map(|c| guide_bytes(c))
}

/// The topic lines of `hydo guide` (indented) and `hydo guide --list`.
fn topic_lines(sk: &Skill, list: bool) -> String {
    let mut all: Vec<(String, String)> = topics();
    for (k, v) in GUIDE_TOPICS {
        all.retain(|(x, _)| x != k);
        all.push((k.to_string(), v.to_string()));
    }
    all.sort();
    let pad = if list { "" } else { "  " };
    let mut s = String::new();
    let mut line = |k: &str, v: &str| {
        s.push_str(&format!("{pad}{} {v}\n", ljust(k, 29)));
    };
    if list {
        line(
            "hydo guide",
            "how to get the onboarding skill, and the topics",
        );
    }
    for (k, v) in &all {
        line(&format!("hydo guide {k}"), v);
    }
    line(
        "hydo guide examples/<file>",
        "a file of the examples (start with examples/example-redis-composed/README.md)",
    );
    if list {
        line(
            "hydo guide <file>",
            &format!("any file of the skill folder ({})", sk.shown()),
        );
    }
    s
}

/// What `hydo guide` prints: this hydo's version, how to get the skill, and the topics.
fn overview(sk: &Skill) -> String {
    let t = guide_bytes("overview.md").map(text_of).unwrap_or_default();
    t.replace("@VERSION@", env!("CARGO_PKG_VERSION"))
        .replace("@TOPICS@\n", &topic_lines(sk, false))
}

// ---------------------------------------------------------------- hydo guide

pub fn guide(argv: &[String], root: &Path, out: &mut Out) -> R<i32> {
    let sk = Skill::find(root);
    let Some(a) = argv.first() else {
        out.put(&overview(&sk))?;
        return Ok(0);
    };
    let a = a.as_str();
    if a == "-h" || a == "--help" {
        out.put("usage: hydo guide [TOPIC | FILE | --list | --path]\n  no argument: how to get the onboarding skill, and the topics of this guide\n  TOPIC: one of `hydo guide --list` (this hydo's facts); `skill`: the onboarding skill's SKILL.md\n  FILE: a file of the examples (examples/...) or of the skill folder (references/...)\n")?;
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
    if a == "--list" {
        out.put(&topic_lines(&sk, true))?;
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
    if a == "skill" {
        out.put(&sk.read("SKILL.md")?)?;
        return Ok(0);
    }
    let name = if a.ends_with(".md") && !a.contains('/') {
        &a[..a.len() - 3]
    } else {
        a
    };
    if name == "overview" {
        out.put(&overview(&sk))?;
        return Ok(0);
    }
    if topics().iter().any(|(k, _)| k == name) {
        if let Some(b) = guide_bytes(&format!("{name}.md")) {
            out.put(&text_of(b))?;
            return Ok(0);
        }
    }
    if let Some(b) = guide_path(a) {
        out.put(&text_of(b))?;
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
    let v = sk.version().unwrap_or_else(|| "unknown".into());
    out.put(&format!("hydo: {verb} {} ({SKILL} {v})\n", dst.display()))
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

/// Where `hydo skill install` and `hydo skill update` put the skill.
#[derive(Debug, Default)]
pub struct Where {
    /// `--agent` values (`all` expanded).
    pub agents: Vec<String>,
    /// `--to DIR`.
    pub to: Option<String>,
    /// `--user`: the tools' directories in the home directory instead of the repository's.
    pub user: bool,
    /// `--force`: replace a folder of the same name that is not the hydo-onboard skill.
    pub force: bool,
}

impl Where {
    /// Parse the options; anything else is an error naming `cmd`'s help.
    pub fn parse(argv: &[String], cmd: &str) -> R<Where> {
        let mut a: Vec<String> = argv.to_vec();
        let mut w = Where::default();
        while !a.is_empty() {
            let x = a.remove(0);
            if x == "--agent" && !a.is_empty() {
                let v = a.remove(0);
                w.agents
                    .extend(v.split(',').filter(|y| !y.is_empty()).map(str::to_string));
            } else if let Some(v) = x.strip_prefix("--agent=") {
                w.agents
                    .extend(v.split(',').filter(|y| !y.is_empty()).map(str::to_string));
            } else if x == "--to" && !a.is_empty() {
                w.to = Some(a.remove(0));
            } else if x == "--user" {
                w.user = true;
            } else if x == "--force" {
                w.force = true;
            } else {
                return Err(Fail(format!(
                    "unknown argument {} ({cmd} --help)",
                    py::repr_str(&x)
                )));
            }
        }
        if w.agents.iter().any(|x| x == "all") {
            w.agents = AGENTS.iter().map(|(k, _, _)| k.to_string()).collect();
        }
        let bad: Vec<&str> = w
            .agents
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
        if w.to.is_some() && w.user {
            return Err(Fail("--user and --to exclude each other".into()));
        }
        Ok(w)
    }

    /// Whether a place was named (`--agent` or `--to`).
    pub fn named(&self) -> bool {
        !self.agents.is_empty() || self.to.is_some()
    }

    /// The skill folders named: `--to DIR` first, then each tool directory once.
    pub fn targets(&self) -> R<Vec<PathBuf>> {
        let mut targets = Vec::new();
        if let Some(t) = &self.to {
            targets.push(PathBuf::from(py::abspath(&expanduser(t))).join(SKILL));
        }
        let base = if self.user {
            PathBuf::from(expanduser("~"))
        } else {
            repo_root(&cwd()?)
        };
        let mut dirs: Vec<&str> = self
            .agents
            .iter()
            .filter_map(|x| AGENTS.iter().find(|(k, _, _)| k == x).map(|(_, d, _)| *d))
            .collect();
        dirs.sort();
        dirs.dedup();
        for d in dirs {
            targets.push(base.join(d).join(SKILL));
        }
        Ok(targets)
    }
}

/// The copies of the skill in the usual places: this repository's and the home directory's
/// `.claude/skills/hydo-onboard` and `.agents/skills/hydo-onboard`, where a folder of the
/// hydo-onboard skill is (not a symbolic link: hydo's own repository links them to its source).
pub fn installed_copies() -> R<Vec<PathBuf>> {
    let mut v = Vec::new();
    for base in [repo_root(&cwd()?), PathBuf::from(expanduser("~"))] {
        for (d, _) in READERS {
            let p = base.join(d).join(SKILL);
            if !p.is_symlink() && p.is_dir() && is_hydo_skill(&p) && !v.contains(&p) {
                v.push(p);
            }
        }
    }
    Ok(v)
}

/// Who reads a skill folder, for the line after "installed".
pub fn readers_of(t: &Path) -> Option<&'static str> {
    let ts = t.display().to_string();
    READERS
        .iter()
        .find(|(k, _)| ts.ends_with(&format!("{k}/{SKILL}")))
        .map(|(_, who)| *who)
}

/// The version of the skill folder at `d`, if it is one.
pub fn version_at(d: &Path) -> Option<String> {
    std::fs::read(d.join("SKILL.md"))
        .ok()
        .and_then(|b| skill_version(&text_of(&b)))
}

/// Whether `dst` may be replaced by the skill: it does not exist, or it is a folder of the
/// hydo-onboard skill, or `force`. A symbolic link is never replaced.
pub fn replaceable(dst: &Path, force: bool) -> R<()> {
    if dst.is_symlink() {
        let to = std::fs::read_link(dst).unwrap_or_default();
        return Err(Fail(format!(
            "{} is a symbolic link (to {}); not replacing it. Remove it or install with --to elsewhere",
            dst.display(),
            to.display()
        )));
    }
    if dst.exists() && !dst.is_dir() {
        return Err(Fail(format!(
            "[Errno 20] Not a directory: {}",
            py::repr_str(&dst.display().to_string())
        )));
    }
    if dst.exists() && !is_hydo_skill(dst) && !force {
        return Err(Fail(format!(
            "{} exists and is not the {SKILL} skill; not replacing it (use --force)",
            dst.display()
        )));
    }
    Ok(())
}

/// Put the skill folder `src` (complete, already checked) at `dst`, replacing an earlier copy
/// in one step: the new folder is written next to it as `.hydo-onboard.new-<pid>`, the old one
/// is moved aside as `.hydo-onboard.old-<pid>` and deleted after the swap. Nothing else in
/// `dst`'s parent is touched. "installed" or "updated".
pub fn replace_dir(src: &Path, dst: &Path, force: bool) -> R<&'static str> {
    replaceable(dst, force)?;
    let parent = dst
        .parent()
        .ok_or_else(|| Fail(format!("{}: no parent directory", dst.display())))?;
    std::fs::create_dir_all(parent).map_err(|e| io_fail(e, parent))?;
    let pid = std::process::id();
    let new = parent.join(format!(".{SKILL}.new-{pid}"));
    let old = parent.join(format!(".{SKILL}.old-{pid}"));
    for t in [&new, &old] {
        if t.exists() {
            std::fs::remove_dir_all(t).map_err(|e| io_fail(e, t))?;
        }
    }
    let r = (|| {
        copytree(src, &new)?;
        let verb = if dst.exists() {
            std::fs::rename(dst, &old).map_err(|e| io_fail(e, dst))?;
            "updated"
        } else {
            "installed"
        };
        if let Err(e) = std::fs::rename(&new, dst) {
            if old.exists() {
                let _ = std::fs::rename(&old, dst);
            }
            return Err(io_fail(e, dst));
        }
        Ok(verb)
    })();
    let _ = std::fs::remove_dir_all(&new);
    let _ = std::fs::remove_dir_all(&old);
    r
}

const SKILL_USAGE: &str = "usage: hydo skill install [--agent NAME ...] [--to DIR] [--user] [--force]
       hydo skill update  [--agent NAME ...] [--to DIR] [--user] [--force] [--version V] [--check] [--offline]
       hydo skill agents        where each AI tool reads skills
       hydo skill path          the skill folder of this installation
       hydo skill version       the version of the skill this hydo carries
  install: copy the skill this hydo carries (its process; hydo guide holds the facts)
  update:  fetch the newest skill release (tag skill-v<version>) from hydo's GitHub releases,
           check it by sha256 against the release's SHA256SUMS over TLS, and install it; without
           --agent or --to, every copy in this repository's and your home's .claude/skills and
           .agents/skills is updated. --check only reports (exit status 0: up to date, 3: newer
           available); --version installs that release. It uses the network only when you run it.
  --agent claude|codex|cursor|copilot|gemini|all (repeatable, or comma-separated): that tool's
          skill directory of this repository (--user: of your home directory)
  --to DIR: DIR/hydo-onboard instead (any other tool, or a custom location)
  An earlier copy of the skill is replaced; another folder of the same name is kept unless --force.
  Claude Code can also take the skill as a plugin: /plugin marketplace add kakoc/hydo, then
  /plugin install hydo-onboard@hydo (updates come as plugin updates).
";

pub fn skill(argv: &[String], root: &Path, out: &mut Out) -> R<i32> {
    let mut a: Vec<String> = argv.to_vec();
    if a.is_empty() || a[0] == "-h" || a[0] == "--help" {
        out.put(SKILL_USAGE)?;
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
    if cmd == "version" {
        out.put(&format!(
            "{SKILL} {} ({})\n",
            sk.version().unwrap_or_else(|| "unknown".into()),
            sk.shown()
        ))?;
        return Ok(0);
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
    let w = Where::parse(&a, "hydo skill")?;
    if !w.named() {
        return Err(Fail("say where: --agent claude|codex|cursor|copilot|gemini|all, or --to DIR (hydo skill --help)".into()));
    }
    for t in &w.targets()? {
        copy_skill(&sk, t, w.force, out)?;
        if let Some(who) = readers_of(t) {
            out.put(&format!("      read by {who}\n"))?;
        }
    }
    if !w.user && w.to.is_none() {
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

fn agents_block() -> R<String> {
    let text = guide_bytes("init/agents-section.md")
        .map(text_of)
        .ok_or_else(|| Fail("init/agents-section.md: missing from the built-in guide".into()))?;
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

pub fn init(argv: &[String], _root: &Path, out: &mut Out) -> R<i32> {
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
    let (mut made, mut kept) = (Vec::new(), Vec::new());
    for (rel, files) in walk_embedded(embedded::GUIDE, "init/hydo") {
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
        let r = update_agents_md(&dp.join("AGENTS.md"), &agents_block()?)?;
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
        "hydo: next: give your AI tool this instruction: \"run `hydo guide skill` and follow it\"\n",
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
        assert!(embedded::GUIDE
            .iter()
            .any(|(p, m, _)| *p == "init/hydo/workload/parallel_driver_todo" && m & 0o111 != 0));
        assert!(!embedded::MANIFEST_MD.is_empty());
        assert!(Skill::Embedded.version().is_some());
        let t = topics();
        for n in ["workflow", "services", "composer", "checks", "hazards"] {
            assert!(t.iter().any(|(k, _)| k == n), "{n}");
        }
        assert!(!t.iter().any(|(k, _)| k == "overview"));
    }

    #[test]
    fn versions() {
        let md = "---\nname: x\nmetadata:\n  version: \"1.2.0\"\n  hydo-min: \"0.1.0\"\n---\n# x\n";
        assert_eq!(skill_version(md).as_deref(), Some("1.2.0"));
        assert_eq!(skill_version("---\nname: x\nversion: 1\n---\n"), None);
        assert_eq!(skill_version("no frontmatter"), None);
        assert!(version_lt("1.0.0", "1.0.1"));
        assert!(version_lt("1.9.0", "1.10.0"));
        assert!(version_lt("1.0", "1.0.1"));
        assert!(!version_lt("1.0.0", "1.0"));
        assert!(!version_lt("2.0.0", "1.99.99"));
    }

    #[test]
    fn guide_paths() {
        assert!(guide_path("references/composer.md").is_some());
        assert!(guide_path("assets/example-miniflux.yaml").is_some());
        assert!(guide_path("assets/hydo-init/hydo/hydo.yaml").is_some());
        assert!(guide_path("examples/example-redis-composed/hydo/hydo.yaml").is_some());
        assert!(guide_path("../hydo/guide/composer.md").is_none());
        assert!(guide_path("/etc/passwd").is_none());
        let w = walk_embedded(embedded::GUIDE, "init/hydo");
        assert_eq!(w[0].0, ".");
        assert!(w.iter().any(|(r, f)| r == "workload" && f.len() == 1));
    }

    #[test]
    fn lines() {
        assert_eq!(splitlines("a\r\nb\rc\n\nd"), vec!["a", "b", "c", "", "d"]);
        assert_eq!(splitlines("a\n"), vec!["a"]);
    }
}
