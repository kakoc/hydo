//! `hydo update [--check] [--yes] [--version V] [--offline]` (Milestone 315): bring hydo and every
//! copy of its onboarding skill to the newest release, in one command.
//!
//! 1. The target is the newest release `v<major>.<minor>.<patch>` in the project's release list
//!    (`HYDO_RELEASE_API`; drafts, pre-releases and the skill's `skill-v` releases are skipped),
//!    or `--version V`. The newest skill release (`skill-v<version>`) comes from the same list.
//! 2. How this hydo was installed is read from its own program's path ([`kind_of`]):
//!    - Homebrew (a keg under `<prefix>/Cellar/hydo/`): `brew update` when the formula is older
//!      than the target, then `brew upgrade kakoc/tap/hydo`; the formula runs `hydo setup`.
//!    - cargo (`<root>/bin/hydo` with cargo's `.crates2.json` in `<root>`): `cargo install hydo
//!      --locked --version V --root <root>`, then the new hydo's `hydo setup`.
//!    - A release tree that `install.sh` made (`SHA256SUMS` around `bin/hydo`, `~/.hydo` by
//!      default): only this platform's `hydo-<mac|linux>-V.tar.gz` and the release's
//!      `SHA256SUMS` are fetched over TLS (`HYDO_RELEASE_URL`), the archive is accepted only with
//!      the sha256 that file names, unpacked in a temporary folder, and its `install.sh` runs
//!      with `--prefix <the tree>` and the options the tree was installed with (on Linux
//!      `--no-module` unless the hvx module is loaded, `--udev` when its rule is installed;
//!      the module's install asks for sudo itself, and hydo says so first).
//!    - A version of the install cache (`~/.hydo/versions/<version>`, made by `hydo setup
//!      --get`): the same download, then the new hydo's `hydo setup --archive` and `--get V`.
//!    - Anything else (a build in a checkout): nothing of hydo changes, and the commands to do it
//!      by hand are printed.
//!
//!    Then `hydo doctor` of the new hydo runs (install.sh runs it itself).
//! 3. Every copy of the skill is brought to the newest skill release: the copies `hydo skill
//!    update` knows (this repository's and the home's `.claude/skills` and `.agents/skills`),
//!    the `skills/hydo` of each Claude Code configuration folder, and every copy in the registry
//!    of copies (`hydo_agents::copies`); a registry line whose copy is gone is dropped. In each
//!    Claude Code configuration folder whose `plugins/installed_plugins.json` lists `hydo@hydo`,
//!    with the `claude` command on PATH: `claude plugin marketplace update hydo`, then `claude
//!    plugin update hydo@hydo --scope <scope>` (in the project's folder for a project or local
//!    install), each with `CLAUDE_CONFIG_DIR` naming that folder.
//! 4. The plan names every part with its version before and after, and the "Before you upgrade"
//!    section of each newer release's notes. `--check` prints it and exits 0 (all current) or 3
//!    (something is newer). Without `--yes` a terminal is asked once; elsewhere the plan is
//!    printed, nothing changes, and the exit status is 3, so an AI tool shows the plan and runs
//!    `hydo update --yes` after the user agreed.
//!
//! [`doctor_note`] is `hydo doctor`'s line about a newer release: at most one look a day, 3 s at
//! most, the answer kept in `~/.hydo/update-check.json`; offline, or with `HYDO_UPDATE_CHECK=0`,
//! nothing is looked up. The command uses the network only when it runs.

use crate::ctx::{which, Ctx, Platform};
use crate::fetch::{Fetcher, Options};
use crate::skillupdate::{self, Source};
use hydo_agents::copies::{self, PluginInstall, MARKETPLACE, PLUGIN_ID};
use hydo_agents::{version_at, version_lt, SKILL};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub const USAGE: &str = "usage: hydo update [--check] [--yes] [--version V] [--offline]
  Update hydo and every copy of its onboarding skill to the newest release, in one command.
  hydo is updated the way it was installed: Homebrew (brew upgrade kakoc/tap/hydo), cargo
  (cargo install hydo --locked --version V), or the release archive and its install.sh (only
  this platform's archive is downloaded, and it is checked by sha256 against the release's
  SHA256SUMS over TLS). A hydo built in a checkout is left as it is, with the commands to update it.
  The skill is updated in this repository's and your home's .claude/skills and .agents/skills, in
  every Claude Code configuration folder (CLAUDE_CONFIG_DIR, ~/.claude, ~/.claude-*), in every
  folder where hydo installed it before (~/.hydo/skill-copies.txt), and as Claude Code's plugin
  hydo@hydo (claude plugin update). Start a new session of your AI tool afterwards.
  --check: only report; exit status 0 when everything is current, 3 when something is newer
  --yes:   do it without asking (without --yes a terminal is asked once; elsewhere hydo update
           prints the plan, changes nothing, and exits with status 3)
  --version V: install hydo V instead of the newest release
  It uses the network only when you run it (HYDO_OFFLINE or --offline: it refuses).
";

/// A hydo release in the release list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The release notes (the release's body).
    pub notes: String,
}

