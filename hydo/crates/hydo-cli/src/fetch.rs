//! HTTP downloads for the install cache (Milestone 162, docs/design/install-cache.md): a file
//! from a release, written to a part file, resumed with a Range request after an interruption,
//! retried a few times, and accepted only with the sha256 the caller expects.
//!
//! The client is ureq with rustls and the compiled-in Mozilla roots (the workspace's registry
//! pulls use the same). Proxies come from the standard variables (`HTTPS_PROXY`, `HTTP_PROXY`,
//! `ALL_PROXY`, `NO_PROXY`), read by ureq when the program builds its [`Options`]. A base URL must
//! be `https://`; plain `http://` is accepted only for a loopback host (tests, a mirror on the
//! same machine), and an HTTPS download never follows a redirect to plain HTTP.

use std::io::{Read, Seek, Write};
use std::path::Path;
use std::time::Duration;

/// How downloads are made.
#[derive(Clone, Debug)]
pub struct Options {
    /// Releases are at `<base>/v<version>/<file>`.
    pub base: String,
    /// The proxy (from the environment, `ureq::Proxy::try_from_env`), or none.
    pub proxy: Option<ureq::Proxy>,
    /// Attempts per file (at least 1).
    pub attempts: u32,
    /// The pause before the second attempt; it doubles after each further one.
    pub pause: Duration,
    /// Print a line per fetch on stderr.
    pub verbose: bool,
}

/// The largest file hydo fetches.
pub const MAX_BYTES: u64 = 4 << 30;

impl Options {
    /// The `hydo` command's: the proxy from the environment, four attempts, 1 s, 2 s and 4 s
    /// apart.
    pub fn standard(base: &str) -> Options {
        Options {
            base: base.trim_end_matches('/').to_string(),
            proxy: ureq::Proxy::try_from_env(),
            attempts: 4,
            pause: Duration::from_secs(1),
            verbose: true,
        }
    }
}

/// Whether a URL is plain HTTP to the loopback.
fn loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let auth = rest.split('/').next().unwrap_or("");
    let host = if let Some(h) = auth.strip_prefix('[') {
        h.split(']').next().unwrap_or("")
    } else {
        auth.split(':').next().unwrap_or("")
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// Check a base URL: `https://`, or `http://` to a loopback host. Ok(true): HTTPS.
pub fn check_base(url: &str) -> Result<bool, String> {
    if url.starts_with("https://") && url.len() > "https://".len() {
        return Ok(true);
    }
    if loopback_http(url) {
        return Ok(false);
    }
    Err(format!(
        "the release URL {url} is not https:// (plain http:// is accepted only for a loopback \
         host): the release's SHA256SUMS is trusted only through TLS"
    ))
}

/// A downloader for one base URL.
pub struct Fetcher {
    agent: ureq::Agent,
    pub opts: Options,
}

/// The size in words (`59.6 MB`).
pub fn size_text(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1} MB", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1} kB", n as f64 / 1e3)
    } else {
        format!("{n} bytes")
    }
}

fn retryable(status: u16) -> bool {
    status == 408 || status == 429 || status >= 500
}

/// What one attempt ended with.
enum Attempt {
    /// The part file holds the whole body.
    Done,
    /// Worth another attempt (the reason).
    Again(String),
    /// Not worth another attempt.
    Fail(String),
}

impl Fetcher {
    pub fn new(opts: Options) -> Result<Fetcher, String> {
        let https = check_base(&opts.base)?;
        let cfg = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(https)
            .proxy(opts.proxy.clone())
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_per_read(Some(Duration::from_secs(60)))
            .user_agent(format!("hydo/{}", hydo_image::hydo_version()))
            .build();
        Ok(Fetcher {
            agent: cfg.into(),
            opts,
        })
    }

    fn pause(&self, attempt: u32) {
        let p = self.opts.pause * (1u32 << (attempt.saturating_sub(1)).min(10));
        if !p.is_zero() {
            std::thread::sleep(p);
        }
    }

    /// A small file (at most `limit` bytes), with retries.
    pub fn get_small(&self, url: &str, limit: u64) -> Result<Vec<u8>, String> {
        let mut last = String::new();
        for attempt in 1..=self.opts.attempts.max(1) {
            if attempt > 1 {
                self.pause(attempt - 1);
            }
            match self.agent.get(url).call() {
                Err(e) => last = e.to_string(),
                Ok(mut r) => {
                    let st = r.status().as_u16();
                    if st == 200 {
                        match r.body_mut().with_config().limit(limit).read_to_vec() {
                            Ok(b) => return Ok(b),
                            Err(e) => last = e.to_string(),
                        }
                    } else if retryable(st) {
                        last = format!("HTTP {st}");
                    } else {
                        return Err(format!("{url}: HTTP {st}"));
                    }
                }
            }
        }
        Err(format!(
            "{url}: {last} (after {} attempts)",
            self.opts.attempts.max(1)
        ))
    }

