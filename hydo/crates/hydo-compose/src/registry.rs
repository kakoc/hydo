//! Image digests straight from the registry, without skopeo.
//!
//! [`resolve_image`] is hydo_compose.py's `resolve_image`: what `skopeo inspect --raw` (the
//! manifest or index a tag names, and its sha256) and `skopeo inspect --config --raw` (each
//! platform's image config) gave, through the registry's HTTP API (the Docker Registry v2 /
//! OCI distribution API: pulls from Docker Hub with a token from auth.docker.io, other registries
//! with no auth, a Bearer challenge, or basic authentication; since Milestone 320 with the
//! credential that `docker login` keeps, [`crate::creds`]). [`fetch_image`] is fetch_image.py: one image
//! by manifest digest into a `docker save` archive, every blob checked against its digest.
//!
//! [`Registry`] is the seam: [`Http`] talks to the network, [`Recorded`] answers from files
//! (the tests' recorded registry answers; [`Recording`] writes them).

use crate::creds;
use crate::pyval::{from_json, Y};
use crate::{parse_ref, short_name, ComposeError};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const INDEX_TYPES: &[&str] = &[
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
];

/// What `skopeo inspect --raw` accepts: indexes and single-platform manifests.
const ACCEPT_ANY: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.v1+prettyjws, application/vnd.docker.distribution.manifest.v1+json";
/// fetch_image.py's Accept: single-platform manifests only.
const ACCEPT_ONE: &str =
    "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

pub fn sha256_hex(b: &[u8]) -> String {
    let d = Sha256::digest(b);
    d.iter().map(|x| format!("{x:02x}")).collect()
}

/// The two requests the importer makes of a registry.
pub trait Registry {
    /// The raw manifest (or index) that `reference` (a tag or a digest) names.
    fn manifest(&mut self, registry: &str, repo: &str, reference: &str) -> Result<Vec<u8>, String>;
    /// A blob by digest.
    fn blob(&mut self, registry: &str, repo: &str, digest: &str) -> Result<Vec<u8>, String>;
}

// ---------------------------------------------------------------- HTTP