/// Whether a version is `<major>.<minor>.<patch>`, numbers only.
fn plain_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The hydo releases of a GitHub release list (JSON), oldest first: tags `v<x>.<y>.<z>` that
/// are neither drafts nor pre-releases.
pub fn hydo_releases(list: &[u8]) -> Result<Vec<Release>, String> {
    let v: serde_json::Value =
        serde_json::from_slice(list).map_err(|e| format!("the release list is not JSON: {e}"))?;
    let arr = v
        .as_array()
        .ok_or("the release list is not a JSON list of releases")?;
    let mut out: Vec<Release> = Vec::new();
    for r in arr {
        if r["draft"].as_bool() == Some(true) || r["prerelease"].as_bool() == Some(true) {
            continue;
        }
        let Some(v) = r["tag_name"].as_str().and_then(|t| t.strip_prefix('v')) else {
            continue;
        };
        if !plain_version(v) || out.iter().any(|x| x.version == v) {
            continue;
        }
        out.push(Release {
            version: v.to_string(),
            notes: r["body"].as_str().unwrap_or("").to_string(),
        });
    }
    out.sort_by(|a, b| {
        if version_lt(&a.version, &b.version) {
            std::cmp::Ordering::Less
        } else if version_lt(&b.version, &a.version) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    Ok(out)
}

/// The "Before you upgrade" section of release notes (without its heading), if they have one.
pub fn before_you_upgrade(notes: &str) -> Option<String> {
    let mut lines = notes.lines();
    lines.find(|l| {
        let t = l.trim().trim_start_matches('#').trim();
        l.trim_start().starts_with('#') && t.eq_ignore_ascii_case("before you upgrade")
    })?;
    let body: Vec<&str> = lines
        .take_while(|l| !(l.starts_with("# ") || l.starts_with("## ")))
        .collect();
    let t = body.join("\n").trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// How the running hydo was installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Homebrew: the program is in a keg under `<prefix>/Cellar/hydo/`.
    Brew { prefix: PathBuf },
    /// cargo: `<root>/bin/hydo`, with cargo's install record in `<root>`.
    Cargo { root: PathBuf },
    /// The release archive's `install.sh`: the release tree `root` (`~/.hydo` by default).
    Archive { root: PathBuf },
    /// A version of the install cache `home` (`hydo setup --get`).
    Cache { home: PathBuf },
    /// Anything else, with why hydo does not update it.
    Other { why: String },
}

/// How the hydo whose program is `exe` (its real path) was installed. `checkout`: the checkout
/// it was built in, if any; `cache_home`: the install cache's folder; `own`: its version.
pub fn kind_of(exe: &Path, checkout: Option<&Path>, cache_home: &Path, own: &str) -> Kind {
    let parts: Vec<Component> = exe.components().collect();
    if let Some(i) = parts
        .windows(2)
        .position(|w| w[0].as_os_str() == "Cellar" && w[1].as_os_str() == "hydo")
    {
        let prefix: PathBuf = parts[..i].iter().collect();
        return Kind::Brew { prefix };
    }
    if let Some(c) = checkout {
        return Kind::Other {
            why: format!("this hydo was built in the checkout {}", c.display()),
        };
    }
    let Some(root) = exe.parent().and_then(Path::parent) else {
        return Kind::Other {
            why: format!("hydo does not know how {} was installed", exe.display()),
        };
    };
    let sums = root.join("SHA256SUMS").is_file();
    if !sums {
        let cargo = std::fs::read_to_string(root.join(".crates2.json"))
            .is_ok_and(|t| t.contains("\"hydo "));
        if cargo {
            return Kind::Cargo {
                root: root.to_path_buf(),
            };
        }
        return Kind::Other {
            why: format!(
                "hydo does not know how {} was installed (no release, Homebrew or cargo around it)",
                exe.display()
            ),
        };
    }
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let version_dir = cache_home.join("versions").join(own);
    if real(root) == real(&version_dir) {
        return Kind::Cache {
            home: cache_home.to_path_buf(),
        };
    }
    Kind::Archive {
        root: root.to_path_buf(),
    }
}

impl Kind {
    /// The way of the install, in words.
    fn words(&self) -> String {
        match self {
            Kind::Brew { prefix } => format!("installed with Homebrew ({})", prefix.display()),
            Kind::Cargo { root } => format!("installed by cargo into {}", root.display()),
            Kind::Archive { root } => {
                format!("installed from the release archive into {}", root.display())
            }
            Kind::Cache { home } => format!(
                "installed into the install cache {} (hydo setup --get)",
                home.display()
            ),
            Kind::Other { why } => why.clone(),
        }
    }

    /// What updating hydo to `v` does, in words.
    fn how(&self, p: Platform, v: &str) -> String {
        let archive = hydo_image::releases::archive_name(crate::setup::platform_name(p), v);
        match self {
            Kind::Brew { .. } => "brew upgrade kakoc/tap/hydo (after brew update when the formula is older)".into(),
            Kind::Cargo { root } => format!(
                "cargo install hydo --locked --version {v} --root {}, then hydo setup",
                root.display()
            ),
            Kind::Archive { root } => format!(
                "download {archive} and SHA256SUMS, check its sha256, run its install.sh --prefix {}",
                root.display()
            ),
            Kind::Cache { .. } => format!(
                "download {archive} and SHA256SUMS, check its sha256, then its hydo setup --get {v}"
            ),
            Kind::Other { .. } => "nothing (see the commands below)".into(),
        }
    }
}

/// One copy of the skill: its folder and its version.
struct Copy {
    path: PathBuf,
    version: Option<String>,
}

/// One Claude Code configuration folder with installs of the plugin.
struct ClaudeConfig {
    dir: PathBuf,
    installs: Vec<PluginInstall>,
}

/// What hydo update found.
struct Plan {
    own: String,
    kind: Kind,
    /// The release to install (None: no release found).
    target: Option<String>,
    hydo_behind: bool,
    /// (version, its "Before you upgrade" section) of each release after this one up to the
    /// target.
    notes: Vec<(String, String)>,
    skill: Option<String>,
    copies: Vec<Copy>,
    /// Registry lines whose copy is gone.
    stale: Vec<PathBuf>,
    claude: Vec<ClaudeConfig>,
    claude_cmd: Option<PathBuf>,
}

fn behind(v: Option<&str>, want: Option<&str>) -> bool {
    match (v, want) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(v), Some(w)) => version_lt(v, w),
    }
}

