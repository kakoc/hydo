//! The project's release list from GitHub's API (Milestone 322), as `hydo update`, `hydo skill
//! update` and `hydo doctor`'s look for a newer release read it.
//!
//! - One request per command. The newest hydo release (`v<x>.<y>.<z>`) and the newest skill
//!   release (`skill-v<version>`) come from the same list, and the answer is kept in
//!   `~/.hydo/update-check.json` ([`CHECK_FILE`]). A command takes a kept answer of the last
//!   [`FRESH`] seconds without a request; `hydo doctor` takes one of the last day.
//! - A token. `GH_TOKEN`, else `GITHUB_TOKEN`, else the output of `gh auth token --hostname
//!   github.com` when `gh` is on `PATH` (asked only when a request needs it, with a timeout, and
//!   read through a temporary file of mode 0600, not a pipe: Milestone 321 explains why a pipe
//!   can wait for another thread's child). It goes as `Authorization: Bearer` to
//!   `https://api.github.com` only ([`token_may_go`]), never to another host and never along a
//!   redirect. hydo never prints, logs or writes it.
//! - GitHub's limit. An answer 403 or 429 with `x-ratelimit-remaining: 0` is [`Fail::Limited`]:
//!   the commands say in one line that the limit was reached, when it resets, and how to lift
//!   it, and exit with [`COULD_NOT_CHECK`]. `hydo doctor` says nothing, keeps the earlier answer,
//!   and does not look again before the limit resets.

use crate::fetch::{Fetcher, GetError};
use crate::update::{hydo_releases, Release};
use hydo_config::run::Secret;
use std::path::Path;
use std::time::Duration;

/// The exit status of `hydo update` and `hydo skill update` when the release list could not be
/// read (3 is "something is newer").
pub const COULD_NOT_CHECK: i32 = 4;

/// The file of the kept answer, in the install cache's folder.
pub const CHECK_FILE: &str = "update-check.json";

/// The only origin a token goes to.
pub const GITHUB_API: &str = "https://api.github.com";

/// How long a command takes a kept answer without a request, in seconds.
pub const FRESH: u64 = 600;

/// How long `hydo doctor` takes a kept answer, in seconds.
pub const DAY: u64 = 24 * 3600;

/// What the release list says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct List {
    /// The hydo releases, oldest first.
    pub hydo: Vec<Release>,
    /// The newest skill release.
    pub skill: Option<String>,
    /// For a kept answer: how many seconds ago it was read.
    pub age: Option<u64>,
}

impl List {
    pub fn parse(body: &[u8]) -> Result<List, String> {
        Ok(List {
            hydo: hydo_releases(body)?,
            skill: crate::skillupdate::newest_in(body)?,
            age: None,
        })
    }

    /// The line that says a kept answer was used, if it was.
    pub fn kept_line(&self, file: Option<&Path>) -> Option<String> {
        let age = self.age?;
        let when = match age / 60 {
            0 => "less than a minute ago".to_string(),
            1 => "1 minute ago".to_string(),
            m => format!("{m} minutes ago"),
        };
        let file = file.map(|f| format!(", kept in {}", f.display()));
        Some(format!(
            "the release list as read {when}{}; it is read again after {} minutes",
            file.unwrap_or_default(),
            FRESH / 60
        ))
    }
}

/// Why the release list could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// GitHub's rate limit: when it resets (Unix seconds), and whether the request had a token.
    Limited { reset: Option<u64>, login: bool },
    /// Anything else, in words.
    Other(String),
}

/// `in 38 minutes`, from the reset time.
fn resets_in(reset: Option<u64>, now: u64) -> String {
    let Some(r) = reset else {
        return "within an hour".into();
    };
    match r.saturating_sub(now).div_ceil(60) {
        0 => "now".into(),
        1 => "in 1 minute".into(),
        m => format!("in {m} minutes"),
    }
}

impl Fail {
    /// The one line that says what happened and what to do.
    pub fn line(&self, now: u64) -> String {
        match self {
            Fail::Limited {
                reset,
                login: false,
            } => format!(
                "GitHub's limit for requests without a login (60 an hour from one address) was \
                 reached, so the release list could not be read; the limit resets {}. Set \
                 GH_TOKEN to a GitHub token, or log in with `gh auth login`, to lift it. Nothing \
                 was changed",
                resets_in(*reset, now)
            ),
            Fail::Limited { reset, login: true } => format!(
                "GitHub's limit for requests with your token was reached, so the release list \
                 could not be read; the limit resets {}. Nothing was changed",
                resets_in(*reset, now)
            ),
            Fail::Other(e) => format!("cannot read the release list: {e}; nothing was changed"),
        }
    }
}