/// The Docker config folder that [`Http::new`] reads registry credentials from (Milestone 320).
/// A program's `main` sets it from hydo-config's `DOCKER_CONFIG` ([`set_docker_config`]); unset,
/// hydo looks for no credential.
static DOCKER_CONFIG: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Set the Docker config folder of every later [`Http::new`] (`hydo_config::tools::Tools`'s
/// `docker_config`). Called once by a program's `main`.
pub fn set_docker_config(dir: Option<PathBuf>) {
    if let Ok(mut g) = DOCKER_CONFIG.lock() {
        *g = dir;
    }
}

/// The registry's HTTP API. A registry that asks for a login gets the credential that Docker
/// keeps for it ([`crate::creds`]), in its token flow (`WWW-Authenticate: Bearer`: the
/// credential goes to the token service the challenge names, and the token to the registry) or
/// as plain basic authentication (`WWW-Authenticate: Basic`).
pub struct Http {
    agent: ureq::Agent,
    /// `Authorization` values: a bearer token per `<host>/<repo>`, basic authentication per
    /// `<host>`.
    auth: Vec<(String, String)>,
    docker: creds::DockerConfig,
    /// What the credential lookup found, per registry (looked up once).
    found: Vec<(String, creds::Found)>,
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

/// docker.io's API host and the repository name it wants.
fn api_host(registry: &str) -> &str {
    match registry {
        "docker.io" | "index.docker.io" | "registry-1.docker.io" => "registry-1.docker.io",
        r => r,
    }
}

/// A host (with its port) on this machine's loopback: `localhost`, `127.x.x.x` or `[::1]`.
/// Docker reaches such a registry over plain HTTP, and so does hydo; every other one over HTTPS.
pub fn is_loopback(host_port: &str) -> bool {
    let h = if let Some(r) = host_port.strip_prefix('[') {
        r.split(']').next().unwrap_or("")
    } else {
        host_port.rsplit_once(':').map(|x| x.0).unwrap_or(host_port)
    };
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>().is_ok_and(|a| a.is_loopback())
}

/// The URL's host with its port (`https://a.b:5000/x` -> `a.b:5000`).
fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map(|x| x.1).unwrap_or(url);
    rest.split(['/', '?']).next().unwrap_or(rest)
}

impl Http {
    /// With the Docker config folder of [`set_docker_config`].
    pub fn new() -> Http {
        let dir = DOCKER_CONFIG.lock().ok().and_then(|g| g.clone());
        Http::with_docker_config(creds::DockerConfig { dir, path: None })
    }

    /// With the credentials of `docker` (tests name a folder and a `PATH` of their own).
    pub fn with_docker_config(docker: creds::DockerConfig) -> Http {
        let cfg = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(600)))
            // a redirect (a blob in a storage service) takes no Authorization header along
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
            .build();
        Http {
            agent: cfg.into(),
            auth: Vec::new(),
            docker,
            found: Vec::new(),
        }
    }

    /// The credential lookup of `registry`, once.
    fn found(&mut self, registry: &str) -> creds::Found {
        if let Some((_, f)) = self.found.iter().find(|(r, _)| r == registry) {
            return f.clone();
        }
        let f = creds::lookup(&self.docker, registry);
        self.found.push((registry.to_string(), f.clone()));
        f
    }

    fn set_auth(&mut self, key: &str, value: String) {
        self.auth.retain(|(k, _)| k != key);
        self.auth.push((key.to_string(), value));
    }

    /// What to say when a registry refuses: where hydo looked, without any secret, and what to
    /// do.
    fn advice(&mut self, registry: &str) -> String {
        let f = self.found(registry);
        let login = if registry == "docker.io" {
            "docker login".to_string()
        } else {
            format!("docker login {registry}")
        };
        let then = format!(
            "Log in with `{login}` and run the command again, or take the image from the local Docker with `hydo compose --from-docker`"
        );
        match f.cred {
            Some(_) => format!(
                "the registry refused the credential from {} (expired, or without access to this repository). {then}",
                f.how
            ),
            None => format!("hydo found no credential for {registry}: {}. {then}", f.how),
        }
    }

    /// Answer a 401's challenge `hdr`: a bearer token for `<host>/<repo>`, or basic
    /// authentication for the host.
    fn authorize(
        &mut self,
        registry: &str,
        host: &str,
        repo: &str,
        hdr: &str,
    ) -> Result<(), String> {
        let Some(ch) = creds::parse_challenge(hdr) else {
            return Err(format!(
                "the registry sent no challenge hydo understands; {}",
                self.advice(registry)
            ));
        };
        let f = self.found(registry);
        match ch.scheme.to_ascii_lowercase().as_str() {
            "bearer" => {
                let t = self.token(&ch, registry, repo, &f)?;
                self.set_auth(&format!("{host}/{repo}"), format!("Bearer {t}"));
                Ok(())
            }
            "basic" => match &f.cred {
                Some(c) if !c.identity_token => {
                    self.set_auth(host, c.basic());
                    Ok(())
                }
                _ => Err(self.advice(registry)),
            },
            s => Err(format!(
                "the registry asks for {s} authentication, which hydo does not speak; take the image from the local Docker with `hydo compose --from-docker`"
            )),
        }
    }

    /// A token from the token service of a Bearer challenge: with the registry's credential
    /// (basic authentication, or an identity token as a refresh token), else anonymous.
    fn token(
        &mut self,
        ch: &creds::Challenge,
        registry: &str,
        repo: &str,
        f: &creds::Found,
    ) -> Result<String, String> {
        let realm = ch
            .get("realm")
            .ok_or("the registry's Bearer challenge names no realm")?
            .to_string();
        let scope = ch
            .get("scope")
            .map(str::to_string)
            .unwrap_or_else(|| format!("repository:{repo}:pull"));
        let service = ch.get("service").unwrap_or("").to_string();
        let secure = realm.starts_with("https://")
            || realm.starts_with("http://") && is_loopback(url_host(&realm));
        if f.cred.is_some() && !secure {
            return Err(format!(
                "the registry's token service {realm} is not HTTPS, so hydo sends it no credential"
            ));
        }
        let r = match &f.cred {
            Some(c) if c.identity_token => self.agent.post(&realm).send_form([
                ("grant_type", "refresh_token"),
                ("service", service.as_str()),
                ("scope", scope.as_str()),
                ("client_id", "hydo"),
                ("refresh_token", c.secret.as_str()),
            ]),
            Some(c) => self
                .agent
                .get(&realm)
                .query("service", &service)
                .query("scope", &scope)
                .header("Authorization", c.basic())
                .call(),
            None => self
                .agent
                .get(&realm)
                .query("service", &service)
                .query("scope", &scope)
                .call(),
        };
        let mut r = r.map_err(|e| format!("{realm}: {e}"))?;
        let st = r.status().as_u16();
        if st == 401 || st == 403 {
            return Err(format!(
                "the token service {realm} answered HTTP {st}: {}",
                self.advice(registry)
            ));
        }
        if st != 200 {
            return Err(format!("{realm}: HTTP {st}"));
        }
        let body = r
            .body_mut()
            .with_config()
            .limit(1 << 20)
            .read_to_vec()
            .map_err(|e| format!("{realm}: {e}"))?;
        // the answer holds the token: an error must not quote it
        let j: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|_| format!("{realm}: the answer is not JSON"))?;
        j.get("token")
            .or_else(|| j.get("access_token"))
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("{realm}: no token in the answer"))
    }

    /// GET /v2/<repo>/<path>, answering an authentication challenge once.
    fn get(
        &mut self,
        registry: &str,
        repo: &str,
        path: &str,
        accept: Option<&str>,
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        let host = api_host(registry).to_string();
        let repo = if host == "registry-1.docker.io" && !repo.contains('/') {
            format!("library/{repo}")
        } else {
            repo.to_string()
        };
        let scheme = if is_loopback(&host) { "http" } else { "https" };
        let url = format!("{scheme}://{host}/v2/{repo}/{path}");
        let key = format!("{host}/{repo}");
        let mut answered = false;
        loop {
            let mut req = self.agent.get(&url);
            if let Some(a) = accept {
                req = req.header("Accept", a);
            }
            if let Some((_, a)) = self.auth.iter().find(|(k, _)| *k == key || *k == host) {
                req = req.header("Authorization", a.as_str());
            }
            let r = req.call().map_err(|e| format!("{url}: {e}"))?;
            let st = r.status().as_u16();
            if st == 401 && !answered {
                answered = true;
                let h = r
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                self.authorize(registry, &host, &repo, &h)
                    .map_err(|m| format!("{url}: HTTP 401 (unauthorized): {m}"))?;
                continue;
            }
            if st == 401 || st == 403 {
                let what = if st == 401 { "unauthorized" } else { "denied" };
                return Err(format!(
                    "{url}: HTTP {st} ({what}): {}",
                    self.advice(registry)
                ));
            }
            if st != 200 {
                return Err(format!("{url}: HTTP {st}"));
            }
            return Ok(r);
        }
    }

    fn read_all(r: ureq::http::Response<ureq::Body>, what: &str) -> Result<Vec<u8>, String> {
        let mut r = r;
        r.body_mut()
            .with_config()
            .limit(u64::MAX)
            .read_to_vec()
            .map_err(|e| format!("{what}: {e}"))
    }
}