impl Plan {
    fn copies_behind(&self) -> impl Iterator<Item = &Copy> {
        self.copies
            .iter()
            .filter(|c| behind(c.version.as_deref(), self.skill.as_deref()))
    }

    fn installs_behind(&self) -> Vec<(&ClaudeConfig, &PluginInstall)> {
        let mut v = Vec::new();
        for c in &self.claude {
            for i in &c.installs {
                if i.scope != "managed" && behind(i.version.as_deref(), self.skill.as_deref()) {
                    v.push((c, i));
                }
            }
        }
        v
    }

    /// Whether anything is newer (hydo, a copy, or a plugin install that hydo can update).
    fn anything_newer(&self) -> bool {
        self.hydo_behind
            || self.copies_behind().next().is_some()
            || (self.claude_cmd.is_some() && !self.installs_behind().is_empty())
    }
}

/// The commands that update a hydo hydo cannot update itself.
fn by_hand(p: Platform, v: &str) -> Vec<String> {
    let mut l = vec![
        "  To update it by hand: in the checkout, git pull and cargo build --release -p hydo; or \
         install a release:"
            .to_string(),
        format!("    cargo install hydo --locked --version {v} && hydo setup"),
    ];
    if p == Platform::Mac {
        l.push("    brew install kakoc/tap/hydo".into());
    }
    l.push(format!(
        "    or the archive: https://github.com/kakoc/hydo/releases/tag/v{v} (docs/INSTALL.md)"
    ));
    l
}

fn print_plan(plan: &Plan, p: Platform, out: &mut dyn std::io::Write) {
    let say = |out: &mut dyn std::io::Write, s: String| {
        let _ = writeln!(out, "{s}");
    };
    say(
        out,
        format!("hydo update: hydo {}, {}", plan.own, plan.kind.words()),
    );
    match &plan.target {
        None => say(out, "  hydo: no release found in the release list".into()),
        Some(t) if plan.hydo_behind => {
            say(
                out,
                format!("  hydo: {} -> {t}: {}", plan.own, plan.kind.how(p, t)),
            );
            if matches!(plan.kind, Kind::Other { .. }) {
                for l in by_hand(p, t) {
                    say(out, l);
                }
            }
        }
        Some(t) if version_lt(t, &plan.own) => say(
            out,
            format!(
                "  hydo: {}, newer than the newest release {t}; kept",
                plan.own
            ),
        ),
        Some(_) => say(
            out,
            format!("  hydo: {}, the newest release (up to date)", plan.own),
        ),
    }
    let sk = plan.skill.as_deref();
    if plan.copies.is_empty() && plan.claude.is_empty() {
        say(
            out,
            format!(
                "  skill: no copy of the {SKILL} skill found (hydo skill install --agent <tool> \
                 installs one)"
            ),
        );
    }
    for c in &plan.copies {
        let v = c.version.as_deref().unwrap_or("?");
        let line = match sk {
            None => format!(
                "  skill {}: {SKILL} {v} (no skill release found)",
                c.path.display()
            ),
            Some(s) if behind(c.version.as_deref(), sk) => {
                format!("  skill {}: {SKILL} {v} -> {s}", c.path.display())
            }
            Some(_) => format!("  skill {}: {SKILL} {v}, up to date", c.path.display()),
        };
        say(out, line);
    }
    for c in &plan.claude {
        for i in &c.installs {
            let v = i.version.as_deref().unwrap_or("?");
            let wherein = match &i.project {
                Some(pr) => format!("scope {}, project {}", i.scope, pr.display()),
                None => format!("scope {}", i.scope),
            };
            let state = if i.scope == "managed" {
                format!("{v}, managed by your organization; hydo leaves it alone")
            } else if !behind(i.version.as_deref(), sk) {
                format!("{v}, up to date")
            } else if plan.claude_cmd.is_none() {
                format!(
                    "{v}, release {} is newer; the claude command is not on PATH, so Claude \
                     Code's own plugin update takes it",
                    sk.unwrap_or("?")
                )
            } else {
                format!(
                    "{v} -> {}: claude plugin update {PLUGIN_ID}",
                    sk.unwrap_or("?")
                )
            };
            say(
                out,
                format!(
                    "  Claude Code {}: plugin {PLUGIN_ID} {state} ({wherein})",
                    c.dir.display()
                ),
            );
        }
    }
    if !plan.stale.is_empty() {
        say(
            out,
            format!(
                "  {} registered cop{} gone; {} dropped from the registry of copies:",
                plan.stale.len(),
                if plan.stale.len() == 1 {
                    "y is"
                } else {
                    "ies are"
                },
                if plan.stale.len() == 1 {
                    "its line is"
                } else {
                    "their lines are"
                }
            ),
        );
        for s in &plan.stale {
            say(out, format!("    {}", s.display()));
        }
    }
    if plan.hydo_behind {
        for (v, n) in &plan.notes {
            say(
                out,
                format!("Before you upgrade to hydo {v} (its release notes):"),
            );
            for l in n.lines() {
                say(out, format!("  {l}"));
            }
        }
    }
}

