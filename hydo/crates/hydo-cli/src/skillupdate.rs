//! `hydo skill update [--agent NAME ...] [--to DIR] [--user] [--force] [--version V] [--check]
//! [--offline]` (Milestone 170): install the onboarding skill of the newest hydo release.
//!
//! Since Milestone 332 the skill has hydo's version and ships in hydo's own release
//! (docs/RELEASING.md, "The onboarding skill"). The newest release is the highest `v<x>.<y>.<z>`
//! tag in the project's release list (`HYDO_RELEASE_API`, GitHub's API; drafts and pre-releases
//! are skipped), the list `hydo update` reads. Its two assets come from
//! `<HYDO_RELEASE_URL>/v<version>/`: `SHA256SUMS`, trusted through TLS as the install cache
//! trusts a release's own sums (Milestone 162), and `hydo-skill-<version>.tar.gz`, accepted only
//! with the sha256 that file names. A copy of the skill's old version line (1.0.0 to 2.4.0, the
//! releases `skill-v<version>` up to hydo 0.4.1) counts as older than any version with hydo's
//! numbers ([`hydo_agents::release::skill_lt`]). The archive is unpacked by `hydo_agents::release::unpack`
//! (plain files and directories below `hydo/`, the plugin folder, only) into a temporary
//! directory, and each target folder is replaced in one step by `hydo_agents::replace_dir`: a
//! Claude Code copy (`.claude/skills/hydo`) by the plugin folder, any other (`onboard`) by the
//! skill folder in it. Nothing outside the target folders is written, except two short-lived
//! siblings `.<name>.new-<pid>` and `.<name>.old-<pid>` that are deleted again. The command uses
//! the network only when the user runs it; nothing it fetches is executed.
//!
//! The release list is read once, with a GitHub token when one is known, or taken from the
//! answer `hydo update` or `hydo doctor` kept in the last 10 minutes ([`crate::releaselist`],
//! Milestone 322). When it cannot be read, also at GitHub's rate limit, the exit status is 4.

use crate::ctx::Ctx;
use crate::fetch::{Fetcher, Options};
use crate::releaselist::{self, Auth};
use hydo_agents::release::{archive_name, skill_lt, unpack, FIRST_IN_HYDO};
use hydo_agents::{readers_of, replace_dir, version_at, version_lt, Where, PLUGIN, SKILL};
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

/// Where the skill releases come from, and how they are fetched.
pub struct Source {
    pub api: String,
    pub base: String,
    pub api_fetch: Fetcher,
    pub fetch: Fetcher,
    /// The token for GitHub's API (Milestone 322); [`Auth::none`] by default.
    pub auth: Auth,
    /// The kept answer of the release list (`~/.hydo/update-check.json`); none by default.
    pub cache: Option<PathBuf>,
}

impl Source {
    pub fn new(api: &str, base: &str, opts: impl Fn(&str) -> Options) -> Result<Source, String> {
        Ok(Source {
            api: api.trim_end_matches('/').to_string(),
            base: base.trim_end_matches('/').to_string(),
            api_fetch: Fetcher::new(opts(api))?,
            fetch: Fetcher::new(opts(base))?,
            auth: Auth::none(),
            cache: None,
        })
    }

    fn url(&self, version: &str, file: &str) -> String {
        crate::fetch::release_url(&self.base, version, file)
    }

    /// The release list: one request, or a kept answer of the last minutes.
    pub fn list(&self, now: u64) -> Result<releaselist::List, releaselist::Fail> {
        releaselist::read(
            &self.api,
            &self.api_fetch,
            &self.auth,
            self.cache.as_deref(),
            now,
        )
    }

    /// Download release `version`, check it by sha256 and unpack its plugin folder into `dir/hydo`.
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
            .ok_or_else(|| {
                if version_lt(version, FIRST_IN_HYDO) {
                    format!(
                        "hydo {version}'s release carries no onboarding skill ({sums_url} does \
                         not list {name}); hydo's releases carry it from {FIRST_IN_HYDO} on"
                    )
                } else {
                    format!("{sums_url} does not list {name}")
                }
            })?;
        let part = dir.join(format!("{name}.part"));
        self.fetch.download(
            &self.url(version, &name),
            &part,
            &sha,
            &format!("the onboarding skill {version}"),
        )?;
        let bytes = std::fs::read(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let out = dir.join(PLUGIN);
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
    match update(&a, source, tmp, out, err) {
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
    err: &mut dyn std::io::Write,
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
             with --agent or --to). A plugin installed from hydo's marketplace in Claude Code is \
             updated by Claude Code's plugin updates instead"
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
        None => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let list = match source.list(now) {
                Ok(l) => l,
                Err(f) => {
                    let _ = writeln!(err, "hydo: {}", f.line(now));
                    return Ok(releaselist::COULD_NOT_CHECK);
                }
            };
            if let Some(l) = list.kept_line(source.cache.as_deref()) {
                let _ = writeln!(out, "hydo: {l}");
            }
            list.skill().ok_or_else(|| {
                format!(
                    "{}: no hydo release (no tag v<x>.<y>.<z>) in the project's releases",
                    source.api
                )
            })?
        }
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
                Some(v) if skill_lt(&v, &wanted) => {
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
            Some(v) if skill_lt(&wanted, &v) && a.version.is_none() && !a.w.force => say(
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
    // Milestone 315: every copy this command saw is registered, so `hydo update` finds it from
    // anywhere (also a copy made before the registry existed)
    for t in &targets {
        if let Some(v) = version_at(t).filter(|_| todo.iter().all(|(x, _)| x != t)) {
            hydo_agents::copies::record(t, &v);
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
            hydo_agents::copies::record(t, &wanted);
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
            Ok(mut s) => {
                s.auth = Auth::from_config(c, std::time::Duration::from_secs(5));
                s.cache = Some(c.home.join(releaselist::CHECK_FILE));
                Some(s)
            }
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
