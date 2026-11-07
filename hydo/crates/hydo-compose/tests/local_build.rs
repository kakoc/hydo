//! Services built locally (`build:`), as most applications' Compose files have them (the Medusa
//! onboarding: `build: ./app` with `image: medusa-shop:hydo`). Until then `hydo compose` stopped
//! at a build without `image:`, tried to resolve a local tag at Docker Hub (HTTP 401), and with
//! `--no-resolve` wrote a fetch-images.sh that would pull the local tag from Docker Hub. Now such
//! a service is imported as a local build: nothing is looked up or fetched, the manifest names
//! the image file at its usual path, and fetch-images.sh says how to make it with Docker (or an
//! OCI layout) and stops while it is missing.

use hydo_compose::registry::Registry;
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

#[test]
fn a_build_without_image_is_a_local_build_named_as_compose_names_it() {
    let d = dir("noimage");
    let (rc, err) = compose(
        &d,
        "services:\n  app:\n    build: ./app\n    ports: [\"9000:9000\"]\n  db:\n    image: postgres:16\n",
        &["--no-resolve"],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(
        err.contains("image shop-app is built locally (build:)"),
        "{err}"
    );
    let m = read(d.join("shop/hydo.yaml"));
    assert!(m.contains("image: {x86_64: images/shop-app-latest-amd64.docker.tar, arm64: images/shop-app-latest-arm64-oci}"), "{m}");
    let f = read(d.join("shop/fetch-images.sh"));
    assert!(f.contains("#   docker build -t shop-app:latest app && docker save -o images/shop-app-latest-amd64.docker.tar shop-app:latest"), "{f}");
    // the local build is never fetched; the other image still is
    assert!(!f.contains("\"$F\" docker.io/library/shop-app"), "{f}");
    assert!(f.contains("\"$F\" docker.io/library/postgres"), "{f}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_local_tag_is_not_resolved_and_fetch_images_says_how_to_make_the_file() {
    let d = dir("tag");
    let (rc, err) = compose(
        &d,
        "services:\n  medusa:\n    build:\n      context: ./app\n      dockerfile: Dockerfile.prod\n      target: run\n    image: medusa-shop:hydo\n",
        &[],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(
        err.contains("image medusa-shop:hydo is not in its registry"),
        "{err}"
    );
    let f = read(d.join("shop/fetch-images.sh"));
    assert!(f.contains("docker build -t medusa-shop:hydo -f app/Dockerfile.prod --target run app && docker save -o images/medusa-shop-hydo-amd64.docker.tar medusa-shop:hydo"), "{f}");
    assert!(f.contains("dest=images/medusa-shop-hydo-arm64-oci"), "{f}");
    // nothing pinned the image, so the lock file has no entry for it
    let lock = std::fs::read_to_string(d.join("shop/hydo-compose.lock.json")).unwrap_or_default();
    assert!(!lock.contains("medusa-shop"), "{lock}");
    // run while the file is missing: it stops with the command, without fetching anything
    let o = Command::new("sh")
        .arg(d.join("shop/fetch-images.sh"))
        .env("HYDO_FETCH_IMAGE", "/bin/false")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("images/medusa-shop-hydo-amd64.docker.tar is built locally (build:): docker build -t medusa-shop:hydo"), "{e}");
    // with the file there (an OCI layout for arm64 counts too), it passes
    std::fs::create_dir_all(d.join("shop/images/medusa-shop-hydo-arm64-oci")).unwrap();
    let o = Command::new("sh")
        .arg(d.join("shop/fetch-images.sh"))
        .env("HYDO_FETCH_IMAGE", "/bin/false")
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let _ = std::fs::remove_dir_all(&d);
}