/// Where hydo update gets what it needs (the tests give their own).
pub struct Env {
    pub platform: Platform,
    pub own: String,
    /// The running program's real path.
    pub exe: PathBuf,
    pub checkout: Option<PathBuf>,
    pub cache_home: PathBuf,
    pub home: PathBuf,
    pub claude_config_dir: Option<PathBuf>,
    pub api: String,
    pub base: String,
    pub offline: bool,
    pub tmp: PathBuf,
    /// The hvx module is loaded (Linux), and the udev rule's group if its rule is installed.
    pub module: bool,
    pub udev_group: Option<String>,
    /// stdin and stdout are a terminal.
    pub terminal: bool,
}

struct Args {
    check: bool,
    yes: bool,
    version: Option<String>,
    offline: bool,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut a = Args {
        check: false,
        yes: false,
        version: None,
        offline: false,
    };
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--check" => a.check = true,
            "--yes" | "-y" => a.yes = true,
            "--offline" => a.offline = true,
            "--version" => {
                let v = argv
                    .get(i + 1)
                    .filter(|v| !v.starts_with('-'))
                    .ok_or("--version needs a version, for example --version 0.4.0")?;
                a.version = Some(v.trim_start_matches('v').to_string());
                i += 1;
            }
            x => {
                if let Some(v) = x.strip_prefix("--version=") {
                    a.version = Some(v.trim_start_matches('v').to_string());
                } else {
                    return Err(format!("unknown argument {x:?}"));
                }
            }
        }
        i += 1;
    }
    if let Some(v) = &a.version {
        if !plain_version(v) {
            return Err(format!(
                "--version {v:?}: not a release version (<major>.<minor>.<patch>)"
            ));
        }
    }
    if a.check && a.yes {
        return Err("--check changes nothing; not with --yes".into());
    }
    Ok(a)
}

fn opts(base: &str) -> Options {
    Options::standard(base)
}

/// Every copy of the skill this hydo can find, and the registry lines whose copy is gone.
fn find_copies(configs: &[PathBuf]) -> (Vec<Copy>, Vec<PathBuf>) {
    let mut paths: Vec<PathBuf> = hydo_agents::installed_copies().unwrap_or_default();
    for c in configs {
        let p = c.join("skills").join(hydo_agents::PLUGIN);
        if copies::is_copy(&p) {
            paths.push(p);
        }
    }
    let mut stale = Vec::new();
    if let Some(f) = copies::registry_file() {
        for (p, _) in copies::read_registry(&f) {
            if copies::is_copy(&p) {
                paths.push(p);
            } else {
                stale.push(p);
            }
        }
    }
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut v = Vec::new();
    for p in paths {
        let real = p.canonicalize().unwrap_or_else(|_| p.clone());
        if seen.contains(&real) {
            continue;
        }
        seen.push(real);
        let version = version_at(&p);
        v.push(Copy { path: p, version });
    }
    (v, stale)
}

/// Run a program with this process's terminal, after saying what it is.
fn run_cmd(
    out: &mut dyn std::io::Write,
    prog: &Path,
    args: &[String],
    cwd: Option<&Path>,
    envs: &[(&str, &Path)],
) -> Result<(), String> {
    let shown = std::iter::once(prog.display().to_string())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    let mut prefix = String::new();
    for (k, v) in envs {
        prefix.push_str(&format!("{k}={} ", v.display()));
    }
    let in_dir = cwd
        .map(|d| format!(" (in {})", d.display()))
        .unwrap_or_default();
    let _ = writeln!(out, "hydo update: running {prefix}{shown}{in_dir}");
    let _ = out.flush();
    let mut c = Command::new(prog);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    for (k, v) in envs {
        c.env(k, v);
    }
    let st = c.status().map_err(|e| format!("{shown}: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!(
            "{shown} failed ({})",
            st.code()
                .map(|c| format!("exit status {c}"))
                .unwrap_or_else(|| "killed by a signal".into())
        ))
    }
}