impl Registry for Http {
    fn manifest(&mut self, registry: &str, repo: &str, reference: &str) -> Result<Vec<u8>, String> {
        let r = self.get(
            registry,
            repo,
            &format!("manifests/{reference}"),
            Some(ACCEPT_ANY),
        )?;
        Self::read_all(r, reference)
    }

    fn blob(&mut self, registry: &str, repo: &str, digest: &str) -> Result<Vec<u8>, String> {
        let r = self.get(registry, repo, &format!("blobs/{digest}"), None)?;
        Self::read_all(r, digest)
    }
}

// ---------------------------------------------------------------- recorded answers

fn rec_path(dir: &Path, registry: &str, repo: &str, kind: &str, reference: &str) -> PathBuf {
    let safe = |s: &str| s.replace([':', '/'], "_");
    dir.join(safe(registry))
        .join(safe(repo))
        .join(kind)
        .join(safe(reference))
}

/// Registry answers from files `<dir>/<registry>/<repo>/{manifests,blobs}/<reference>` (`/` and
/// `:` written as `_`), as [`Recording`] wrote them.
pub struct Recorded(pub PathBuf);

impl Registry for Recorded {
    fn manifest(&mut self, registry: &str, repo: &str, reference: &str) -> Result<Vec<u8>, String> {
        let p = rec_path(&self.0, registry, repo, "manifests", reference);
        std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
    }

