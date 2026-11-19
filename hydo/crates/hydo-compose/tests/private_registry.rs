//! Images in private registries (Milestone 320). A user's `hydo compose` stopped with HTTP 401
//! for an image of a private registry although `docker login` had worked: hydo did not read
//! Docker's credentials, which a credential helper kept in the system's keychain, and
//! `hydo-fetch-image` could not log in either.
//!
//! Since then both take the credential that Docker keeps (`hydo_compose::creds`): an `auths`
//! entry (a base64 `user:password`, or an identity token), or the credential helper that
//! `credHelpers` names for the registry or `credsStore` names for every registry. The registry
//! gets it in its token flow (a Bearer challenge) or as basic authentication. A 401 names the
//! registry and where hydo looked, never a secret, and says `docker login <host>` or
//! `--from-docker`.
//!
//! The first tests run a registry in this process (the token flow, or basic authentication) on
//! 127.0.0.1, with a temporary Docker config and a fake credential helper, so they need no Docker
//! and never read this machine's Docker config or keychain. The last test needs Docker: it runs
//! the `registry:2` image with htpasswd authentication, imports and fetches an image from it, and
//! saves an image of the local Docker with `--from-docker`. It is skipped where Docker does not
//! answer. Every Docker command of the tests runs with a temporary `DOCKER_CONFIG`.

use crate::tmpdir::TmpDir;
use hydo_compose::creds::DockerConfig;
use hydo_compose::registry::{self, Http};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const USER: &str = "hydo-test";
/// The test registries' password (never a real one). No output may contain it.
const PASS: &str = "hydo-test-pw-320";
/// The token the in-process token service gives; no output may contain it either.
const TOKEN: &str = "hydo-test-token-7f3a";
/// An identity token (`auths[...].identitytoken`), as some registries' `docker login` keeps.
const ID_TOKEN: &str = "hydo-test-refresh-91c2";
/// `htpasswd -nbB hydo-test hydo-test-pw-320`, for the registry:2 container.
const HTPASSWD: &str = "hydo-test:$2y$05$mpCZ5X8v.No9wZus0yYn5u8Dk4vlq0VkgFN0DyKcKCdZMlxsYjmja";

fn b64(s: &str) -> String {
    hydo_compose::creds::base64_encode(s.as_bytes())
}

/// No secret of the tests in `text`.
fn no_secret(text: &str) {
    for s in [
        PASS.to_string(),
        TOKEN.to_string(),
        ID_TOKEN.to_string(),
        b64(&format!("{USER}:{PASS}")),
    ] {
        assert!(!text.contains(&s), "a secret is in the output:\n{text}");
    }
}

fn sha(b: &[u8]) -> String {
    let h: String = Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect();
    format!("sha256:{h}")
}

// ---------------------------------------------------------------- the image

const INDEX: &str = "application/vnd.oci.image.index.v1+json";
const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";

/// One small image for linux/amd64 and linux/arm64: an OCI index, two manifests, two configs
/// and two layers (an uncompressed tar with the file `/hello`).
struct Image {
    blobs: Vec<(String, Vec<u8>)>,
    /// digest, bytes, media type, architecture ("" for the index)
    manifests: Vec<(String, Vec<u8>, &'static str, &'static str)>,
    index: Vec<u8>,
}

fn layer(text: &str) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(text.len() as u64);
    h.set_mode(0o755);
    h.set_mtime(0);
    h.set_cksum();
    b.append_data(&mut h, "hello", text.as_bytes()).unwrap();
    b.into_inner().unwrap()
}

fn config(arch: &str, layer: &[u8]) -> Vec<u8> {
    serde_json::json!({
        "architecture": arch,
        "os": "linux",
        "config": {"Cmd": ["/hello"], "ExposedPorts": {"8080/tcp": {}}},
        "rootfs": {"type": "layers", "diff_ids": [sha(layer)]},
    })
    .to_string()
    .into_bytes()
}