/// The output of a program (stdout), or None.
fn output_of(prog: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new(prog).args(args).output().ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// A program in `dir`, else on PATH.
fn program(dir: Option<&Path>, name: &str) -> Option<PathBuf> {
    dir.map(|d| d.join(name))
        .filter(|p| p.is_file())
        .or_else(|| which(name))
}

/// Make a tree removable, then remove it.
fn remove_tree(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fn writable(p: &Path) {
        let Ok(md) = std::fs::symlink_metadata(p) else {
            return;
        };
        if md.file_type().is_symlink() {
            return;
        }
        let m = md.permissions().mode();
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(m | 0o700));
        if md.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    writable(&e.path());
                }
            }
        }
    }
    if std::fs::symlink_metadata(dir).is_ok() {
        writable(dir);
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Download release `v`'s archive of this platform into `work` (checked by the sha256 its
/// `SHA256SUMS` names; the sums are kept next to it) and unpack it there. Returns the archive
/// and the unpacked release tree.
fn fetch_release(env: &Env, v: &str, work: &Path) -> Result<(PathBuf, PathBuf), String> {
    let plat = crate::setup::platform_name(env.platform);
    let name = hydo_image::releases::archive_name(plat, v);
    let f = Fetcher::new(opts(&env.base))?;
    let sums_url = crate::fetch::release_url(&env.base, v, crate::cache::SUMS_ASSET);
    let sums = f.get_small(&sums_url, 1 << 20)?;
    let text = String::from_utf8(sums).map_err(|_| format!("{sums_url}: not text"))?;
    let sha = crate::setup::parse_sums(&text)
        .map_err(|e| format!("{sums_url}: {e}"))?
        .into_iter()
        .find(|(_, p)| *p == name)
        .map(|(s, _)| s)
        .ok_or_else(|| format!("{sums_url} does not list {name}"))?;
    std::fs::create_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))?;
    std::fs::write(work.join(crate::cache::SUMS_ASSET), &text)
        .map_err(|e| format!("{}: {e}", work.display()))?;
    let part = work.join(format!("{name}.part"));
    f.download(
        &crate::fetch::release_url(&env.base, v, &name),
        &part,
        &sha,
        &format!("hydo {v}"),
    )?;
    let archive = work.join(&name);
    std::fs::rename(&part, &archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let into = work.join("unpacked");
    std::fs::create_dir_all(&into).map_err(|e| format!("{}: {e}", into.display()))?;
    let fa = std::fs::File::open(&archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut a = tar::Archive::new(flate2::read::GzDecoder::new(fa));
    a.set_preserve_permissions(true);
    a.set_preserve_mtime(true);
    a.unpack(&into)
        .map_err(|e| format!("cannot unpack {name}: {e}"))?;
    let top = hydo_image::releases::top_dir(plat, v);
    let names: Vec<String> = std::fs::read_dir(&into)
        .map_err(|e| format!("{}: {e}", into.display()))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    if names != [top.clone()] {
        return Err(format!(
            "{name} does not hold exactly the folder {top} (it holds {})",
            names.join(", ")
        ));
    }
    Ok((archive, into.join(top)))
}

/// Update hydo itself to `v`.
fn update_hydo(
    env: &Env,
    kind: &Kind,
    v: &str,
    out: &mut dyn std::io::Write,
) -> Result<(), String> {
    let say = |out: &mut dyn std::io::Write, s: String| {
        let _ = writeln!(out, "{s}");
    };
    match kind {
        Kind::Other { why } => {
            say(out, format!("hydo update: hydo is not changed: {why}."));
            for l in by_hand(env.platform, v) {
                say(out, l);
            }
            Ok(())
        }
        Kind::Brew { prefix } => {
            let bin = prefix.join("bin");
            let brew = program(Some(&bin), "brew")
                .ok_or("the brew command is not on PATH, nor in Homebrew's bin folder")?;
            let formula = "kakoc/tap/hydo";
            let stable = |brew: &Path| -> Option<String> {
                let t = output_of(brew, &["info", "--json=v2", formula])?;
                let j: serde_json::Value = serde_json::from_str(&t).ok()?;
                j["formulae"][0]["versions"]["stable"]
                    .as_str()
                    .map(str::to_string)
            };
            if stable(&brew).is_none_or(|s| version_lt(&s, v)) {
                run_cmd(out, &brew, &["update".into()], None, &[])?;
            }
            match stable(&brew) {
                Some(s) if s != v => {
                    return Err(format!(
                        "Homebrew's formula {formula} is at {s}, not {v}; Homebrew installs the \
                         formula's version only (use the archive or cargo for another version)"
                    ))
                }
                _ => {}
            }
            run_cmd(out, &brew, &["upgrade".into(), formula.into()], None, &[])?;
            let hydo = bin.join("hydo");
            run_cmd(out, &hydo, &["doctor".into()], None, &[])
                .unwrap_or_else(|e| say(out, format!("hydo update: {e} (see above)")));
            Ok(())
        }
        Kind::Cargo { root } => {
            let cargo = program(Some(&root.join("bin")), "cargo")
                .ok_or("the cargo command is not on PATH")?;
            run_cmd(
                out,
                &cargo,
                &[
                    "install".into(),
                    "hydo".into(),
                    "--locked".into(),
                    "--version".into(),
                    v.into(),
                    "--root".into(),
                    root.display().to_string(),
                ],
                None,
                &[],
            )?;
            let hydo = root.join("bin").join("hydo");
            run_cmd(out, &hydo, &["setup".into()], None, &[])?;
            run_cmd(out, &hydo, &["doctor".into()], None, &[])
                .unwrap_or_else(|e| say(out, format!("hydo update: {e} (see above)")));
            Ok(())
        }
        Kind::Archive { root } => {
            let work = env.tmp.join(format!("hydo-update-{}", std::process::id()));
            remove_tree(&work);
            let r = (|| {
                let (_, tree) = fetch_release(env, v, &work)?;
                let mut args = vec!["--prefix".to_string(), root.display().to_string()];
                if env.platform == Platform::Linux {
                    if env.module {
                        say(
                            out,
                            "hydo update: the kernel module is loaded here, so install.sh builds \
                             and loads the new one; that needs sudo, and sudo asks for your \
                             password"
                                .into(),
                        );
                    } else {
                        args.push("--no-module".into());
                    }
                    if let Some(g) = &env.udev_group {
                        args.extend(["--udev".into(), "--udev-group".into(), g.clone()]);
                    }
                }
                let mut sh_args = vec![tree.join("install.sh").display().to_string()];
                sh_args.extend(args);
                run_cmd(out, Path::new("/bin/sh"), &sh_args, None, &[])
            })();
            remove_tree(&work);
            r
        }
        Kind::Cache { home } => {
            let work = env.tmp.join(format!("hydo-update-{}", std::process::id()));
            remove_tree(&work);
            let r = (|| {
                let (archive, tree) = fetch_release(env, v, &work)?;
                let new = tree.join("bin").join("hydo");
                let h: &[(&str, &Path)] = &[("HYDO_HOME", home)];
                run_cmd(
                    out,
                    &new,
                    &[
                        "setup".into(),
                        "--archive".into(),
                        archive.display().to_string(),
                    ],
                    None,
                    h,
                )?;
                run_cmd(
                    out,
                    &new,
                    &["setup".into(), "--get".into(), v.into()],
                    None,
                    h,
                )
            })();
            remove_tree(&work);
            r?;
            let hydo = home.join("versions").join(v).join("bin").join("hydo");
            run_cmd(out, &hydo, &["doctor".into()], None, &[])
                .unwrap_or_else(|e| say(out, format!("hydo update: {e} (see above)")));
            Ok(())
        }
    }
}

/// The run, with everything it reads given (tests).
pub fn run_with(
    argv: &[String],
    env: &Env,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
    answer: &mut dyn FnMut() -> bool,
) -> i32 {
    let a = match parse(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(err, "hydo update: {e}\n{USAGE}");
            return 2;
        }
    };
    if a.offline || env.offline {
        let _ = writeln!(
            err,
            "hydo update: it needs the network to find the newest release, and the network is \
             off (--offline or HYDO_OFFLINE); nothing was changed"
        );
        return 1;
    }
    match update(&a, env, out, answer) {
        Ok(rc) => rc,
        Err(e) => {
            let _ = writeln!(err, "hydo update: {e}");
            1
        }
    }
}