    fn blob(&mut self, registry: &str, repo: &str, digest: &str) -> Result<Vec<u8>, String> {
        let p = rec_path(&self.0, registry, repo, "blobs", digest);
        std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
    }
}

/// Another registry, with every answer also written under a directory (for [`Recorded`]).
pub struct Recording<'a>(pub &'a mut dyn Registry, pub PathBuf);

impl Registry for Recording<'_> {
    fn manifest(&mut self, registry: &str, repo: &str, reference: &str) -> Result<Vec<u8>, String> {
        let b = self.0.manifest(registry, repo, reference)?;
        let p = rec_path(&self.1, registry, repo, "manifests", reference);
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&p, &b).map_err(|e| e.to_string())?;
        Ok(b)
    }

    fn blob(&mut self, registry: &str, repo: &str, digest: &str) -> Result<Vec<u8>, String> {
        let b = self.0.blob(registry, repo, digest)?;
        let p = rec_path(&self.1, registry, repo, "blobs", digest);
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&p, &b).map_err(|e| e.to_string())?;
        Ok(b)
    }
}

// ---------------------------------------------------------------- resolve

fn jget<'a>(v: &'a serde_json::Value, k: &str) -> Option<&'a serde_json::Value> {
    v.get(k).filter(|x| !x.is_null())
}

fn jstr(v: &serde_json::Value, k: &str) -> String {
    jget(v, k)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// `skopeo inspect --config --raw docker://<registry>/<repo>@<digest>`: the config blob of a
/// single-platform manifest.
fn config_of(
    reg: &mut dyn Registry,
    registry: &str,
    repo: &str,
    digest: &str,
) -> Result<serde_json::Value, String> {
    let raw = reg.manifest(registry, repo, digest)?;
    let m: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|e| format!("{digest}: {e}"))?;
    let cd = m
        .get("config")
        .and_then(|c| c.get("digest"))
        .and_then(|d| d.as_str())
        .ok_or_else(|| format!("{registry}/{repo}@{digest}: the manifest names no config"))?
        .to_string();
    let b = reg.blob(registry, repo, &cd)?;
    if format!("sha256:{}", sha256_hex(&b)) != cd {
        return Err(format!("{cd} does not match its digest"));
    }
    serde_json::from_slice(&b).map_err(|e| format!("{cd}: {e}"))
}