fn image() -> Image {
    let mut blobs = Vec::new();
    let mut manifests = Vec::new();
    let mut entries = Vec::new();
    for arch in ["amd64", "arm64"] {
        let l = layer(&format!("#!/bin/sh\necho {arch}\n"));
        let c = config(arch, &l);
        let m = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MANIFEST,
            "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "size": c.len(), "digest": sha(&c)},
            "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar", "size": l.len(), "digest": sha(&l)}],
        })
        .to_string()
        .into_bytes();
        entries.push(serde_json::json!({
            "mediaType": MANIFEST, "size": m.len(), "digest": sha(&m),
            "platform": {"architecture": arch, "os": "linux"},
        }));
        blobs.push((sha(&l), l));
        blobs.push((sha(&c), c));
        manifests.push((sha(&m), m, MANIFEST, arch));
    }
    let index = serde_json::json!({"schemaVersion": 2, "mediaType": INDEX, "manifests": entries})
        .to_string()
        .into_bytes();
    manifests.push((sha(&index), index.clone(), INDEX, ""));
    Image {
        blobs,
        manifests,
        index,
    }
}

// ---------------------------------------------------------------- a registry in this process

#[derive(Clone, Copy, PartialEq)]
enum Scheme {
    /// `WWW-Authenticate: Bearer`, with a token service at /token on the same port.
    Bearer,
    /// `WWW-Authenticate: Basic`.
    Basic,
}

struct State {
    scheme: Scheme,
    image: Image,
    port: u16,
    stop: AtomicBool,
    /// "<method> <path> <what Authorization it had>" of every request (no values)
    log: Mutex<Vec<String>>,
}

/// A registry on 127.0.0.1 that serves [`image`] as `team/app:1.0` to a client with the user
/// [`USER`] and [`PASS`] (or [`ID_TOKEN`] at the token service). It stops when dropped.
struct Fake(Arc<State>);

impl Fake {
    fn start(scheme: Scheme) -> Fake {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let st = Arc::new(State {
            scheme,
            image: image(),
            port,
            stop: AtomicBool::new(false),
            log: Mutex::new(Vec::new()),
        });
        let s2 = st.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                if s2.stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(c) = c {
                    let s3 = s2.clone();
                    std::thread::spawn(move || handle(c, &s3));
                }
            }
        });
        Fake(st)
    }

    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.0.port)
    }

    fn log(&self) -> Vec<String> {
        self.0.log.lock().unwrap().clone()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.0.port));
    }
}

type Answer = (u16, Vec<(String, String)>, Vec<u8>);

fn handle(mut s: TcpStream, st: &State) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let first: Vec<String> = lines
        .next()
        .unwrap_or("")
        .split(' ')
        .map(str::to_string)
        .collect();
    let mut auth = String::new();
    let mut len = 0usize;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("authorization") {
                auth = v.trim().to_string();
            } else if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < len {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    let (method, target) = (
        first.first().cloned().unwrap_or_default(),
        first.get(1).cloned().unwrap_or_default(),
    );
    let kind = auth.split(' ').next().unwrap_or("none").to_string();
    st.log.lock().unwrap().push(format!(
        "{method} {} {}",
        target.split('?').next().unwrap(),
        if kind.is_empty() { "none" } else { &kind }
    ));
    let (code, hdrs, out) = route(st, &method, &target, &auth, &body);
    let mut r = format!(
        "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        out.len()
    );
    for (k, v) in hdrs {
        r.push_str(&format!("{k}: {v}\r\n"));
    }
    r.push_str("\r\n");
    let _ = s.write_all(r.as_bytes());
    let _ = s.write_all(&out);
}

