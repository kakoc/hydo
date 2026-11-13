//! Services built locally (`build:`), as most applications' Compose files have them (the Medusa
//! onboarding: `build: ./app` with `image: medusa-shop:hydo`). Until then `hydo compose` stopped
//! at a build without `image:`, tried to resolve a local tag at Docker Hub (HTTP 401), and with
//! `--no-resolve` wrote a fetch-images.sh that would pull the local tag from Docker Hub. Since
//! then such a service is imported as a local build: nothing is looked up or fetched, and the
//! manifest names the image file at its usual path.
//!
//! Since M214 the first try is one step: `hydo compose` builds the image with Docker (here a fake
//! Docker CLI that saves a prepared archive), reads its settings from the file, and pins its
//! digest in the lock file. fetch-images.sh builds it again where the file is missing, with
//! `hydo-fetch-image build`, and stops when the new build's digest differs from the pin, until
//! `HYDO_ACCEPT_REBUILD=1` takes it. Without Docker, both say what to install.

use hydo_compose::registry::Registry;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A registry that has no image (as Docker Hub answers for a tag that names only a local build).
struct Refuses;

impl Registry for Refuses {
    fn manifest(&mut self, _: &str, repo: &str, r: &str) -> Result<Vec<u8>, String> {
        Err(format!("HTTP 401 for {repo}:{r}"))
    }
    fn blob(&mut self, _: &str, repo: &str, d: &str) -> Result<Vec<u8>, String> {
        Err(format!("HTTP 401 for {repo}@{d}"))
    }
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-compose-local-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("shop")).unwrap();
    d
}

/// One import with the refusing registry; the warnings are in hydo.yaml's header comments.
fn compose(d: &Path, text: &str, args: &[&str]) -> (i32, String) {
    std::fs::write(d.join("shop/docker-compose.yml"), text).unwrap();
    let mut a = vec![d.join("shop/docker-compose.yml").display().to_string()];
    a.extend(args.iter().map(|x| x.to_string()));
    let rc = hydo_compose::main_with(&a, &mut Refuses);
    let m = std::fs::read_to_string(d.join("shop/hydo.yaml")).unwrap_or_default();
    (rc, m)
}

