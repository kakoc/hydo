//! Image digests straight from the registry, without skopeo.
//!
//! [`resolve_image`] is hydo_compose.py's `resolve_image`: what `skopeo inspect --raw` (the
//! manifest or index a tag names, and its sha256) and `skopeo inspect --config --raw` (each
//! platform's image config) gave, through the registry's HTTP API (the Docker Registry v2 /
//! OCI distribution API: anonymous pulls from Docker Hub with a token from auth.docker.io, other
//! registries with no auth or a Bearer challenge). [`fetch_image`] is fetch_image.py: one image
//! by manifest digest into a `docker save` archive, every blob checked against its digest.
//!
//! [`Registry`] is the seam: [`Http`] talks to the network, [`Recorded`] answers from files
//! (the tests' recorded registry answers; [`Recording`] writes them).

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

/// The registry's HTTP API.
pub struct Http {
    agent: ureq::Agent,
    tokens: Vec<(String, String)>,
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

impl Http {
    pub fn new() -> Http {
        let cfg = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(600)))
            .build();
        Http {
            agent: cfg.into(),
            tokens: Vec::new(),
        }
    }

    fn auth(&mut self, hdr: &str, repo: &str) -> Result<String, String> {
        let mut m: Vec<(String, String)> = Vec::new();
        let re = regex::Regex::new(r#"(\w+)="([^"]*)""#).unwrap();
        for c in re.captures_iter(hdr) {
            m.push((c[1].to_string(), c[2].to_string()));
        }
        let get = |k: &str| m.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
        let realm = get("realm").ok_or_else(|| format!("no realm in {hdr}"))?;
        let scope = get("scope").unwrap_or_else(|| format!("repository:{repo}:pull"));
        let mut req = self.agent.get(&realm);
        req = req.query("service", get("service").unwrap_or_default());
        req = req.query("scope", scope);
        let mut r = req.call().map_err(|e| format!("{realm}: {e}"))?;
        if r.status().as_u16() != 200 {
            return Err(format!("{realm}: HTTP {}", r.status().as_u16()));
        }
        let body = r
            .body_mut()
            .with_config()
            .limit(1 << 20)
            .read_to_vec()
            .map_err(|e| format!("{realm}: {e}"))?;
        let j: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| format!("{realm}: {e}"))?;
        j.get("token")
            .or_else(|| j.get("access_token"))
            .and_then(|t| t.as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("{realm}: no token"))
    }

    /// GET /v2/<repo>/<path>, answering a Bearer challenge once.
    fn get(
        &mut self,
        registry: &str,
        repo: &str,
        path: &str,
        accept: Option<&str>,
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        let host = api_host(registry);
        let repo = if host == "registry-1.docker.io" && !repo.contains('/') {
            format!("library/{repo}")
        } else {
            repo.to_string()
        };
        let url = format!("https://{host}/v2/{repo}/{path}");
        let key = format!("{host}/{repo}");
        for _ in 0..2 {
            let mut req = self.agent.get(&url);
            if let Some(a) = accept {
                req = req.header("Accept", a);
            }
            if let Some((_, t)) = self.tokens.iter().find(|(k, _)| *k == key) {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            let r = req.call().map_err(|e| format!("{url}: {e}"))?;
            let st = r.status().as_u16();
            if st == 401 && !self.tokens.iter().any(|(k, _)| *k == key) {
                let h = r
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                if h.contains("Bearer") {
                    let t = self.auth(&h, &repo)?;
                    self.tokens.push((key.clone(), t));
                    continue;
                }
            }
            if st != 200 {
                return Err(format!("{url}: HTTP {st}"));
            }
            return Ok(r);
        }
        Err(format!("{url}: unauthorized"))
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
    if !out.has("amd64") {
        let mut found: Vec<String> = out.iter_keys().iter().map(crate::pyval::pstr).collect();
        found.sort();
        return Err(ComposeError(format!(
            "{r}: no linux/amd64 image (found: {})",
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
/// archive `out` (manifest.json, the config as <hex>.json, the layers as downloaded), every blob
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