fn route(st: &State, method: &str, target: &str, auth: &str, body: &[u8]) -> Answer {
    let basic_ok = format!("Basic {}", b64(&format!("{USER}:{PASS}")));
    let path = target.split('?').next().unwrap_or("");
    if path == "/token" {
        let ok = if method == "POST" {
            let f = String::from_utf8_lossy(body);
            f.contains("grant_type=refresh_token")
                && f.contains(&format!("refresh_token={ID_TOKEN}"))
        } else if auth.is_empty() {
            // an anonymous token, which the registry then refuses (as Docker Hub does for a
            // private repository)
            return (200, vec![], br#"{"token":"anonymous"}"#.to_vec());
        } else {
            auth == basic_ok
        };
        if !ok {
            return (401, vec![], b"{}".to_vec());
        }
        let key = if method == "POST" {
            "access_token"
        } else {
            "token"
        };
        return (
            200,
            vec![],
            format!(r#"{{"{key}":"{TOKEN}"}}"#).into_bytes(),
        );
    }
    let authorized = match st.scheme {
        Scheme::Bearer => auth == format!("Bearer {TOKEN}"),
        Scheme::Basic => auth == basic_ok,
    };
    if !authorized {
        let ch = match st.scheme {
            Scheme::Bearer => format!(
                r#"Bearer realm="http://127.0.0.1:{}/token",service="hydo-test",scope="repository:team/app:pull""#,
                st.port
            ),
            Scheme::Basic => r#"Basic realm="hydo-test""#.to_string(),
        };
        return (401, vec![("WWW-Authenticate".into(), ch)], b"{}".to_vec());
    }
    if path == "/v2/" {
        return (200, vec![], b"{}".to_vec());
    }
    let Some(rest) = path.strip_prefix("/v2/team/app/") else {
        return (404, vec![], b"{}".to_vec());
    };
    if let Some(r) = rest.strip_prefix("manifests/") {
        if r == "1.0" {
            return (
                200,
                vec![("Content-Type".into(), INDEX.into())],
                st.image.index.clone(),
            );
        }
        if let Some((_, b, mt, _)) = st.image.manifests.iter().find(|m| m.0 == r) {
            return (
                200,
                vec![("Content-Type".into(), mt.to_string())],
                b.clone(),
            );
        }
    }
    if let Some(d) = rest.strip_prefix("blobs/") {
        if let Some((_, b)) = st.image.blobs.iter().find(|x| x.0 == d) {
            return (200, vec![], b.clone());
        }
    }
    (404, vec![], b"{}".to_vec())
}

// ---------------------------------------------------------------- Docker's config, faked

fn executable(p: &Path, text: &str) {
    std::fs::write(p, text).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A credential helper `docker-credential-<name>` in `<d>/bin` that knows [`USER`] and `secret`
/// for `host` only, and writes every server address it is asked for to `<d>/asked-<name>`.
fn helper(d: &Path, name: &str, host: &str, secret: &str) {
    std::fs::create_dir_all(d.join("bin")).unwrap();
    executable(
        &d.join(format!("bin/docker-credential-{name}")),
        &format!(
            "#!/bin/sh\n[ \"$1\" = get ] || exit 1\nh=$(cat)\necho \"$h\" >> {asked}\nif [ \"$h\" = {host} ]; then\n  printf '{{\"ServerURL\":\"%s\",\"Username\":\"{USER}\",\"Secret\":\"{secret}\"}}' \"$h\"\nelse\n  echo 'credentials not found in native keychain'\n  exit 1\nfi\n",
            asked = d.join(format!("asked-{name}")).display(),
        ),
    );
}

fn asked(d: &Path, name: &str) -> String {
    std::fs::read_to_string(d.join(format!("asked-{name}"))).unwrap_or_default()
}

/// A Docker config folder `<d>/docker` with `config.json` = `doc`.
fn docker_config(d: &Path, doc: serde_json::Value) -> DockerConfig {
    let c = d.join("docker");
    std::fs::create_dir_all(&c).unwrap();
    std::fs::write(c.join("config.json"), doc.to_string()).unwrap();
    DockerConfig {
        dir: Some(c),
        path: Some(format!("{}:/usr/bin:/bin", d.join("bin").display()).into()),
    }
}

fn dir(name: &str) -> TmpDir {
    TmpDir::new(&format!(
        "hydo-compose-private-{}-{name}",
        std::process::id()
    ))
}

/// Resolve `team/app:1.0` of `f` with the credentials of `cfg`: the lock entry, or the error.
fn resolve(f: &Fake, cfg: DockerConfig) -> Result<hydo_compose::pyval::Y, String> {
    let mut h = Http::with_docker_config(cfg);
    registry::resolve_image(&mut h, &format!("{}/team/app:1.0", f.host())).map_err(|e| e.0)
}

/// The lock entry pins the index and both platforms by the digests the registry serves.
fn pinned(info: &hydo_compose::pyval::Y) {
    let im = image();
    assert_eq!(
        hydo_compose::pyval::pstr(info.getn("index_digest")),
        sha(&im.index)
    );
    for (d, _, _, arch) in &im.manifests {
        if !arch.is_empty() {
            let p = info.getn("platforms").getn(arch);
            assert_eq!(hydo_compose::pyval::pstr(p.getn("digest")), *d, "{arch}");
        }
    }
}

// ---------------------------------------------------------------- the tests

/// `auths` with a base64 `user:password`, in the token flow: the credential goes to the token
/// service as basic authentication, and the registry gets the token.
#[test]
fn auths_in_the_token_flow() {
    let f = Fake::start(Scheme::Bearer);
    let d = dir("auths");
    let cfg = docker_config(
        &d,
        serde_json::json!({"auths": {f.host(): {"auth": b64(&format!("{USER}:{PASS}"))}}}),
    );
    let info = resolve(&f, cfg).unwrap();
    pinned(&info);
    let log = f.log();
    assert!(log.contains(&"GET /token Basic".to_string()), "{log:?}");
    assert!(
        log.iter()
            .any(|l| l.starts_with("GET /v2/team/app/manifests/") && l.ends_with("Bearer")),
        "{log:?}"
    );
}

/// An `auths` entry with an identity token: the token service gets it as a refresh token.
#[test]
fn auths_identity_token() {
    let f = Fake::start(Scheme::Bearer);
    let d = dir("idtoken");
    let cfg = docker_config(
        &d,
        serde_json::json!({"auths": {format!("http://{}", f.host()): {"auth": b64(&format!("{USER}:")), "identitytoken": ID_TOKEN}}}),
    );
    pinned(&resolve(&f, cfg).unwrap());
    assert!(
        f.log().contains(&"POST /token none".to_string()),
        "{:?}",
        f.log()
    );
}

/// `credHelpers` names a helper for this registry: only this registry's address is asked for, and
/// the registry's Basic challenge gets the credential. `credsStore` and `auths` are not used.
#[test]
fn cred_helpers_with_basic_authentication() {
    let f = Fake::start(Scheme::Basic);
    let d = dir("credhelpers");
    helper(&d, "fake", &f.host(), PASS);
    helper(&d, "store", &f.host(), "wrong");
    let cfg = docker_config(
        &d,
        serde_json::json!({
            "credHelpers": {f.host(): "fake", "other.example:5000": "other"},
            "credsStore": "store",
            "auths": {f.host(): {"auth": b64(&format!("{USER}:wrong"))}},
        }),
    );
    pinned(&resolve(&f, cfg).unwrap());
    assert_eq!(asked(&d, "fake"), format!("{}\n", f.host()));
    assert_eq!(asked(&d, "store"), "");
}

/// `credsStore` serves every registry without a `credHelpers` entry.
#[test]
fn creds_store() {
    let f = Fake::start(Scheme::Bearer);
    let d = dir("credsstore");
    helper(&d, "store", &f.host(), PASS);
    let cfg = docker_config(
        &d,
        serde_json::json!({"credsStore": "store", "auths": {f.host(): {}}}),
    );
    pinned(&resolve(&f, cfg).unwrap());
    assert_eq!(asked(&d, "store"), format!("{}\n", f.host()));
}

/// A wrong password: the error names the registry, where the credential came from, and what to
/// do, and holds no secret. The same for a refused token service and for basic authentication.
#[test]
fn a_wrong_password_is_named_without_the_secret() {
    for scheme in [Scheme::Bearer, Scheme::Basic] {
        let f = Fake::start(scheme);
        let d = dir("wrong");
        helper(&d, "store", &f.host(), "not-the-password");
        let cfg = docker_config(&d, serde_json::json!({"credsStore": "store"}));
        let e = resolve(&f, cfg).unwrap_err();
        no_secret(&e);
        assert!(!e.contains("not-the-password"), "{e}");
        for want in [
            format!("registry {}/team/app", f.host()),
            "HTTP 401".to_string(),
            "the registry refused the credential from the credential helper docker-credential-store (credsStore in".to_string(),
            "docker/config.json)".to_string(),
            format!("`docker login {}`", f.host()),
            "`hydo compose --from-docker`".to_string(),
        ] {
            assert!(e.contains(&want), "{want:?} not in: {e}");
        }
    }
}

/// No credential: the error says where hydo looked (the config file, the helper's answer) and
/// suggests `docker login <host>` or `--from-docker`. A helper that has nothing is asked once.
#[test]
fn a_missing_credential_says_where_hydo_looked() {
    let f = Fake::start(Scheme::Bearer);
    let d = dir("missing");
    // a config without an entry for the registry
    let cfg = docker_config(
        &d,
        serde_json::json!({"auths": {"other.example": {"auth": b64("a:b")}}}),
    );
    let file = cfg.dir.clone().unwrap().join("config.json");
    let e = resolve(&f, cfg).unwrap_err();
    for want in [
        "HTTP 401".to_string(),
        format!("hydo found no credential for {}", f.host()),
        format!(
            "{} has no credHelpers entry for {}",
            file.display(),
            f.host()
        ),
        format!("`docker login {}`", f.host()),
        "--from-docker".to_string(),
    ] {
        assert!(e.contains(&want), "{want:?} not in: {e}");
    }
    // a helper without a credential for it
    helper(&d, "store", "elsewhere.example", PASS);
    let cfg = docker_config(&d, serde_json::json!({"credsStore": "store"}));
    let e = resolve(&f, cfg).unwrap_err();
    assert!(
        e.contains("the credential helper docker-credential-store (credsStore in")
            && e.contains(&format!("has no credential for {}", f.host())),
        "{e}"
    );
    assert_eq!(asked(&d, "store"), format!("{}\n", f.host()));
    no_secret(&e);
    // no config file at all
    let cfg = DockerConfig::at(d.join("none"));
    let e = resolve(&f, cfg).unwrap_err();
    assert!(e.contains("config.json does not exist"), "{e}");
    // a helper that is not installed
    let cfg = docker_config(&d, serde_json::json!({"credHelpers": {f.host(): "absent"}}));
    let e = resolve(&f, cfg).unwrap_err();
    assert!(e.contains("docker-credential-absent (credHelpers["), "{e}");
    assert!(e.contains("is not on PATH"), "{e}");
}

/// The command line: `hydo-compose` pins an image of a private registry by digest, with the
/// credential of a helper that `DOCKER_CONFIG` names, and the fetch-images.sh it writes fetches
/// it with `hydo-fetch-image`. Neither prints a secret. Without the credential both stop with the
/// advice.
#[test]
fn compose_and_fetch_images_with_a_credential_helper() {
    let f = Fake::start(Scheme::Bearer);
    let d = dir("cli");
    helper(&d, "store", &f.host(), PASS);
    let cfg = docker_config(&d, serde_json::json!({"credsStore": "store"}));
    let (out, e) = cli_import(&d, &format!("{}/team/app:1.0", f.host()), &cfg, &[]);
    assert_eq!(out.0, 0, "{e}");
    no_secret(&e);
    let lock: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(d.join("shop/hydo-compose.lock.json")).unwrap(),
    )
    .unwrap();
    let entry = &lock["images"][format!("{}/team/app:1.0", f.host())];
    assert_eq!(entry["index_digest"], sha(&image().index), "{lock}");
    let (rc, e) = fetch(&d, &cfg, &[]);
    assert_eq!(rc, 0, "{e}");
    no_secret(&e);
    let arch = if cfg!(target_os = "macos") {
        "arm64"
    } else {
        "amd64"
    };
    let file = if arch == "arm64" {
        "arm64-oci"
    } else {
        "amd64.docker.tar"
    };
    let p = d.join(format!(
        "shop/images/{}-team-app-1.0-{file}",
        f.host().replace(':', "-")
    ));
    let got = hydo_compose::ocilocal::config_from_file(&p.display().to_string(), arch).unwrap();
    assert_eq!(
        hydo_compose::pyval::pstr(got.getn("config").getn("Cmd")),
        "['/hello']"
    );
    // the same with an empty Docker config: both stop and say why, with no secret
    std::fs::remove_file(&p).unwrap();
    let empty = docker_config(&d.join("empty"), serde_json::json!({}));
    let (rc, e) = fetch(&d, &empty, &[]);
    assert_ne!(rc, 0);
    assert!(
        e.contains("HTTP 401") && e.contains(&format!("`docker login {}`", f.host())),
        "{e}"
    );
    std::fs::remove_file(d.join("shop/hydo-compose.lock.json")).unwrap();
    let (out, e) = cli_import(&d, &format!("{}/team/app:1.0", f.host()), &empty, &[]);
    assert_eq!(out.0, 1, "{e}");
    assert!(e.contains("hydo found no credential for"), "{e}");
}

/// `hydo-compose <file> --out shop/hydo.yaml` of a Compose file with one service of `image`,
/// with `DOCKER_CONFIG` and `PATH` from `cfg`: ((status,), stderr).
fn cli_import(d: &Path, image: &str, cfg: &DockerConfig, extra: &[&str]) -> ((i32,), String) {
    std::fs::create_dir_all(d.join("shop")).unwrap();
    std::fs::write(
        d.join("shop/docker-compose.yml"),
        format!("services:\n  app:\n    image: {image}\n"),
    )
    .unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_hydo-compose"));
    c.arg(d.join("shop/docker-compose.yml")).args(extra);
    with_docker(&mut c, d, cfg);
    let o = c.output().unwrap();
    (
        (o.status.code().unwrap_or(-1),),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// The variables of the generated fetch-images.sh. They are the script's, not hydo's: hydo never
/// reads them, so hydo-config's inventory leaves them out, as it does every shell script's.
const FETCH_IMAGE: &str = "HYDO_FETCH_IMAGE";
const DOCKER: &str = "HYDO_DOCKER";
const ACCEPT: &str = "HYDO_ACCEPT_REBUILD";

/// sh shop/fetch-images.sh with the real hydo-fetch-image: (status, stderr).
fn fetch(d: &Path, cfg: &DockerConfig, env: &[(&str, &str)]) -> (i32, String) {
    let mut c = Command::new("sh");
    c.arg(d.join("shop/fetch-images.sh"))
        .env(FETCH_IMAGE, env!("CARGO_BIN_EXE_hydo-fetch-image"));
    with_docker(&mut c, d, cfg);
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// The child's `DOCKER_CONFIG` and `PATH` from `cfg`, and a `HOME` of its own, so that nothing
/// it runs reads this machine's Docker config.
fn with_docker(c: &mut Command, d: &Path, cfg: &DockerConfig) {
    let path = format!(
        "{}:{}",
        cfg.path.as_ref().unwrap().to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    c.env("DOCKER_CONFIG", cfg.dir.as_ref().unwrap())
        .env("PATH", path)
        .env("HOME", d)
        .env_remove(DOCKER)
        .env_remove(ACCEPT);
}

// ---------------------------------------------------------------- with Docker

/// A Docker config in `<d>/docker` whose `credsStore` is a helper of the test's own that has no
/// credential, so that a Docker CLI run with it never asks this machine's keychain (without a
/// `credsStore`, the CLI on a Mac takes the keychain's helper).
fn no_credentials(d: &Path) -> DockerConfig {
    helper(d, "hydo-test-none", "no-registry.invalid", PASS);
    docker_config(d, serde_json::json!({"credsStore": "hydo-test-none"}))
}

/// The Docker CLI with the temporary Docker config of [`no_credentials`] in `<d>/docker-cli`,
/// or None where Docker does not answer.
fn docker(d: &Path) -> Option<impl Fn(&[&str]) -> Result<String, String>> {
    let cfg = no_credentials(&d.join("docker-cli"));
    let path = format!(
        "{}:{}",
        cfg.path.as_ref().unwrap().to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    let cfgdir = cfg.dir.unwrap();
    let run = move |a: &[&str]| -> Result<String, String> {
        let o = Command::new("docker")
            .args(a)
            .env("DOCKER_CONFIG", &cfgdir)
            .env("PATH", &path)
            .output()
            .map_err(|e| e.to_string())?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
        }
    };
    match run(&["version", "--format", "{{.Server.Version}}"]) {
        Ok(_) => Some(run),
        Err(e) => {
            eprintln!("skipped: Docker does not answer ({e})");
            None
        }
    }
}

/// Runs `docker <args>` when dropped (removes what a test made).
struct Cleanup<F: Fn(&[&str]) -> Result<String, String>>(F, Vec<String>);

impl<F: Fn(&[&str]) -> Result<String, String>> Drop for Cleanup<F> {
    fn drop(&mut self) {
        let a: Vec<&str> = self.1.iter().map(String::as_str).collect();
        let _ = (self.0)(&a);
    }
}

/// A `docker save` archive (`docker load` reads it) of one image for this machine's guest
/// architecture, named `tag`, whose one file `/hello` holds `text`.
fn docker_archive(path: &Path, tag: &str, text: &str) {
    let arch = if cfg!(target_os = "macos") {
        "arm64"
    } else {
        "amd64"
    };
    let l = layer(text);
    let c = config(arch, &l);
    let ch = &sha(&c)[7..];
    let lh = &sha(&l)[7..];
    let m = serde_json::json!([{"Config": format!("{ch}.json"), "RepoTags": [tag], "Layers": [format!("{lh}/layer.tar")]}]).to_string();
    let mut b = tar::Builder::new(std::fs::File::create(path).unwrap());
    for (name, data) in [
        ("manifest.json".to_string(), m.into_bytes()),
        (format!("{ch}.json"), c),
        (format!("{lh}/layer.tar"), l),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, &name, &data[..]).unwrap();
    }
    b.finish().unwrap();
}

/// Push [`image`] to the registry at `base` as `team/app:1.0`, with basic authentication.
fn push(base: &str) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let auth = format!("Basic {}", b64(&format!("{USER}:{PASS}")));
    let im = image();
    for (d, b) in &im.blobs {
        let r = agent
            .post(&format!("{base}/v2/team/app/blobs/uploads/"))
            .header("Authorization", &auth)
            .send_empty()
            .unwrap();
        assert_eq!(r.status().as_u16(), 202);
        let loc = r
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let loc = if loc.starts_with("http") {
            loc
        } else {
            format!("{base}{loc}")
        };
        let sep = if loc.contains('?') { '&' } else { '?' };
        let r = agent
            .put(&format!("{loc}{sep}digest={d}"))
            .header("Authorization", &auth)
            .header("Content-Type", "application/octet-stream")
            .send(&b[..])
            .unwrap();
        assert_eq!(r.status().as_u16(), 201, "blob {d}");
    }
    // the platforms' manifests by digest, then the index by its tag
    let mut ms = im.manifests;
    ms.sort_by_key(|m| m.3.is_empty());
    for (d, b, mt, arch) in &ms {
        let reference = if arch.is_empty() { "1.0" } else { d.as_str() };
        let r = agent
            .put(&format!("{base}/v2/team/app/manifests/{reference}"))
            .header("Authorization", &auth)
            .header("Content-Type", *mt)
            .send(&b[..])
            .unwrap();
        assert_eq!(r.status().as_u16(), 201, "manifest {reference}");
    }
}

/// With Docker: the `registry:2` image with htpasswd authentication on 127.0.0.1, an image pushed
/// into it, then `hydo-compose` and fetch-images.sh with a `credsStore` helper (as Docker Desktop
/// and Colima set one up), and a wrong password. Then `--from-docker` with an image of the local
/// Docker whose registry cannot be reached: the file is saved, pinned, saved again by
/// fetch-images.sh, and a changed image is stopped at its pin.
#[test]
fn with_docker_a_private_registry_and_from_docker() {
    let d = dir("docker");
    let Some(dk) = docker(&d) else {
        return;
    };
    if dk(&["image", "inspect", "registry:2"]).is_err() {
        if let Err(e) = dk(&["pull", "-q", "registry:2"]) {
            eprintln!("skipped: registry:2 cannot be pulled ({e})");
            return;
        }
    }
    let name = format!("hydo-test-registry-{}", std::process::id());
    let _ = dk(&["rm", "-f", &name]);
    let started = dk(&[
        "run",
        "-d",
        "--name",
        &name,
        "-p",
        "127.0.0.1::5000",
        "-e",
        &format!("HT={HTPASSWD}"),
        "-e",
        "REGISTRY_AUTH=htpasswd",
        "-e",
        "REGISTRY_AUTH_HTPASSWD_REALM=hydo-test",
        "-e",
        "REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd",
        "--entrypoint",
        "sh",
        "registry:2",
        "-c",
        "mkdir -p /auth && printf '%s\\n' \"$HT\" > /auth/htpasswd && exec /entrypoint.sh /etc/docker/registry/config.yml",
    ]);
    let _gone = Cleanup(
        docker(&d).unwrap(),
        vec!["rm".into(), "-f".into(), name.clone()],
    );
    started.unwrap();
    let port = dk(&["port", &name, "5000/tcp"]).unwrap();
    let host = port
        .lines()
        .find(|l| l.starts_with("127.0.0.1:"))
        .unwrap_or_else(|| panic!("docker port: {port}"))
        .to_string();
    let base = format!("http://{host}");
    // wait for the registry (at most 60 s)
    let t0 = Instant::now();
    loop {
        let ok = ureq::get(&format!("{base}/v2/"))
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .is_ok_and(|r| r.status().as_u16() == 401);
        if ok {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "the registry did not answer at {base}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    push(&base);
    // hydo-compose and fetch-images.sh with a credsStore helper
    helper(&d, "store", &host, PASS);
    let cfg = docker_config(&d, serde_json::json!({"credsStore": "store"}));
    let image_ref = format!("{host}/team/app:1.0");
    let (out, e) = cli_import(&d, &image_ref, &cfg, &[]);
    assert_eq!(out.0, 0, "{e}");
    no_secret(&e);
    let lock: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(d.join("shop/hydo-compose.lock.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        lock["images"][&image_ref]["index_digest"],
        sha(&image().index)
    );
    let (rc, e) = fetch(&d, &cfg, &[]);
    assert_eq!(rc, 0, "{e}");
    no_secret(&e);
    assert!(e.contains("fetch_image: layer 1/1"), "{e}");
    // a wrong password: the registry's Basic challenge refuses it, and the message says so
    let wrong = d.join("wrong");
    helper(&wrong, "store", &host, "not-the-password");
    let cfg_wrong = docker_config(&wrong, serde_json::json!({"credsStore": "store"}));
    // the whole project folder: with the lock file alone gone, the import pinned the image files
    // that fetch-images.sh had saved there and asked no registry, so Linux CI passed the wrong
    // password (the registry's log showed no request)
    std::fs::remove_dir_all(d.join("shop")).unwrap();
    let (out, e) = cli_import(&d, &image_ref, &cfg_wrong, &[]);
    if out.0 != 1 {
        // what the registry saw: whether credentials came, and what it answered (Linux CI's
        // import once passed with the wrong password, open item 165)
        let logs = dk(&["logs", "--tail", "60", &name]).unwrap_or_else(|x| x);
        let asked_wrong = asked(&wrong, "store");
        panic!(
            "a wrong password was accepted (status {}): {e}\nthe helper was asked for: {asked_wrong:?}\nthe registry's log:\n{logs}",
            out.0
        );
    }
    no_secret(&e);
    assert!(e.contains("the registry refused the credential"), "{e}");

    // --from-docker: an image of the local Docker whose registry (127.0.0.1:1) answers nothing
    let tag = format!("127.0.0.1:1/hydo-test-from-docker-{}:1", std::process::id());
    let ar = d.join("load.tar");
    docker_archive(&ar, &tag, "#!/bin/sh\necho saved\n");
    dk(&["load", "-q", "-i", &ar.display().to_string()]).unwrap();
    let _rmi = Cleanup(
        docker(&d).unwrap(),
        vec!["rmi".into(), "-f".into(), tag.clone()],
    );
    let fd = d.join("fd");
    let empty = no_credentials(&fd);
    let (out, e) = cli_import(&fd, &tag, &empty, &[]);
    assert_eq!(out.0, 1, "without --from-docker: {e}");
    assert!(e.contains("--from-docker"), "{e}");
    let (out, e) = cli_import(&fd, &tag, &empty, &["--from-docker"]);
    assert_eq!(out.0, 0, "{e}");
    assert!(
        e.contains("saving it from the local Docker instead (--from-docker)"),
        "{e}"
    );
    let lock: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fd.join("shop/hydo-compose.lock.json")).unwrap(),
    )
    .unwrap();
    let entry = &lock["images"][&tag];
    assert_eq!(entry["from_docker"], true, "{lock}");
    let (arch, file) = if cfg!(target_os = "macos") {
        ("arm64", "arm64-oci")
    } else {
        ("amd64", "amd64.docker.tar")
    };
    let pin = entry["platforms"][arch]["digest"]
        .as_str()
        .unwrap()
        .to_string();
    let stem = format!("127.0.0.1-1-hydo-test-from-docker-{}-1", std::process::id());
    let p = fd.join(format!("shop/images/{stem}-{file}"));
    assert!(p.exists(), "{}", p.display());
    let m = std::fs::read_to_string(fd.join("shop/hydo.yaml")).unwrap();
    assert!(
        m.contains("saved from the local Docker (--from-docker)"),
        "{m}"
    );
    assert!(m.contains(&format!("{stem}-arm64-oci")), "{m}");
    // a second import needs neither the registry nor Docker
    let (out, e) = cli_import(&fd, &tag, &empty, &[]);
    assert_eq!(out.0, 0, "{e}");
    // fetch-images.sh saves a missing file again and finds it as pinned
    std::fs::remove_file(&p).unwrap();
    let (rc, e) = fetch(&fd, &empty, &[]);
    assert_eq!(rc, 0, "{e}");
    assert!(
        e.contains(&format!(
            "saved from the local Docker {tag} for linux/{arch}, {pin}, as pinned"
        )),
        "{e}"
    );
    // another image under the same name stops at the pin, until HYDO_ACCEPT_REBUILD=1
    std::fs::remove_file(&p).unwrap();
    let ar2 = d.join("load2.tar");
    docker_archive(&ar2, &tag, "#!/bin/sh\necho saved again\n");
    dk(&["load", "-q", "-i", &ar2.display().to_string()]).unwrap();
    let (rc, e) = fetch(&fd, &empty, &[]);
    assert_eq!(rc, 1, "{e}");
    assert!(
        e.contains(&format!("but hydo-compose.lock.json pins {pin}")),
        "{e}"
    );
    assert!(!p.exists());
    let (rc, e) = fetch(&fd, &empty, &[(ACCEPT, "1")]);
    assert_eq!(rc, 0, "{e}");
    assert!(p.exists());
}