    /// Download `url` into `part` until the whole file is there and its sha256 is `sha256`.
    /// An existing `part` is resumed. On success `part` holds the verified file; on failure it
    /// holds what may be resumed later (nothing after a sha256 mismatch).
    pub fn download(&self, url: &str, part: &Path, sha256: &str, why: &str) -> Result<(), String> {
        if let Some(d) = part.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let attempts = self.opts.attempts.max(1);
        let mut last = String::new();
        let mut announced = false;
        for attempt in 1..=attempts {
            if attempt > 1 {
                self.pause(attempt - 1);
            }
            let have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
            match self.attempt(url, part, have, &mut announced, why) {
                Attempt::Fail(e) => return Err(e),
                Attempt::Again(e) => {
                    last = e;
                    continue;
                }
                Attempt::Done => {}
            }
            let got = hydo_image::store::sha256_file_now(part)
                .map_err(|e| format!("{}: {e}", part.display()))?;
            if got == sha256 {
                return Ok(());
            }
            let _ = std::fs::remove_file(part);
            let msg = format!(
                "{url}: the download has sha256 {got}, not the expected {sha256}; refused and \
                 deleted"
            );
            if have > 0 && attempt < attempts {
                // the part kept from an earlier attempt may have been the bad half: once more
                // from the start
                last = msg;
                continue;
            }
            return Err(msg);
        }
        Err(format!("{url}: {last} (after {attempts} attempts)"))
    }

    fn attempt(
        &self,
        url: &str,
        part: &Path,
        have: u64,
        announced: &mut bool,
        why: &str,
    ) -> Attempt {
        let mut req = self.agent.get(url);
        if have > 0 {
            req = req.header("Range", format!("bytes={have}-"));
        }
        let mut r = match req.call() {
            Ok(r) => r,
            Err(e) => return Attempt::Again(e.to_string()),
        };
        let st = r.status().as_u16();
        let header = |n: &str| {
            r.headers()
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let append = match st {
            200 => false,
            206 if have > 0 => {
                let cr = header("content-range").unwrap_or_default();
                if !cr.starts_with(&format!("bytes {have}-")) {
                    // not the range asked for: start again without a range
                    let _ = std::fs::remove_file(part);
                    return Attempt::Again(format!("HTTP 206 with Content-Range {cr:?}"));
                }
                true
            }
            416 if have > 0 => return Attempt::Done,
            s if retryable(s) => return Attempt::Again(format!("HTTP {s}")),
            s => return Attempt::Fail(format!("{url}: HTTP {s}")),
        };
        let len = r.body().content_length();
        if self.opts.verbose && !*announced {
            *announced = true;
            let total = len.map(|n| if append { n + have } else { n });
            let size = total
                .map(|n| format!(" ({})", size_text(n)))
                .unwrap_or_default();
            let resumed = if append {
                format!(", resuming at {}", size_text(have))
            } else {
                String::new()
            };
            eprintln!("hydo: fetching {url}{size} for {why}{resumed}");
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(part);
        let mut f = match f {
            Ok(f) => f,
            Err(e) => return Attempt::Fail(format!("{}: {e}", part.display())),
        };
        let start = if append { have } else { 0 };
        if let Err(e) = f
            .set_len(start)
            .and_then(|_| f.seek(std::io::SeekFrom::Start(start)).map(|_| ()))
        {
            return Attempt::Fail(format!("{}: {e}", part.display()));
        }
        let mut body = r
            .body_mut()
            .with_config()
            .limit(MAX_BYTES.saturating_sub(start))
            .reader();
        let mut buf = vec![0u8; 1 << 16];
        let mut n_read: u64 = 0;
        loop {
            match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    n_read += n as u64;
                    if let Err(e) = f.write_all(&buf[..n]) {
                        return Attempt::Fail(format!("{}: {e}", part.display()));
                    }
                }
                Err(e) => {
                    let _ = f.flush();
                    return Attempt::Again(format!(
                        "the download stopped after {} of this attempt: {e}",
                        size_text(n_read)
                    ));
                }
            }
        }
        if let Some(l) = len {
            if n_read < l {
                return Attempt::Again(format!(
                    "the download is short ({} of {})",
                    size_text(n_read),
                    size_text(l)
                ));
            }
        }
        if let Err(e) = f.sync_all() {
            return Attempt::Fail(format!("{}: {e}", part.display()));
        }
        Attempt::Done
    }
}

/// `<base>/v<version>/<file>`.
pub fn release_url(base: &str, version: &str, file: &str) -> String {
    format!("{}/v{version}/{file}", base.trim_end_matches('/'))
}

/// Lower-case hex SHA-256 of bytes.
#[cfg(test)]
pub fn sha256_bytes(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hydo_image::store::hex(&Sha256::digest(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls() {
        assert_eq!(check_base("https://example.org/r"), Ok(true));
        assert_eq!(check_base("http://127.0.0.1:8080/r"), Ok(false));
        assert_eq!(check_base("http://localhost/r"), Ok(false));
        assert_eq!(check_base("http://[::1]:9/r"), Ok(false));
        for bad in [
            "http://example.org/r",
            "http://127.0.0.1.example.org/r",
            "ftp://x/y",
            "https://",
            "file:///tmp/x",
        ] {
            assert!(check_base(bad).is_err(), "{bad}");
        }
        assert_eq!(
            release_url("https://h/x/", "0.1.0", "SHA256SUMS"),
            "https://h/x/v0.1.0/SHA256SUMS"
        );
        assert_eq!(size_text(59_563_571), "59.6 MB");
    }
}