/// `scheme://authority` of a URL (None: no `://`).
fn origin(url: &str) -> Option<&str> {
    let i = url.find("://")? + 3;
    let end = url[i..]
        .find(['/', '?', '#'])
        .map(|j| i + j)
        .unwrap_or(url.len());
    Some(&url[..end])
}

/// Whether a request to `url` may carry the token: its origin is exactly [`GITHUB_API`], or
/// the origin `standin` names when that is plain HTTP to a loopback host (the tests).
pub fn token_may_go(url: &str, standin: Option<&str>) -> bool {
    let Some(o) = origin(url) else {
        return false;
    };
    if o.eq_ignore_ascii_case(GITHUB_API) {
        return true;
    }
    standin
        .filter(|s| crate::fetch::loopback_http(s))
        .and_then(origin)
        .is_some_and(|s| s.eq_ignore_ascii_case(o))
}

/// Where the token comes from.
pub struct Auth {
    /// `GH_TOKEN` or `GITHUB_TOKEN`.
    env: Option<Secret>,
    /// `HYDO_GITHUB_API_STANDIN`.
    standin: Option<String>,
    /// How long `gh auth token` may take.
    gh_wait: Duration,
    /// The answer of `gh`, asked once.
    gh: std::cell::OnceCell<Option<Secret>>,
}

impl Auth {
    /// No token at all (tests).
    pub fn none() -> Auth {
        Auth {
            env: None,
            standin: None,
            gh_wait: Duration::ZERO,
            gh: std::cell::OnceCell::from(None),
        }
    }

    /// From the configuration; `gh auth token` may take `gh_wait`.
    pub fn from_config(c: &hydo_config::run::Cache, gh_wait: Duration) -> Auth {
        Auth {
            env: c.github_token.clone(),
            standin: c.github_api_standin.clone(),
            gh_wait,
            gh: std::cell::OnceCell::new(),
        }
    }

    /// The token for a request to `url`, or None (none known, or `url` may not have one).
    pub fn for_url(&self, url: &str) -> Option<Secret> {
        if !token_may_go(url, self.standin.as_deref()) {
            return None;
        }
        if let Some(t) = &self.env {
            return Some(t.clone());
        }
        self.gh
            .get_or_init(|| crate::ctx::which("gh").and_then(|gh| gh_token(&gh, self.gh_wait)))
            .clone()
    }
}

/// The output of `<gh> auth token --hostname github.com`, waited for at most `wait`. The
/// output goes to a new file of mode 0600 that is removed at once; a program that does not end
/// in time is killed. None when it fails, prints nothing, or prints something that is no token.
pub fn gh_token(gh: &Path, wait: Duration) -> Option<Secret> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "hydo-gh-{}-{}.out",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .ok()?;
    let child = std::process::Command::new(gh)
        .args(["auth", "token", "--hostname", "github.com"])
        .stdin(std::process::Stdio::null())
        .stdout(file)
        .stderr(std::process::Stdio::null())
        .spawn();
    let ok = match child {
        Err(_) => false,
        Ok(mut c) => {
            let until = std::time::Instant::now() + wait;
            loop {
                match c.try_wait() {
                    Ok(Some(st)) => break st.success(),
                    Ok(None) if std::time::Instant::now() < until => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    _ => {
                        let _ = c.kill();
                        let _ = c.wait();
                        break false;
                    }
                }
            }
        }
    };
    let out = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    if !ok {
        return None;
    }
    let t = String::from_utf8(out.ok()?).ok()?;
    let t = t.trim();
    let plausible = !t.is_empty() && t.len() <= 512 && t.bytes().all(|b| b.is_ascii_graphic());
    plausible.then(|| Secret::new(t))
}

fn load(file: &Path) -> Option<serde_json::Value> {
    std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

fn store(file: &Path, doc: &serde_json::Value) {
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = file.with_extension(format!("tmp-{}", std::process::id()));
    if std::fs::write(&tmp, doc.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, file);
    }
}

/// The kept document of a list that was read at `at`.
fn doc_of(api: &str, at: u64, l: &List) -> serde_json::Value {
    let releases: Vec<serde_json::Value> = l
        .hydo
        .iter()
        .map(|r| serde_json::json!({"version": r.version, "upgrade": r.upgrade}))
        .collect();
    serde_json::json!({
        "checked": at,
        "api": api,
        "newest": l.hydo.last().map(|r| r.version.clone()),
        "skill": l.skill,
        "releases": releases,
    })
}