/// The lock entry of an image reference: {ref, name, tag, index_digest, registry, repo,
/// platforms: {amd64: {digest, variant, variants, config}, arm64: ...}}.
pub fn resolve_image(reg: &mut dyn Registry, r: &str) -> Result<Y, ComposeError> {
    let (registry, repo, tag, digest) = parse_ref(r);
    let e = |m: String| ComposeError(format!("registry {registry}/{repo}: {m}"));
    let reference = match (&digest, &tag) {
        (Some(d), _) => d.clone(),
        (None, Some(t)) => t.clone(),
        _ => "latest".into(),
    };
    let raw = reg.manifest(&registry, &repo, &reference).map_err(e)?;
    let top = format!("sha256:{}", sha256_hex(&raw));
    if let Some(d) = &digest {
        if *d != top {
            return Err(ComposeError(format!(
                "{r}: the registry's manifest has digest {top}, not {d}"
            )));
        }
    }
    let doc: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|x| e(format!("{reference}: {x}")))?;
    // platforms: arch (None for a single manifest) -> (digest, variant, variants)
    #[allow(clippy::type_complexity)]
    let mut plats: Vec<(Option<String>, String, String, Option<Vec<String>>)> = Vec::new();
    let mt = jstr(&doc, "mediaType");
    if INDEX_TYPES.contains(&mt.as_str()) || doc.get("manifests").is_some() {
        let mut cands: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let empty = Vec::new();
        for mf in doc
            .get("manifests")
            .and_then(|m| m.as_array())
            .unwrap_or(&empty)
        {
            let p = mf
                .get("platform")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let os = jstr(&p, "os");
            let arch = jstr(&p, "architecture");
            if os != "linux" || !(arch == "amd64" || arch == "arm64") {
                continue;
            }
            let v = jstr(&p, "variant");
            let d = jstr(mf, "digest");
            match cands.iter_mut().find(|(a, _)| *a == arch) {
                Some(c) => c.1.push((v, d)),
                None => cands.push((arch, vec![(v, d)])),
            }
        }
        for (arch, lst) in cands {
            let ok: &[&str] = if arch == "amd64" {
                &["", "v1", "v2"]
            } else {
                &["", "v8"]
            };
            let mut pref: Vec<(String, String)> = lst
                .iter()
                .filter(|x| ok.contains(&x.0.as_str()))
                .cloned()
                .collect();
            if pref.is_empty() {
                pref = lst.clone();
                pref.sort();
            }
            let mut variants: Vec<String> = lst.iter().map(|x| x.0.clone()).collect();
            variants.sort();
            plats.push((
                Some(arch),
                pref[0].1.clone(),
                pref[0].0.clone(),
                Some(variants),
            ));
        }
    } else {
        plats.push((None, top.clone(), String::new(), None));
    }
    let mut out = Y::Map(Vec::new());
    for (arch, pd, pv, variants) in plats {
        let cfg = config_of(reg, &registry, &repo, &pd).map_err(e)?;
        let a = match &arch {
            Some(a) => a.clone(),
            None => jstr(&cfg, "architecture"),
        };
        if a != "amd64" && a != "arm64" {
            continue;
        }
        let c = cfg
            .get("config")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let c = if c.is_object() {
            c
        } else {
            serde_json::json!({})
        };
        let mut conf = Y::Map(Vec::new());
        for k in [
            "Entrypoint",
            "Cmd",
            "Env",
            "WorkingDir",
            "User",
            "ExposedPorts",
            "Healthcheck",
            "Volumes",
        ] {
            conf.set(Y::s(k), c.get(k).map(from_json).unwrap_or(Y::Null));
        }
        let variant = if !pv.is_empty() {
            pv
        } else {
            jstr(&cfg, "variant")
        };
        let entry = Y::Map(vec![
            (Y::s("digest"), Y::Str(pd)),
            (Y::s("variant"), Y::Str(variant)),
            (
                Y::s("variants"),
                Y::Seq(
                    variants
                        .unwrap_or_default()
                        .into_iter()
                        .map(Y::Str)
                        .collect(),
                ),
            ),
            (Y::s("config"), conf),
        ]);
        out.set(Y::Str(a), entry);
    }
    // an image with only one of the two architectures is kept: hydo compose writes a hydo.yaml
    // for both, and fetch-images.sh stops on the machine whose architecture it lacks
    if !out.has("amd64") && !out.has("arm64") {
        let mut found: Vec<String> = out.iter_keys().iter().map(crate::pyval::pstr).collect();
        found.sort();
        return Err(ComposeError(format!(
            "{r}: no linux/amd64 or linux/arm64 image (found: {})",
            if found.is_empty() {
                "none".into()
            } else {
                found.join(", ")
            }
        )));
    }
    Ok(Y::Map(vec![
        (Y::s("ref"), Y::s(r)),
        (Y::s("name"), Y::Str(short_name(&registry, &repo))),
        (Y::s("tag"), tag.map(Y::Str).unwrap_or(Y::Null)),
        (Y::s("index_digest"), Y::Str(top)),
        (Y::s("registry"), Y::Str(registry)),
        (Y::s("repo"), Y::Str(repo)),
        (Y::s("platforms"), out),
    ]))
}

// ---------------------------------------------------------------- fetch one image (fetch_image.py)

/// The tag as skopeo's docker-archive records it in RepoTags: normalised, with the registry
/// (docker.io/library/redis:alpine for redis:alpine). hydo_oci copies RepoTags into image.json.
pub fn full_tag(tag: &str) -> String {
    let first = tag.split('/').next().unwrap_or("");
    if !tag.contains('/') || !(first.contains('.') || first.contains(':') || first == "localhost") {
        let t = if tag.contains('/') {
            tag.to_string()
        } else {
            format!("library/{tag}")
        };
        return format!("docker.io/{t}");
    }
    tag.to_string()
}

/// `<registry>/<repository>` -> (registry, repository) as fetch_image.py splits it.
fn split_ref(r: &str) -> (String, String) {
    let (host, repo) = r.split_once('/').unwrap_or((r, ""));
    if matches!(
        host,
        "docker.io" | "index.docker.io" | "registry-1.docker.io"
    ) {
        let repo = if repo.contains('/') {
            repo.to_string()
        } else {
            format!("library/{repo}")
        };
        return ("docker.io".into(), repo);
    }
    (host.to_string(), repo.to_string())
}

