//! `hydo skill update [--agent NAME ...] [--to DIR] [--user] [--force] [--version V] [--check]
//! [--offline]` (Milestone 170): install the newest release of the onboarding skill, which is
//! released separately from hydo (docs/RELEASING.md, "Releasing the onboarding skill").
//!
//! The newest release is the highest `skill-v<version>` tag in the project's release list
//! (`HYDO_RELEASE_API`, GitHub's API; drafts and pre-releases are skipped). Its two assets come
//! from `<HYDO_RELEASE_URL>/skill-v<version>/`: `SHA256SUMS`, trusted through TLS as the install
//! cache trusts a release's own sums (Milestone 162), and `hydo-skill-<version>.tar.gz`, accepted
//! only with the sha256 that file names. The archive is unpacked by `hydo_agents::release::unpack`
//! (plain files and directories below `hydo-onboard/` only) into a temporary directory, and each
//! target folder is replaced in one step by `hydo_agents::replace_dir`; nothing outside the skill
//! folders is written, except two short-lived siblings `.hydo-onboard.new-<pid>` and
//! `.hydo-onboard.old-<pid>` that are deleted again. The command uses the network only when the
//! user runs it; nothing it fetches is executed.

use crate::ctx::Ctx;
use crate::fetch::{Fetcher, Options};
use hydo_agents::release::{archive_name, unpack, TAG_PREFIX};
use hydo_agents::{readers_of, replace_dir, version_at, version_lt, Where, SKILL};
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: hydo skill update [--agent NAME ...] [--to DIR] [--user] [--force] [--version V] [--check] [--offline]
  (hydo skill --help explains each option)
";

/// What the update was asked to do.
struct Args {
    w: Where,
    check: bool,
    version: Option<String>,
    offline: bool,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut rest = Vec::new();
    let (mut check, mut version, mut offline) = (false, None, false);
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--check" => check = true,
            "--offline" => offline = true,
            "--version" => {
                let v = argv
                    .get(i + 1)
                    .filter(|v| !v.starts_with('-'))
                    .ok_or("--version needs a version, for example --version 1.0.0")?;
                version = Some(v.trim_start_matches('v').to_string());
                i += 1;
            }
            a => {
                if let Some(v) = a.strip_prefix("--version=") {
                    version = Some(v.trim_start_matches('v').to_string());
                } else {
                    rest.push(a.to_string());
                }
            }
        }
        i += 1;
    }
    if let Some(v) = &version {
        if v.is_empty()
            || !v
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            return Err(format!("--version {v:?}: not a version"));
        }
    }
    let w = Where::parse(&rest, "hydo skill").map_err(|e| e.0)?;
    Ok(Args {
        w,
        check,
        version,
        offline,
    })
}

/// The newest skill release in a GitHub release list (JSON): the highest `skill-v<version>` tag
/// that is neither a draft nor a pre-release.
pub fn newest_in(list: &[u8]) -> Result<Option<String>, String> {
    let v: serde_json::Value =
        serde_json::from_slice(list).map_err(|e| format!("the release list is not JSON: {e}"))?;
    let arr = v
        .as_array()
        .ok_or("the release list is not a JSON list of releases")?;
    let mut best: Option<String> = None;
    for r in arr {
        if r["draft"].as_bool() == Some(true) || r["prerelease"].as_bool() == Some(true) {
            continue;
        }
        let Some(v) = r["tag_name"]
            .as_str()
            .and_then(|t| t.strip_prefix(TAG_PREFIX))
        else {
            continue;
        };
        if v.is_empty() || !v.split('.').all(|x| x.parse::<u64>().is_ok()) {
            continue;
        }
        if best.as_deref().is_none_or(|b| version_lt(b, v)) {
            best = Some(v.to_string());
        }
    }
    Ok(best)
}

/// Where the skill releases come from, and how they are fetched.
pub struct Source {
    pub api: String,
    pub base: String,
    pub api_fetch: Fetcher,
    pub fetch: Fetcher,
}

impl Source {
    pub fn new(api: &str, base: &str, opts: impl Fn(&str) -> Options) -> Result<Source, String> {
        Ok(Source {
            api: api.trim_end_matches('/').to_string(),
            base: base.trim_end_matches('/').to_string(),
            api_fetch: Fetcher::new(opts(api))?,
            fetch: Fetcher::new(opts(base))?,
        })
    }

    fn url(&self, version: &str, file: &str) -> String {
        format!("{}/{TAG_PREFIX}{version}/{file}", self.base)
    }

    /// The newest skill release.
    pub fn newest(&self) -> Result<String, String> {
        let url = format!("{}?per_page=100", self.api);
        let body = self.api_fetch.get_small(&url, 8 << 20)?;
        newest_in(&body)?.ok_or_else(|| {
            format!(
                "{}: no skill release (no tag {TAG_PREFIX}<version>) in the project's releases",
                self.api
            )
        })
    }