/// A kept answer for `api` younger than `max_age` seconds at `now`, as a list. A kept failed
/// look, or one an older hydo wrote (without `releases`), is no list.
fn kept_list(k: &serde_json::Value, api: &str, now: u64, max_age: u64) -> Option<List> {
    let checked = k["checked"].as_u64()?;
    if k["api"].as_str() != Some(api) || checked > now || now - checked >= max_age {
        return None;
    }
    let hydo = k["releases"]
        .as_array()?
        .iter()
        .map(|r| {
            Some(Release {
                version: r["version"].as_str()?.to_string(),
                upgrade: r["upgrade"].as_str().map(str::to_string),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(List {
        hydo,
        skill: k["skill"].as_str().map(str::to_string),
        age: Some(now - checked),
    })
}

/// Keep that GitHub's limit was reached for `api` until `reset` (an hour when GitHub did not
/// say), with the earlier answer as it was.
fn mark_limited(file: &Path, api: &str, now: u64, reset: Option<u64>) {
    let mut doc = load(file)
        .filter(|k| k["api"].as_str() == Some(api))
        .unwrap_or_else(|| serde_json::json!({"api": api}));
    doc["limited_until"] = serde_json::json!(reset.unwrap_or(now + 3600).max(now));
    store(file, &doc);
}

/// The release list of `api` for a command: a kept answer of the last [`FRESH`] seconds, else
/// one request (with the token where it may go), whose answer is kept in `cache`.
pub fn read(
    api: &str,
    fetch: &Fetcher,
    auth: &Auth,
    cache: Option<&Path>,
    now: u64,
) -> Result<List, Fail> {
    if let Some(l) = cache
        .and_then(load)
        .and_then(|k| kept_list(&k, api, now, FRESH))
    {
        return Ok(l);
    }
    let url = format!("{}?per_page=100", api.trim_end_matches('/'));
    let token = auth.for_url(&url);
    match fetch.get_api(&url, 8 << 20, token.as_ref().map(Secret::expose)) {
        Ok(body) => {
            let l = List::parse(&body).map_err(Fail::Other)?;
            if let Some(f) = cache {
                store(f, &doc_of(api, now, &l));
            }
            Ok(l)
        }
        Err(GetError::Limited { reset, .. }) => {
            if let Some(f) = cache {
                mark_limited(f, api, now, reset);
            }
            Err(Fail::Limited {
                reset,
                login: token.is_some(),
            })
        }
        Err(GetError::Failed(e)) => Err(Fail::Other(e)),
    }
}

/// `hydo doctor`'s look: the newest hydo release of `api`, from a kept answer of the last day,
/// else from one look (`fetch` gets the list's URL and makes the request). A failed look is
/// kept as no answer for a day, so an offline machine looks once a day only. At GitHub's limit
/// the earlier answer stays as it was, the look gives no answer, and no look is made before
/// the limit resets.
pub fn doctor_newest(
    file: &Path,
    api: &str,
    at: u64,
    fetch: &dyn Fn(&str) -> Result<Vec<u8>, GetError>,
) -> Option<String> {
    let kept = load(file).filter(|k| k["api"].as_str() == Some(api));
    if let Some(k) = &kept {
        if k["limited_until"].as_u64().is_some_and(|u| at < u) {
            return None;
        }
        let checked = k["checked"].as_u64().unwrap_or(0);
        if k.get("checked").is_some() && checked <= at && at - checked < DAY {
            return k["newest"].as_str().map(str::to_string);
        }
    }
    match fetch(&format!("{}?per_page=100", api.trim_end_matches('/'))) {
        Ok(body) => match List::parse(&body) {
            Ok(l) => {
                store(file, &doc_of(api, at, &l));
                l.hydo.last().map(|r| r.version.clone())
            }
            Err(_) => {
                store(
                    file,
                    &serde_json::json!({"checked": at, "api": api, "newest": null}),
                );
                None
            }
        },
        Err(GetError::Limited { reset, .. }) => {
            mark_limited(file, api, at, reset);
            None
        }
        Err(GetError::Failed(_)) => {
            store(
                file,
                &serde_json::json!({"checked": at, "api": api, "newest": null}),
            );
            None
        }
    }
}

#[cfg(test)]
mod tests;