fn read(p: PathBuf) -> String {
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn executable(p: &Path, text: &str) {
    std::fs::write(p, text).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The guest architecture this machine builds (fetch-images.sh's default).
fn host() -> (&'static str, &'static str) {
    if cfg!(target_os = "macos") {
        ("arm64", "arm64")
    } else {
        ("x86_64", "amd64")
    }
}

/// The image file of `stem` for this machine, relative to shop/.
fn host_file(stem: &str) -> String {
    if host().0 == "arm64" {
        format!("images/{stem}-arm64-oci")
    } else {
        format!("images/{stem}-amd64.docker.tar")
    }
}

/// A `docker save` archive of one image with no layers, for `arch` (amd64 or arm64). `created`
/// changes the bytes without changing the settings; `cmd` changes the settings.
fn archive(path: &Path, arch: &str, created: &str, cmd: &str) -> String {
    let cfg = serde_json::json!({
        "architecture": arch,
        "os": "linux",
        "created": created,
        "config": {
            "Cmd": [cmd],
            "ExposedPorts": {"8080/tcp": {}},
            "Healthcheck": {"Test": ["CMD", "/app", "--health"]},
        },
        "rootfs": {"type": "layers", "diff_ids": []},
    })
    .to_string();
    let hex: String = Sha256::digest(cfg.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let manifest = serde_json::json!([{
        "Config": format!("{hex}.json"),
        "RepoTags": ["shop-app:latest"],
        "Layers": [],
    }])
    .to_string();
    let mut b = tar::Builder::new(std::fs::File::create(path).unwrap());
    for (name, data) in [
        ("manifest.json".to_string(), manifest),
        (format!("{hex}.json"), cfg),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, &name, data.as_bytes()).unwrap();
    }
    b.finish().unwrap();
    format!("sha256:{hex}")
}

/// A fake Docker CLI in d/bin: `version` answers, `build` records its arguments in d/built,
/// `save -o FILE` copies d/img-<variant>-<arch>.tar (the variant in d/variant). The archives of
/// variants a (the first build), b (other bytes, the same settings) and c (other settings) are
/// made for both architectures; the digests of this machine's architecture are returned.
fn fake_docker(d: &Path) -> (PathBuf, [String; 3]) {
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mut dg = Vec::new();
    for (v, created, cmd) in [
        ("a", "2026-01-01T00:00:00Z", "/app"),
        ("b", "2026-01-02T00:00:00Z", "/app"),
        ("c", "2026-01-03T00:00:00Z", "/app2"),
    ] {
        for a in ["amd64", "arm64"] {
            let x = archive(&d.join(format!("img-{v}-{a}.tar")), a, created, cmd);
            if a == host().1 {
                dg.push(x);
            }
        }
    }
    std::fs::write(d.join("variant"), "a").unwrap();
    let f = bin.join("docker");
    executable(
        &f,
        &format!(
            "#!/bin/sh\nd={}\ncase \"$1\" in\nversion) exit 0;;\nbuild) echo \"$*\" >> \"$d/built\"; echo \"$3\" > \"$d/plat\"; exit 0;;\nsave) p=$(cat \"$d/plat\"); cp \"$d/img-$(cat \"$d/variant\")-${{p#linux/}}.tar\" \"$3\"; exit 0;;\nesac\nexit 1\n",
            d.display()
        ),
    );
    (f, [dg[0].clone(), dg[1].clone(), dg[2].clone()])
}

/// The variables of the generated fetch-images.sh. They are the script's, not hydo's: hydo never
/// reads them, so hydo-config's inventory leaves them out, as it does every shell script's.
const FETCH_IMAGE: &str = "HYDO_FETCH_IMAGE";
const DOCKER: &str = "HYDO_DOCKER";
const ACCEPT: &str = "HYDO_ACCEPT_REBUILD";

/// sh shop/fetch-images.sh with the real hydo-fetch-image and `env`: (status, stderr).
fn fetch(d: &Path, env: &[(&str, &str)]) -> (i32, String) {
    let mut c = Command::new("sh");
    c.arg(d.join("shop/fetch-images.sh"))
        .env(FETCH_IMAGE, env!("CARGO_BIN_EXE_hydo-fetch-image"))
        .env_remove(DOCKER)
        .env_remove(ACCEPT);
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn lock(d: &Path) -> serde_json::Value {
    serde_json::from_str(&read(d.join("shop/hydo-compose.lock.json"))).unwrap()
}

const APP: &str = "services:\n  app:\n    build:\n      context: ./app\n      dockerfile: Dockerfile.prod\n      target: run\n      args:\n        VERSION: \"1.2\"\n        FROM_ENV:\n      labels: [team=shop]\n      ssh: [default]\n";

#[test]
fn a_build_without_image_is_a_local_build_named_as_compose_names_it() {
    let d = dir("noimage");
    let (rc, err) = compose(
        &d,
        "services:\n  app:\n    build: ./app\n    ports: [\"9000:9000\"]\n  db:\n    image: postgres:16\n",
        &["--no-resolve", "--no-build"],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(
        err.contains(
            "image shop-app:latest is built locally (build:) and not built now (--no-build)"
        ),
        "{err}"
    );
    let m = read(d.join("shop/hydo.yaml"));
    assert!(m.contains("image: {x86_64: images/shop-app-latest-amd64.docker.tar, arm64: images/shop-app-latest-arm64-oci}"), "{m}");
    let f = read(d.join("shop/fetch-images.sh"));
    assert!(f.contains("\"$F\" build --docker \"${HYDO_DOCKER:-}\" --accept \"${HYDO_ACCEPT_REBUILD:-0}\" x86_64 images/shop-app-latest-amd64.docker.tar shop-app:latest hydo-compose.lock.json shop-app -- app || LOCAL=1"), "{f}");
    assert!(
        f.contains(" arm64 images/shop-app-latest-arm64-oci shop-app:latest "),
        "{f}"
    );
    // the local build is never fetched; the other image still is
    assert!(!f.contains("\"$F\" docker.io/library/shop-app"), "{f}");
    assert!(f.contains("\"$F\" docker.io/library/postgres"), "{f}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_local_tag_is_not_resolved_and_fetch_images_builds_the_file() {
    let d = dir("tag");
    let (rc, err) = compose(
        &d,
        "services:\n  medusa:\n    build:\n      context: ./app\n      dockerfile: Dockerfile.prod\n      target: run\n    image: medusa-shop:hydo\n",
        &["--no-build"],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(
        err.contains("image medusa-shop:hydo is not in its registry"),
        "{err}"
    );
    let f = read(d.join("shop/fetch-images.sh"));
    assert!(f.contains("x86_64 images/medusa-shop-hydo-amd64.docker.tar medusa-shop:hydo hydo-compose.lock.json medusa-shop:hydo -- -f app/Dockerfile.prod --target run app || LOCAL=1"), "{f}");
    // nothing pinned the image, so the lock file has no entry for it
    let lock = std::fs::read_to_string(d.join("shop/hydo-compose.lock.json")).unwrap_or_default();
    assert!(!lock.contains("medusa-shop"), "{lock}");
    // run while the file is missing and no Docker CLI is there: it says what to install and the
    // commands to make the file by hand, fetches nothing, and stops at the end
    let (rc, e) = fetch(&d, &[(DOCKER, "/nonexistent/docker")]);
    assert_eq!(rc, 1, "{e}");
    assert!(
        e.contains("no Docker CLI was found (tried /nonexistent/docker)"),
        "{e}"
    );
    assert!(e.contains("install Docker Engine on Linux"), "{e}");
    assert!(e.contains("-t medusa-shop:hydo -f app/Dockerfile.prod --target run app && docker save -o images/medusa-shop-hydo-"), "{e}");
    assert!(e.contains("an image built locally is missing"), "{e}");
    // with this machine's file there, it passes
    let hf = d.join("shop").join(host_file("medusa-shop-hydo"));
    std::fs::create_dir_all(hf.parent().unwrap()).unwrap();
    if host().0 == "arm64" {
        std::fs::create_dir_all(&hf).unwrap();
    } else {
        std::fs::write(&hf, "").unwrap();
    }
    let (rc, e) = fetch(&d, &[(DOCKER, "/nonexistent/docker")]);
    assert_eq!(rc, 0, "{e}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn hydo_compose_builds_the_image_reads_its_settings_and_pins_it() {
    let d = dir("build");
    let (docker, [a, _, _]) = fake_docker(&d);
    let docker = docker.display().to_string();
    let (rc, m) = compose(&d, APP, &["--no-resolve", "--docker", &docker]);
    assert_eq!(rc, 0, "{m}");
    // Compose's build settings, for this machine's guest; ssh: is not passed, with a warning
    let built = read(d.join("built"));
    let plat = format!("linux/{}", host().1);
    assert_eq!(
        built.trim(),
        format!("build --platform {plat} -t shop-app:latest -f app/Dockerfile.prod --target run --build-arg VERSION=1.2 --build-arg FROM_ENV --label team=shop app")
    );
    assert!(m.contains("build.ssh: not passed to docker build"), "{m}");
    // the image's settings come from the file: EXPOSE 8080 and its HEALTHCHECK
    assert!(m.contains("port: 8080"), "{m}");
    assert!(m.contains("/app --health"), "{m}");
    assert!(
        m.contains(&format!(
            "pinned by its content in hydo-compose.lock.json\n  #   linux/{}: {a}",
            host().1
        )),
        "{m}"
    );
    assert!(!m.contains("NOT pinned"), "{m}");
    let l = lock(&d);
    assert_eq!(l["images"]["shop-app"]["local_build"], true);
    assert_eq!(
        l["images"]["shop-app"]["platforms"][host().1]["digest"],
        a.as_str()
    );
    assert!(d.join("shop").join(host_file("shop-app-latest")).exists());
    // a second import changes nothing and builds nothing
    let before = (
        read(d.join("shop/hydo.yaml")),
        read(d.join("shop/hydo-compose.lock.json")),
    );
    std::fs::remove_file(d.join("built")).unwrap();
    let (rc, _) = compose(&d, APP, &["--no-resolve", "--docker", &docker]);
    assert_eq!(rc, 0);
    assert!(!d.join("built").exists());
    let after = (
        read(d.join("shop/hydo.yaml")),
        read(d.join("shop/hydo-compose.lock.json")),
    );
    assert_eq!(before, after);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn fetch_images_rebuilds_a_missing_file_and_stops_at_other_bytes() {
    let d = dir("rebuild");
    let (docker, [a, b, c]) = fake_docker(&d);
    let docker = docker.display().to_string();
    let (rc, m) = compose(&d, APP, &["--no-resolve", "--docker", &docker]);
    assert_eq!(rc, 0, "{m}");
    let file = d.join("shop").join(host_file("shop-app-latest"));
    let env = [(DOCKER, docker.as_str())];
    // a fresh checkout: the file is missing; the same bytes again pass
    std::fs::remove_file(&file).unwrap();
    let (rc, e) = fetch(&d, &env);
    assert_eq!(rc, 0, "{e}");
    assert!(
        e.contains(&format!("{a}, as pinned in hydo-compose.lock.json")),
        "{e}"
    );
    assert!(file.exists());
    // other bytes with the same settings: stopped, kept beside, the lock unchanged
    std::fs::remove_file(&file).unwrap();
    std::fs::write(d.join("variant"), "b").unwrap();
    let (rc, e) = fetch(&d, &env);
    assert_eq!(rc, 1, "{e}");
    assert!(
        e.contains(&format!(
            "built to {b}, but hydo-compose.lock.json pins {a}"
        )),
        "{e}"
    );
    assert!(e.contains("its settings (entrypoint, command, environment, ports, healthcheck, volumes, user) are the same"), "{e}");
    assert!(
        e.contains("HYDO_ACCEPT_REBUILD=1 sh fetch-images.sh"),
        "{e}"
    );
    assert!(!file.exists());
    let kept = PathBuf::from(format!("{}.rebuilt", file.display()));
    assert!(kept.exists());
    assert_eq!(
        lock(&d)["images"]["shop-app"]["platforms"][host().1]["digest"],
        a.as_str()
    );
    // accepted: in place and re-pinned
    let (rc, e) = fetch(&d, &[(DOCKER, docker.as_str()), (ACCEPT, "1")]);
    assert_eq!(rc, 0, "{e}");
    assert!(e.contains("accepted"), "{e}");
    assert!(file.exists());
    assert_eq!(
        lock(&d)["images"]["shop-app"]["platforms"][host().1]["digest"],
        b.as_str()
    );
    // other settings: the message says so, and accepting asks for hydo compose again
    std::fs::remove_file(&file).unwrap();
    std::fs::write(d.join("variant"), "c").unwrap();
    let (rc, e) = fetch(&d, &[(DOCKER, docker.as_str()), (ACCEPT, "1")]);
    assert_eq!(rc, 0, "{e}");
    assert!(e.contains("changed too"), "{e}");
    assert!(e.contains("run hydo compose again"), "{e}");
    assert_eq!(
        lock(&d)["images"]["shop-app"]["platforms"][host().1]["digest"],
        c.as_str()
    );
    // hydo compose follows the file, which now matches the lock: no warning about it
    let (rc, m) = compose(&d, APP, &["--no-resolve", "--docker", &docker]);
    assert_eq!(rc, 0, "{m}");
    assert!(!m.contains("re-pinned"), "{m}");
    assert!(m.contains(&c), "{m}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_file_replaced_by_hand_is_re_pinned_with_a_warning() {
    let d = dir("byhand");
    let (docker, [a, b, _]) = fake_docker(&d);
    let docker = docker.display().to_string();
    let (rc, _) = compose(&d, APP, &["--no-resolve", "--docker", &docker]);
    assert_eq!(rc, 0);
    let file = d.join("shop").join(host_file("shop-app-latest"));
    std::fs::copy(d.join(format!("img-b-{}.tar", host().1)), &file).unwrap();
    let (rc, m) = compose(&d, APP, &["--no-resolve", "--docker", &docker]);
    assert_eq!(rc, 0, "{m}");
    assert!(
        m.contains(&format!(
            "is {b}, but hydo-compose.lock.json pinned {a}: re-pinned to the file"
        )),
        "{m}"
    );
    assert_eq!(
        lock(&d)["images"]["shop-app"]["platforms"][host().1]["digest"],
        b.as_str()
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn without_docker_the_import_says_what_to_install_and_the_script_pins_later() {
    let d = dir("nodocker");
    let (rc, m) = compose(
        &d,
        APP,
        &["--no-resolve", "--docker", "/nonexistent/docker"],
    );
    assert_eq!(rc, 0, "{m}");
    assert!(m.contains("is not pinned, because no Docker CLI was found (tried /nonexistent/docker): install Docker Engine on Linux"), "{m}");
    assert!(m.contains("NOT pinned yet"), "{m}");
    // with Docker there later, fetch-images.sh builds the file and records the pin itself
    let (docker, [a, _, _]) = fake_docker(&d);
    let (rc, e) = fetch(&d, &[(DOCKER, &docker.display().to_string())]);
    assert_eq!(rc, 0, "{e}");
    assert!(
        e.contains(&format!("{a}; pinned in hydo-compose.lock.json")),
        "{e}"
    );
    assert!(
        e.contains("run hydo compose again, so that hydo.yaml takes the image's settings"),
        "{e}"
    );
    let l = lock(&d);
    assert_eq!(l["images"]["shop-app"]["local_build"], true);
    assert_eq!(
        l["images"]["shop-app"]["platforms"][host().1]["digest"],
        a.as_str()
    );
    // and the next import reads the settings from the file
    let (rc, m) = compose(
        &d,
        APP,
        &["--no-resolve", "--docker", "/nonexistent/docker"],
    );
    assert_eq!(rc, 0, "{m}");
    assert!(m.contains("port: 8080"), "{m}");
    let _ = std::fs::remove_dir_all(&d);
}
