//! Registry credentials, found where Docker finds them (Milestone 320).
//!
//! `docker login` keeps a registry's credential in Docker's config file,
//! `$DOCKER_CONFIG/config.json` (default `~/.docker/config.json`): in `auths` (a base64
//! `user:password` in `auth`, or an `identitytoken`), or in a credential helper program that
//! `credHelpers` names for one registry and `credsStore` names for every other one. A helper is
//! the program `docker-credential-<name>`; `get` with the registry's server address on stdin
//! answers `{"ServerURL", "Username", "Secret"}` (the protocol of docker-credential-helpers;
//! a `Username` of `<token>` makes the secret an identity token). [`lookup`] follows Docker's
//! order: a `credHelpers` entry for the registry, else `credsStore`, else `auths`.
//!
//! A credential is asked for one registry only and is used only for that registry's requests and
//! for the token service its challenge names ([`crate::registry::Http`]). Its secret is never
//! printed, logged, or written: [`Credential`]'s `Debug` hides it, and [`Found::how`] says only
//! where hydo looked and what it found there.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Where to look: Docker's config folder (`DOCKER_CONFIG`, else `~/.docker`; None: none known)
/// and the `PATH` that the helper programs are found on (None: this process's).
#[derive(Clone, Debug, Default)]
pub struct DockerConfig {
    pub dir: Option<PathBuf>,
    pub path: Option<OsString>,
}

impl DockerConfig {
    /// The folder `dir` and this process's `PATH`.
    pub fn at(dir: impl Into<PathBuf>) -> DockerConfig {
        DockerConfig {
            dir: Some(dir.into()),
            path: None,
        }
    }

    fn file(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("config.json"))
    }
}

/// A registry credential. `Debug` never shows the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential {
    pub username: String,
    pub secret: String,
    /// The secret is an identity token (a refresh token for the registry's token service), not
    /// a password.
    pub identity_token: bool,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("username", &self.username)
            .field("secret", &"<hidden>")
            .field("identity_token", &self.identity_token)
            .finish()
    }
}

impl Credential {
    /// The value of an `Authorization: Basic` header.
    pub fn basic(&self) -> String {
        format!(
            "Basic {}",
            base64_encode(format!("{}:{}", self.username, self.secret).as_bytes())
        )
    }
}

/// What [`lookup`] found: the credential, if any, and where hydo looked, in words without any
/// secret (for messages).
#[derive(Clone, Debug)]
pub struct Found {
    pub cred: Option<Credential>,
    pub how: String,
}

/// The server address Docker keeps a registry's credential under: Docker Hub's is
/// `https://index.docker.io/v1/`, every other registry's its host (with its port).
pub fn server_address(registry: &str) -> String {
    if is_docker_hub(registry) {
        "https://index.docker.io/v1/".into()
    } else {
        registry.to_string()
    }
}

fn is_docker_hub(h: &str) -> bool {
    matches!(
        h,
        "docker.io" | "index.docker.io" | "registry-1.docker.io" | "registry.hub.docker.com"
    )
}

/// Docker's `ConvertToHostname`: a key of `auths` or `credHelpers` without its scheme and path
/// (`https://index.docker.io/v1/` is `index.docker.io`), with Docker Hub's names made one.
fn hostname(key: &str) -> String {
    let k = key
        .strip_prefix("https://")
        .or_else(|| key.strip_prefix("http://"))
        .unwrap_or(key);
    let h = k.split('/').next().unwrap_or(k).to_ascii_lowercase();
    if is_docker_hub(&h) {
        "index.docker.io".into()
    } else {
        h
    }
}

/// The value of a map whose key names `registry` (an exact key first, as Docker looks).
fn entry<'a>(
    m: &'a serde_json::Map<String, serde_json::Value>,
    registry: &str,
) -> Option<(&'a String, &'a serde_json::Value)> {
    let want = hostname(&server_address(registry));
    m.iter()
        .find(|(k, _)| **k == server_address(registry) || **k == registry)
        .or_else(|| m.iter().find(|(k, _)| hostname(k) == want))
}