/// A blob into a file, streamed and checked against its digest.
fn blob_to_file(
    h: &mut Http,
    registry: &str,
    repo: &str,
    digest: &str,
    dst: &Path,
) -> Result<(), String> {
    let mut r = h.get(registry, repo, &format!("blobs/{digest}"), None)?;
    let mut f = std::fs::File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let mut hs = Sha256::new();
    let mut rd = r.body_mut().with_config().limit(u64::MAX).reader();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = rd.read(&mut buf).map_err(|e| format!("{digest}: {e}"))?;
        if n == 0 {
            break;
        }
        hs.update(&buf[..n]);
        f.write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", dst.display()))?;
    }
    let got: String = hs.finalize().iter().map(|x| format!("{x:02x}")).collect();
    if format!("sha256:{got}") != digest {
        return Err(format!(
            "{digest} does not match its digest (got sha256:{got})"
        ));
    }
    Ok(())
}

fn tar_header(name: &str, size: u64) -> Result<tar::Header, String> {
    let mut h = tar::Header::new_gnu();
    h.set_path(name).map_err(|e| format!("{name}: {e}"))?;
    h.set_size(size);
    h.set_mode(0o644);
    h.set_mtime(0);
    h.set_uid(0);
    h.set_gid(0);
    h.set_entry_type(tar::EntryType::Regular);
    h.set_cksum();
    Ok(h)
}