    /// Download release `version`, check it by sha256 and unpack it into `dir/hydo-onboard`.
    pub fn get(&self, version: &str, dir: &Path) -> Result<PathBuf, String> {
        let name = archive_name(version);
        let sums_url = self.url(version, crate::cache::SUMS_ASSET);
        let sums = self.fetch.get_small(&sums_url, 1 << 20)?;
        let sums = String::from_utf8_lossy(&sums);
        let sha = crate::setup::parse_sums(&sums)
            .map_err(|e| format!("{sums_url}: {e}"))?
            .into_iter()
            .find(|(_, p)| *p == name)
            .map(|(s, _)| s)
            .ok_or_else(|| format!("{sums_url} does not list {name}"))?;
        let part = dir.join(format!("{name}.part"));
        self.fetch.download(
            &self.url(version, &name),
            &part,
            &sha,
            &format!("the onboarding skill {version}"),
        )?;
        let bytes = std::fs::read(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let out = dir.join(SKILL);
        let got = unpack(&bytes, &out).map_err(|e| format!("{name}: {}", e.0))?;
        if got != version {
            return Err(format!(
                "{name}: its SKILL.md says version {got}, not {version}; refused"
            ));
        }
        Ok(out)
    }
}

/// The run, with the source and the place for temporary files given (tests).
pub fn run_with(
    argv: &[String],
    source: Option<&Source>,
    tmp: &Path,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32 {
    let a = match parse(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(err, "hydo: {e}\n{USAGE}");
            return 2;
        }
    };
    match update(&a, source, tmp, out) {
        Ok(rc) => rc,
        Err(e) => {
            let _ = writeln!(err, "hydo: {e}");
            1
        }
    }
}

fn update(
    a: &Args,
    source: Option<&Source>,
    tmp: &Path,
    out: &mut dyn std::io::Write,
) -> Result<i32, String> {
    let targets = if a.w.named() {
        a.w.targets().map_err(|e| e.0)?
    } else {
        hydo_agents::installed_copies().map_err(|e| e.0)?
    };
    if targets.is_empty() {
        return Err(format!(
            "no copy of the {SKILL} skill in this repository's or your home's .claude/skills or \
             .agents/skills; install one with `hydo skill install --agent <tool>` (or name one \
             with --agent or --to). In Claude Code, the hydo plugin is updated by Claude Code's \
             plugin updates instead"
        ));
    }
    for t in &targets {
        hydo_agents::replaceable(t, a.w.force).map_err(|e| e.0)?;
    }
    let source =
        match source {
            Some(s) if !a.offline => s,
            _ => return Err(
                "hydo skill update needs the network to fetch the skill release, and it is off \
                 (--offline or HYDO_OFFLINE); `hydo skill install` installs the skill this hydo \
                 carries without the network"
                    .into(),
            ),
        };
    let wanted = match &a.version {
        Some(v) => v.clone(),
        None => source.newest()?,
    };
    let say = |out: &mut dyn std::io::Write, s: String| {
        let _ = writeln!(out, "{s}");
    };
    if a.check {
        let mut behind = false;
        for t in &targets {
            match version_at(t) {
                None => {
                    behind = true;
                    say(
                        out,
                        format!("{}: not installed (release {wanted})", t.display()),
                    );
                }
                Some(v) if version_lt(&v, &wanted) => {
                    behind = true;
                    say(
                        out,
                        format!("{}: {SKILL} {v}, release {wanted} is newer", t.display()),
                    );
                }
                Some(v) => say(
                    out,
                    format!(
                        "{}: {SKILL} {v}, up to date (release {wanted})",
                        t.display()
                    ),
                ),
            }
        }
        return Ok(if behind { 3 } else { 0 });
    }
    // which targets change: an older copy (or none); a newer one only when a version was asked
    let mut todo = Vec::new();
    for t in &targets {
        match version_at(t) {
            Some(v) if v == wanted && !a.w.force => say(
                out,
                format!("hydo: {} is already at {SKILL} {v}", t.display()),
            ),
            Some(v) if version_lt(&wanted, &v) && a.version.is_none() && !a.w.force => say(
                out,
                format!(
                    "hydo: {} has {SKILL} {v}, newer than the newest release {wanted}; kept \
                     (--version {wanted} installs that release)",
                    t.display()
                ),
            ),
            v => todo.push((t.clone(), v)),
        }
    }
    if todo.is_empty() {
        return Ok(0);
    }
    let work = tmp.join(format!("hydo-skill-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let r = (|| -> Result<(), String> {
        let src = source.get(&wanted, &work)?;
        for (t, before) in &todo {
            let verb = replace_dir(&src, t, a.w.force).map_err(|e| e.0)?;
            let was = before
                .as_ref()
                .map(|v| format!("{v} -> "))
                .unwrap_or_default();
            say(
                out,
                format!("hydo: {verb} {} ({SKILL} {was}{wanted})", t.display()),
            );
            if let Some(who) = readers_of(t) {
                say(out, format!("      read by {who}"));
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    r.map(|_| 0)
}

/// `hydo skill update`.
pub fn cmd_skill_update(ctx: &Ctx, argv: &[String]) -> i32 {
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return 0;
    }
    let c = &ctx.config.cli.cache;
    let offline = c.offline || argv.iter().any(|a| a == "--offline");
    let source = if offline {
        None
    } else {
        match Source::new(&c.release_api, &c.release_url, Options::standard) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("hydo: {e}");
                return 1;
            }
        }
    };
    let mut so = std::io::stdout().lock();
    let mut se = std::io::stderr().lock();
    run_with(
        argv,
        source.as_ref(),
        &std::env::temp_dir(),
        &mut so,
        &mut se,
    )
}

#[cfg(test)]
mod tests;