/// The credential of `registry` (`docker.io`, `ghcr.io`, `localhost:5000`, ...), looked for as
/// Docker looks for it. Nothing is printed.
pub fn lookup(cfg: &DockerConfig, registry: &str) -> Found {
    let none = |how: String| Found { cred: None, how };
    let Some(file) = cfg.file() else {
        return none(
            "no Docker config folder is known (DOCKER_CONFIG is not set and there is no home folder)"
                .into(),
        );
    };
    let f = file.display();
    let text = match std::fs::read(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return none(format!(
                "{f} does not exist (`docker login` writes it; DOCKER_CONFIG names another folder)"
            ))
        }
        Err(e) => return none(format!("{f} could not be read ({})", e.kind())),
    };
    let doc: serde_json::Value = match serde_json::from_slice(&text) {
        Ok(d) => d,
        // the parser's message can quote the file's text, which may hold a secret
        Err(_) => return none(format!("{f} is not valid JSON")),
    };
    let empty = serde_json::Map::new();
    let obj = |k: &str| doc.get(k).and_then(|v| v.as_object()).unwrap_or(&empty);
    let host = server_address(registry);
    if let Some((key, h)) = entry(obj("credHelpers"), registry) {
        if let Some(name) = h.as_str().filter(|n| !n.is_empty()) {
            return helper(cfg, name, &host, &format!("credHelpers[\"{key}\"] in {f}"));
        }
    }
    if let Some(name) = doc
        .get("credsStore")
        .and_then(|v| v.as_str())
        .filter(|n| !n.is_empty())
    {
        return helper(cfg, name, &host, &format!("credsStore in {f}"));
    }
    let Some((key, e)) = entry(obj("auths"), registry) else {
        return none(format!(
            "{f} has no credHelpers entry for {registry}, no credsStore, and no auths entry for {registry}"
        ));
    };
    let s = |k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (mut user, mut pass) = (s("username"), s("password"));
    let auth = s("auth");
    if !auth.is_empty() {
        let Some((u, p)) = base64_decode(&auth)
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| {
                t.split_once(':')
                    .map(|(u, p)| (u.to_string(), p.to_string()))
            })
        else {
            return none(format!(
                "the auths entry \"{key}\" in {f} has an `auth` value that is not base64 of user:password"
            ));
        };
        (user, pass) = (u, p);
    }
    let token = s("identitytoken");
    if !token.is_empty() {
        return Found {
            cred: Some(Credential {
                username: user,
                secret: token,
                identity_token: true,
            }),
            how: format!("the identity token of the auths entry \"{key}\" in {f}"),
        };
    }
    if user.is_empty() && pass.is_empty() {
        return none(format!(
            "the auths entry \"{key}\" in {f} holds no credential (a credsStore that `docker login` used may have been removed)"
        ));
    }
    Found {
        cred: Some(Credential {
            username: user,
            secret: pass,
            identity_token: false,
        }),
        how: format!("the auths entry \"{key}\" in {f}"),
    }
}

/// How long a credential helper may take (a helper may ask a cloud service for a token).
const HELPER_TIMEOUT: Duration = Duration::from_secs(60);

/// `docker-credential-<name> get` with `server` on stdin.
fn helper(cfg: &DockerConfig, name: &str, server: &str, from: &str) -> Found {
    let prog = format!("docker-credential-{name}");
    let what = format!("the credential helper {prog} ({from})");
    let none = |how: String| Found { cred: None, how };
    let mut cmd = Command::new(&prog);
    cmd.arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // a helper's own messages are not shown: hydo cannot tell what they contain
        .stderr(Stdio::null());
    if let Some(p) = &cfg.path {
        cmd.env("PATH", p);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return none(format!("{what} is not on PATH"))
        }
        Err(e) => return none(format!("{what} could not start ({})", e.kind())),
    };
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(server.as_bytes());
    }
    let mut out = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(o) = out.as_mut() {
            let _ = o.take(1 << 20).read_to_end(&mut b);
        }
        b
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() < HELPER_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let body = reader.join().unwrap_or_default();
    let Some(status) = status else {
        return none(format!(
            "{what} gave no answer in {} s and was stopped",
            HELPER_TIMEOUT.as_secs()
        ));
    };
    if !status.success() {
        // docker-credential-helpers answer "credentials not found in native keychain"
        if String::from_utf8_lossy(&body).contains("not found") {
            return none(format!("{what} has no credential for {server}"));
        }
        return none(format!(
            "{what} failed for {server} ({status}; its output is not shown, as it may hold a secret)"
        ));
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return none(format!("{what} answered something other than JSON")),
    };
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let (user, secret) = (s("Username"), s("Secret"));
    if secret.is_empty() {
        return none(format!("{what} has no credential for {server}"));
    }
    Found {
        cred: Some(Credential {
            identity_token: user == "<token>",
            username: user,
            secret,
        }),
        how: what,
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64_encode(b: &[u8]) -> String {
    let mut o = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                o.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                o.push('=');
            }
        }
    }
    o
}