fn update(
    a: &Args,
    env: &Env,
    out: &mut dyn std::io::Write,
    answer: &mut dyn FnMut() -> bool,
) -> Result<i32, String> {
    let api = Fetcher::new(opts(&env.api))?;
    let list = api
        .get_small(
            &format!("{}?per_page=100", env.api.trim_end_matches('/')),
            8 << 20,
        )
        .map_err(|e| format!("cannot read the release list: {e}; nothing was changed"))?;
    let releases = hydo_releases(&list)?;
    let skill = skillupdate::newest_in(&list)?;
    let target = match &a.version {
        Some(v) => {
            if !releases.iter().any(|r| r.version == *v) {
                return Err(format!(
                    "the release list has no hydo {v} (it has {})",
                    releases
                        .iter()
                        .map(|r| r.version.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Some(v.clone())
        }
        None => releases.last().map(|r| r.version.clone()),
    };
    let kind = kind_of(&env.exe, env.checkout.as_deref(), &env.cache_home, &env.own);
    let hydo_behind = match (&target, &a.version) {
        (Some(t), Some(_)) => *t != env.own,
        (Some(t), None) => version_lt(&env.own, t),
        _ => false,
    };
    let notes = match &target {
        Some(t) => releases
            .iter()
            .filter(|r| version_lt(&env.own, &r.version) && !version_lt(t, &r.version))
            .filter_map(|r| before_you_upgrade(&r.notes).map(|n| (r.version.clone(), n)))
            .collect(),
        None => Vec::new(),
    };
    let configs = copies::claude_config_dirs(&env.home, env.claude_config_dir.as_deref());
    let (found, stale) = find_copies(&configs);
    let claude: Vec<ClaudeConfig> = configs
        .iter()
        .map(|d| ClaudeConfig {
            dir: d.clone(),
            installs: copies::plugin_installs(d),
        })
        .filter(|c| !c.installs.is_empty())
        .collect();
    let plan = Plan {
        own: env.own.clone(),
        kind,
        target,
        hydo_behind,
        notes,
        skill,
        copies: found,
        stale,
        claude,
        claude_cmd: which("claude"),
    };
    print_plan(&plan, env.platform, out);
    let newer = plan.anything_newer();
    if a.check {
        return Ok(if newer { 3 } else { 0 });
    }
    if !newer && plan.stale.is_empty() {
        let _ = writeln!(out, "hydo update: everything is up to date");
        return Ok(0);
    }
    if !a.yes {
        if !env.terminal {
            let _ = writeln!(
                out,
                "hydo update: nothing was changed. Run `hydo update --yes` to do this."
            );
            return Ok(3);
        }
        let _ = write!(out, "Go ahead? [y/N] ");
        let _ = out.flush();
        if !answer() {
            let _ = writeln!(out, "hydo update: nothing was changed");
            return Ok(3);
        }
    }
    let mut failed: Vec<String> = Vec::new();
    let mut done: Vec<String> = Vec::new();
    // hydo
    if plan.hydo_behind {
        if let Some(t) = &plan.target {
            match update_hydo(env, &plan.kind, t, out) {
                Ok(()) if matches!(plan.kind, Kind::Other { .. }) => {}
                Ok(()) => done.push(format!("hydo {} -> {t}", plan.own)),
                Err(e) => failed.push(format!("hydo was not updated: {e}")),
            }
        }
    }
    // the copies of the skill
    let todo: Vec<&Copy> = plan.copies_behind().collect();
    if let (Some(s), false) = (&plan.skill, todo.is_empty()) {
        let work = env
            .tmp
            .join(format!("hydo-update-skill-{}", std::process::id()));
        remove_tree(&work);
        let r = (|| -> Result<(), String> {
            std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
            let src = Source::new(&env.api, &env.base, opts)?;
            let got = src.get(s, &work)?;
            for c in &todo {
                match hydo_agents::replace_dir(&got, &c.path, false) {
                    Ok(_) => {
                        copies::record(&c.path, s);
                        done.push(format!(
                            "skill {}: {SKILL} {} -> {s}",
                            c.path.display(),
                            c.version.as_deref().unwrap_or("?")
                        ));
                    }
                    Err(e) => failed.push(format!("{}: {}", c.path.display(), e.0)),
                }
            }
            Ok(())
        })();
        remove_tree(&work);
        if let Err(e) = r {
            failed.push(format!("the skill copies were not updated: {e}"));
        }
    }
    // Claude Code's plugin installs
    if let Some(claude) = &plan.claude_cmd {
        let mut refreshed: Vec<PathBuf> = Vec::new();
        for (c, i) in plan.installs_behind() {
            let envs: &[(&str, &Path)] = &[("CLAUDE_CONFIG_DIR", &c.dir)];
            if !refreshed.contains(&c.dir) {
                refreshed.push(c.dir.clone());
                if let Err(e) = run_cmd(
                    out,
                    claude,
                    &[
                        "plugin".into(),
                        "marketplace".into(),
                        "update".into(),
                        MARKETPLACE.into(),
                    ],
                    None,
                    envs,
                ) {
                    failed.push(e);
                    continue;
                }
            }
            let cwd = match (&i.project, i.scope.as_str()) {
                (Some(p), "project" | "local") => {
                    if !p.is_dir() {
                        failed.push(format!(
                            "{} ({PLUGIN_ID}, scope {}): its project {} is gone",
                            c.dir.display(),
                            i.scope,
                            p.display()
                        ));
                        continue;
                    }
                    Some(p.as_path())
                }
                _ => None,
            };
            match run_cmd(
                out,
                claude,
                &[
                    "plugin".into(),
                    "update".into(),
                    PLUGIN_ID.into(),
                    "--scope".into(),
                    i.scope.clone(),
                ],
                cwd,
                envs,
            ) {
                Ok(()) => {
                    let now = copies::plugin_installs(&c.dir)
                        .into_iter()
                        .find(|x| x.scope == i.scope && x.project == i.project)
                        .and_then(|x| x.version);
                    done.push(format!(
                        "Claude Code {}: plugin {PLUGIN_ID} {} -> {} (scope {})",
                        c.dir.display(),
                        i.version.as_deref().unwrap_or("?"),
                        now.as_deref().unwrap_or("?"),
                        i.scope
                    ));
                }
                Err(e) => failed.push(e),
            }
        }
    }
    // the registry: lines of copies that are gone are dropped
    if let (Some(f), false) = (copies::registry_file(), plan.stale.is_empty()) {
        let keep: Vec<(PathBuf, String)> = copies::read_registry(&f)
            .into_iter()
            .filter(|(p, _)| !plan.stale.contains(p))
            .collect();
        match copies::write_registry(&f, &keep) {
            Ok(()) => done.push(format!(
                "{} line{} of copies that are gone dropped from {}",
                plan.stale.len(),
                if plan.stale.len() == 1 { "" } else { "s" },
                f.display()
            )),
            Err(e) => failed.push(format!("{}: {e}", f.display())),
        }
    }
    let _ = writeln!(out, "hydo update: what changed:");
    if done.is_empty() {
        let _ = writeln!(out, "  nothing");
    }
    for d in &done {
        let _ = writeln!(out, "  {d}");
    }
    for f in &failed {
        let _ = writeln!(out, "  FAILED: {f}");
    }
    if done
        .iter()
        .any(|d| d.starts_with("skill") || d.starts_with("Claude Code"))
    {
        let _ = writeln!(
            out,
            "Start a new session of your AI tool (Claude Code, Codex, ...): a session that runs \
             now keeps the skill it loaded."
        );
    }
    Ok(i32::from(!failed.is_empty()))
}

/// Whether stdin and stdout are a terminal.
fn terminal() -> bool {
    // SAFETY: isatty only reads the descriptors' state.
    unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 }
}

/// The group of the installed udev rule of `/dev/hvx`, if one is installed.
fn udev_group() -> Option<String> {
    let rule = crate::module::host(crate::module::udev_rule_path("hvx"));
    let t = std::fs::read_to_string(rule).ok()?;
    let at = t.find("GROUP=\"")? + "GROUP=\"".len();
    let g: String = t[at..].chars().take_while(|c| *c != '"').collect();
    (!g.is_empty()).then_some(g)
}

/// `hydo update`.
pub fn cmd_update(ctx: &Ctx, argv: &[String]) -> i32 {
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return 0;
    }
    let c = &ctx.config.cli.cache;
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .unwrap_or_default();
    let env = Env {
        platform: ctx.platform,
        own: hydo_image::hydo_version().to_string(),
        exe,
        checkout: ctx.checkout.clone(),
        cache_home: c.home.clone(),
        home: crate::ctx::home_join(".claude")
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default(),
        claude_config_dir: c.claude_config_dir.clone(),
        api: c.release_api.clone(),
        base: c.release_url.clone(),
        offline: c.offline,
        tmp: std::env::temp_dir(),
        module: ctx.platform == Platform::Linux
            && crate::module::loaded_modules().iter().any(|m| m == "hvx"),
        udev_group: (ctx.platform == Platform::Linux).then(udev_group).flatten(),
        terminal: terminal(),
    };
    let mut so = std::io::stdout().lock();
    let mut se = std::io::stderr().lock();
    let mut answer = || {
        let mut l = String::new();
        let _ = std::io::stdin().read_line(&mut l);
        matches!(l.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    };
    run_with(argv, &env, &mut so, &mut se, &mut answer)
}

// ---------------------------------------------------------------- hydo doctor's line

/// The file of `hydo doctor`'s look for a newer release, in the install cache's folder.
pub const CHECK_FILE: &str = "update-check.json";

/// Seconds between two looks.
const DAY: u64 = 24 * 3600;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The newest release that the look at `api` found, kept in `file` for a day; a failed look is
/// kept too (as no answer), so an offline machine looks once a day only. `fetch` makes the
/// look (it gets the list's URL).
pub fn newest_known(
    file: &Path,
    api: &str,
    at: u64,
    fetch: &dyn Fn(&str) -> Result<Vec<u8>, String>,
) -> Option<String> {
    let kept: Option<serde_json::Value> = std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    if let Some(k) = &kept {
        let checked = k["checked"].as_u64().unwrap_or(0);
        if k["api"].as_str() == Some(api) && checked <= at && at - checked < DAY {
            return k["newest"].as_str().map(str::to_string);
        }
    }
    let newest = fetch(&format!("{}?per_page=100", api.trim_end_matches('/')))
        .ok()
        .and_then(|b| hydo_releases(&b).ok())
        .and_then(|r| r.last().map(|r| r.version.clone()));
    let doc = serde_json::json!({"checked": at, "api": api, "newest": newest});
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = file.with_extension(format!("tmp-{}", std::process::id()));
    if std::fs::write(&tmp, doc.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, file);
    }
    newest
}

/// `hydo doctor`'s line when a newer hydo release exists (None: none, offline, the look is off,
/// or the answer could not be had).
pub fn doctor_note(ctx: &Ctx) -> Option<String> {
    let c = &ctx.config.cli.cache;
    if !c.update_check || c.offline {
        return None;
    }
    let f = Fetcher::new(Options::quick(&c.release_api, 3)).ok()?;
    let own = hydo_image::hydo_version();
    let newest = newest_known(&c.home.join(CHECK_FILE), &c.release_api, now(), &|u| {
        f.get_small(u, 8 << 20)
    })?;
    version_lt(own, &newest).then(|| {
        format!(
            "note: hydo {newest} is out (this is hydo {own}); `hydo update` installs it and \
             updates the onboarding skill (HYDO_UPDATE_CHECK=0 turns this look off)"
        )
    })
}

#[cfg(test)]
mod tests;