/// fetch_image.py: copy one image (a single-platform manifest digest) into a `docker save`
/// archive `out` (manifest.json, the config as `<hex>.json`, the layers as downloaded), every blob
/// checked. Progress goes to stderr with fetch_image.py's lines.
pub fn fetch_image(r: &str, digest: &str, tag: &str, out: &str) -> Result<(), String> {
    let tag = full_tag(tag);
    let (registry, repo) = split_ref(r);
    let mut h = Http::new();
    let raw = {
        let resp = h.get(
            &registry,
            &repo,
            &format!("manifests/{digest}"),
            Some(ACCEPT_ONE),
        )?;
        Http::read_all(resp, digest)?
    };
    let got = sha256_hex(&raw);
    if format!("sha256:{got}") != digest {
        return Err(format!(
            "{digest} does not match its digest (got sha256:{got})"
        ));
    }
    let man: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|e| format!("{digest}: {e}"))?;
    if man.get("manifests").is_some() {
        return Err(format!(
            "{digest} is an index; give the single-platform manifest digest"
        ));
    }
    let cfg_d = man["config"]["digest"]
        .as_str()
        .ok_or("the manifest has no config")?
        .to_string();
    let cfg = h.blob(&registry, &repo, &cfg_d)?;
    if format!("sha256:{}", sha256_hex(&cfg)) != cfg_d {
        return Err(format!("{cfg_d} does not match its digest"));
    }
    let outp = Path::new(out);
    let parent = outp
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let tmp = parent.join(format!("fetch-image-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let res = (|| -> Result<usize, String> {
        let layers = man["layers"].as_array().cloned().unwrap_or_default();
        let mut files = Vec::new();
        for (i, l) in layers.iter().enumerate() {
            let d = l["digest"].as_str().ok_or("a layer has no digest")?;
            let hex = d.split(':').nth(1).unwrap_or(d);
            let p = tmp.join(format!("{hex}.tar"));
            blob_to_file(&mut h, &registry, &repo, d, &p)?;
            eprintln!(
                "fetch_image: layer {}/{} {}",
                i + 1,
                layers.len(),
                d.chars().take(19).collect::<String>()
            );
            files.push(p);
        }
        let chex = cfg_d.split(':').nth(1).unwrap_or(&cfg_d).to_string();
        let mut mj = String::from("[{\"Config\": ");
        crate::pyval::json_str(&format!("{chex}.json"), &mut mj);
        mj.push_str(", \"RepoTags\": [");
        crate::pyval::json_str(&tag, &mut mj);
        mj.push_str("], \"Layers\": [");
        for (i, p) in files.iter().enumerate() {
            if i > 0 {
                mj.push_str(", ");
            }
            crate::pyval::json_str(&p.file_name().unwrap().to_string_lossy(), &mut mj);
        }
        mj.push_str("]}]");
        let part = format!("{out}.part");
        {
            let f = std::fs::File::create(&part).map_err(|e| format!("{part}: {e}"))?;
            let mut t = tar::Builder::new(f);
            for (name, data) in [
                ("manifest.json".to_string(), mj.into_bytes()),
                (format!("{chex}.json"), cfg.clone()),
            ] {
                let hd = tar_header(&name, data.len() as u64)?;
                t.append(&hd, &data[..])
                    .map_err(|e| format!("{part}: {e}"))?;
            }
            for p in &files {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                let size = std::fs::metadata(p).map_err(|e| e.to_string())?.len();
                let hd = tar_header(&name, size)?;
                let f = std::fs::File::open(p).map_err(|e| e.to_string())?;
                t.append(&hd, f).map_err(|e| format!("{part}: {e}"))?;
            }
            t.into_inner()
                .and_then(|mut f| f.flush())
                .map_err(|e| format!("{part}: {e}"))?;
        }
        std::fs::rename(&part, out).map_err(|e| format!("{out}: {e}"))?;
        Ok(files.len())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    let n = res?;
    eprintln!(
        "fetch_image: {r}@{} -> {out} ({n} layers, config {})",
        digest.chars().take(19).collect::<String>(),
        cfg_d.chars().take(19).collect::<String>()
    );
    Ok(())
}

/// Milestone 258: an npm package's tarball (`url`), checked against its pinned sha256 and
/// unpacked into the folder `out` (see [`unpack_npm`]). A folder that is there already is kept.
pub fn fetch_npm_package(url: &str, sha256: &str, out: &Path) -> Result<(), String> {
    if out.is_dir() {
        return Ok(());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .build()
        .into();
    let r = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    if r.status().as_u16() != 200 {
        return Err(format!("{url}: HTTP {}", r.status().as_u16()));
    }
    let body = Http::read_all(r, url)?;
    let got = sha256_hex(&body);
    if got != sha256 {
        return Err(format!(
            "{url}: sha256 {got}, and this hydo pins {sha256}; nothing was unpacked"
        ));
    }
    unpack_npm(&body, out).map_err(|e| format!("{url}: {e}"))
}

/// An npm tarball's `package/` folder into the folder `out`. Files get the mode 0644, or 0755
/// when the tarball marks them executable, and folders 0755, so that the folder is the same
/// whatever the umask: an image's bytes depend on the modes (open item 78).
pub fn unpack_npm(tgz: &[u8], out: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let part = out.with_extension("part");
    let _ = std::fs::remove_dir_all(&part);
    std::fs::create_dir_all(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(tgz));
    for e in ar.entries().map_err(|e| e.to_string())? {
        let mut e = e.map_err(|e| e.to_string())?;
        let name = e
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .to_string();
        let Some(rel) = name.strip_prefix("package/") else {
            continue;
        };
        if rel.is_empty() || rel.split('/').any(|c| c == ".." || c.is_empty()) {
            continue;
        }
        let dst = part.join(rel);
        let kind = e.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(&dst).map_err(|e| format!("{}: {e}", dst.display()))?;
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        let exec = e.header().mode().unwrap_or(0o644) & 0o111 != 0;
        let mut bytes = Vec::new();
        e.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        std::fs::write(&dst, &bytes).map_err(|e| format!("{}: {e}", dst.display()))?;
        let mode = if exec { 0o755 } else { 0o644 };
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("{}: {e}", dst.display()))?;
    }
    fn dirs(p: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
        for e in std::fs::read_dir(p)? {
            let e = e?;
            if e.file_type()?.is_dir() {
                dirs(&e.path())?;
            }
        }
        Ok(())
    }
    dirs(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    std::fs::rename(&part, out).map_err(|e| format!("{}: {e}", out.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags() {
        assert_eq!(full_tag("redis:alpine"), "docker.io/library/redis:alpine");
        assert_eq!(full_tag("dockersamples/x:1"), "docker.io/dockersamples/x:1");
        assert_eq!(full_tag("ghcr.io/a/b:1"), "ghcr.io/a/b:1");
        assert_eq!(
            split_ref("docker.io/redis"),
            ("docker.io".into(), "library/redis".into())
        );
    }
}