/// Base64, standard or URL-safe, with or without padding (whitespace is skipped).
pub fn base64_decode(t: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in t.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------- the challenge

/// A registry's `WWW-Authenticate` challenge: its scheme (`Bearer`, `Basic`) and parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    pub scheme: String,
    pub params: Vec<(String, String)>,
}

impl Challenge {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(x, _)| x.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.as_str())
    }
}

/// The first challenge of a `WWW-Authenticate` header (RFC 7235): a scheme and
/// `key=value` or `key="quoted value"` parameters separated by commas. A quoted value may hold
/// commas, and `\"` an escaped quote (`scope="repository:a/b:pull,push"`).
pub fn parse_challenge(h: &str) -> Option<Challenge> {
    let h = h.trim();
    let (scheme, rest) = match h.find(char::is_whitespace) {
        Some(i) => (&h[..i], &h[i..]),
        None => (h, ""),
    };
    if scheme.is_empty() || scheme.contains('=') {
        return None;
    }
    let mut params = Vec::new();
    let b = rest.as_bytes();
    let mut i = 0;
    loop {
        while i < b.len() && (b[i] == b',' || b[i].is_ascii_whitespace()) {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let ks = i;
        while i < b.len() && b[i] != b'=' && b[i] != b',' && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = rest[ks..i].to_string();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            // a second challenge starts (`Basic realm=...`): only the first one is read
            break;
        }
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut val = Vec::new();
        if i < b.len() && b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 1;
                }
                val.push(b[i]);
                i += 1;
            }
            i += 1;
        } else {
            while i < b.len() && b[i] != b',' && !b[i].is_ascii_whitespace() {
                val.push(b[i]);
                i += 1;
            }
        }
        params.push((key, String::from_utf8_lossy(&val).into_owned()));
    }
    Some(Challenge {
        scheme: scheme.to_string(),
        params,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenges() {
        let c = parse_challenge(
            r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/redis:pull""#,
        )
        .unwrap();
        assert_eq!(c.scheme, "Bearer");
        assert_eq!(c.get("realm"), Some("https://auth.docker.io/token"));
        assert_eq!(c.get("service"), Some("registry.docker.io"));
        assert_eq!(c.get("scope"), Some("repository:library/redis:pull"));
        // a quoted value with commas and an escaped quote, spaces around the separators, and a
        // parameter without quotes
        let c = parse_challenge(
            r#"bearer  realm = "http://127.0.0.1:5/t" , scope="repository:a/b:pull,push", error=insufficient_scope, x="a\"b""#,
        )
        .unwrap();
        assert_eq!(c.scheme, "bearer");
        assert_eq!(c.get("Realm"), Some("http://127.0.0.1:5/t"));
        assert_eq!(c.get("scope"), Some("repository:a/b:pull,push"));
        assert_eq!(c.get("error"), Some("insufficient_scope"));
        assert_eq!(c.get("x"), Some("a\"b"));
        // a Basic challenge (as an htpasswd registry and some cloud registries answer)
        let c = parse_challenge(r#"Basic realm="Registry Realm""#).unwrap();
        assert_eq!(c.scheme, "Basic");
        assert_eq!(c.get("realm"), Some("Registry Realm"));
        // only the first of two challenges
        let c = parse_challenge(r#"Bearer realm="r",service="s", Basic realm="b""#).unwrap();
        assert_eq!(c.params.len(), 2);
        assert_eq!(c.get("service"), Some("s"));
        // a scheme alone; nothing
        assert_eq!(parse_challenge("Basic").unwrap().params, vec![]);
        assert!(parse_challenge("").is_none());
        assert!(parse_challenge("realm=x").is_none());
    }

    #[test]
    fn base64_both_ways() {
        for s in ["", "a", "ab", "abc", "user:pa ss:wörd"] {
            assert_eq!(
                base64_decode(&base64_encode(s.as_bytes())).unwrap(),
                s.as_bytes()
            );
        }
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn hostnames_and_secrets() {
        assert_eq!(hostname("https://index.docker.io/v1/"), "index.docker.io");
        assert_eq!(hostname("docker.io"), "index.docker.io");
        assert_eq!(hostname("http://localhost:5000"), "localhost:5000");
        assert_eq!(server_address("docker.io"), "https://index.docker.io/v1/");
        assert_eq!(server_address("ghcr.io"), "ghcr.io");
        let c = Credential {
            username: "u".into(),
            secret: "s3cr3t".into(),
            identity_token: false,
        };
        assert!(!format!("{c:?}").contains("s3cr3t"));
        assert_eq!(c.basic(), "Basic dTpzM2NyM3Q=");
    }
}
