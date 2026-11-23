//! Image pinning without skopeo: the registry answers for Docker's voting app (recorded once from
//! the registries by `record_voting_registry`, an ignored test that needs the network) give the
//! same lock entries as hydo-compose.lock.json, which the Python importer wrote with skopeo
//! (M84/M95): the same index digests, per-platform manifest digests, variants and image configs.
//!
//! The answers are recorded by digest (`<repo>@<index digest>`), because the tags have moved since
//! the lock was written; `voting_lock_from_registry` also replays the tag requests (the recorded
//! index under the tag's name, the registry as it was when the lock was written) and checks that a
//! run without a lock writes hydo-compose.lock.json byte for byte.

use crate::tmpdir::TmpDir;
use hydo_compose::pyval::{from_json, Y};
use hydo_compose::registry::{resolve_image, Http, Recorded, Recording, Registry};
use std::path::{Path, PathBuf};

fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/registry")
}

fn voting() -> PathBuf {
    top().join("hydo/onboard/examples/compose-voting")
}

fn lock() -> Y {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(voting().join("hydo-compose.lock.json")).unwrap())
            .unwrap();
    from_json(&v)
}

/// The by-digest reference of a lock entry.
fn by_digest(e: &Y) -> String {
    let s = |k: &str| hydo_compose::pyval::pstr(e.getn(k));
    format!("{}/{}@{}", s("registry"), s("repo"), s("index_digest"))
}

#[test]
#[ignore = "needs the network: records tests/fixtures/registry from the registries"]
fn record_voting_registry() {
    let mut http = Http::new();
    let mut rec = Recording(&mut http as &mut dyn Registry, fixtures());
    for (_, e) in lock().getn("images").map_items() {
        resolve_image(&mut rec, &by_digest(e)).unwrap();
    }
}

#[test]
fn voting_digests_equal_the_lock() {
    let mut reg = Recorded(fixtures());
    let mut n = 0;
    for (r, e) in lock().getn("images").map_items() {
        let got = resolve_image(&mut reg, &by_digest(e)).unwrap();
        for k in ["name", "index_digest", "registry", "repo", "platforms"] {
            assert_eq!(
                got.getn(k),
                e.getn(k),
                "{}: {k}",
                hydo_compose::pyval::pstr(r)
            );
        }
        n += e.getn("platforms").map_items().len();
    }
    assert_eq!(n, 10, "5 images x 2 platforms");
}

/// A registry that answers a tag with the index recorded under its digest (the tag as it was).
struct AtLockTime(Recorded, Vec<(String, String)>);

impl Registry for AtLockTime {
    fn manifest(&mut self, registry: &str, repo: &str, reference: &str) -> Result<Vec<u8>, String> {
        let r = self
            .1
            .iter()
            .find(|(k, _)| *k == format!("{registry}/{repo}:{reference}"))
            .map(|(_, d)| d.clone())
            .unwrap_or_else(|| reference.to_string());
        self.0.manifest(registry, repo, &r)
    }

    fn blob(&mut self, registry: &str, repo: &str, digest: &str) -> Result<Vec<u8>, String> {
        self.0.blob(registry, repo, digest)
    }
}

#[test]
fn voting_lock_from_registry() {
    let mut tags = Vec::new();
    for (_, e) in lock().getn("images").map_items() {
        let s = |k: &str| hydo_compose::pyval::pstr(e.getn(k));
        tags.push((
            format!("{}/{}:{}", s("registry"), s("repo"), s("tag")),
            s("index_digest"),
        ));
    }
    let d = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-compose-reg-{}", std::process::id())),
    );
    std::fs::create_dir_all(d.join("proj/healthchecks")).unwrap();
    std::fs::copy(
        voting().join("docker-compose.yml"),
        d.join("proj/docker-compose.yml"),
    )
    .unwrap();
    for f in std::fs::read_dir(voting().join("healthchecks")).unwrap() {
        let f = f.unwrap();
        std::fs::copy(f.path(), d.join("proj/healthchecks").join(f.file_name())).unwrap();
    }
    let p = |x: &str| d.join(x).display().to_string();
    let mut reg = AtLockTime(Recorded(fixtures()), tags);
    let rc = hydo_compose::main_with(
        &[
            p("proj/docker-compose.yml"),
            "--out".into(),
            p("proj/hydo.yaml"),
            "--name".into(),
            "compose-voting".into(),
        ],
        &mut reg,
    );
    assert_eq!(rc, 0);
    let got = std::fs::read(d.join("proj/hydo-compose.lock.json")).unwrap();
    let want = std::fs::read(voting().join("hydo-compose.lock.json")).unwrap();
    assert!(
        got == want,
        "the lock written from the registry differs from the voting app's lock"
    );
    // and a second run reads that lock: the same manifest, no registry needed
    let first = std::fs::read(d.join("proj/hydo.yaml")).unwrap();
    let mut none = Recorded(d.join("no-such-dir"));
    let rc = hydo_compose::main_with(
        &[
            p("proj/docker-compose.yml"),
            "--out".into(),
            p("proj/hydo.yaml"),
            "--name".into(),
            "compose-voting".into(),
        ],
        &mut none,
    );
    assert_eq!(rc, 0);
    assert_eq!(std::fs::read(d.join("proj/hydo.yaml")).unwrap(), first);
    let _ = std::fs::remove_dir_all(&d);
}

/// A registry of one image whose index has only a linux/arm64 build, as an image pushed from a
/// Mac has. Every digest is the sha256 of its bytes, which the resolution checks.
struct ArmOnly;

fn arm_config() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64",
        "config": {"Cmd": ["/app", "serve"], "User": "app", "ExposedPorts": {"8080/tcp": {}}},
    }))
    .unwrap()
}

fn arm_manifest() -> Vec<u8> {
    let cd = format!(
        "sha256:{}",
        hydo_compose::registry::sha256_hex(&arm_config())
    );
    serde_json::to_vec(&serde_json::json!({
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {"digest": cd},
    }))
    .unwrap()
}

impl Registry for ArmOnly {
    fn manifest(&mut self, _: &str, _: &str, reference: &str) -> Result<Vec<u8>, String> {
        let md = format!(
            "sha256:{}",
            hydo_compose::registry::sha256_hex(&arm_manifest())
        );
        if reference == md {
            return Ok(arm_manifest());
        }
        Ok(serde_json::to_vec(&serde_json::json!({
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [{"digest": md, "platform": {"os": "linux", "architecture": "arm64"}}],
        }))
        .unwrap())
    }
    fn blob(&mut self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, String> {
        Ok(arm_config())
    }
}

/// An image with only an arm64 build is imported on every machine (before, hydo compose refused
/// it with "no linux/amd64 image", also on a Mac): its settings come from the arm64 build, the
/// import warns, and fetch-images.sh stops on an x86_64 machine with the reason.
#[test]
fn an_arm64_only_image_is_imported() {
    let got = resolve_image(&mut ArmOnly, "registry.example.test/me/app:1").unwrap();
    assert!(got.getn("platforms").has("arm64"));
    assert!(!got.getn("platforms").has("amd64"));

    let d = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-compose-arm-{}", std::process::id())),
    );
    std::fs::create_dir_all(d.join("proj")).unwrap();
    std::fs::write(
        d.join("proj/docker-compose.yml"),
        "services:\n  app:\n    image: registry.example.test/me/app:1\n",
    )
    .unwrap();
    let p = |x: &str| d.join(x).display().to_string();
    let rc = hydo_compose::main_with(
        &[
            p("proj/docker-compose.yml"),
            "--out".into(),
            p("proj/hydo.yaml"),
        ],
        &mut ArmOnly,
    );
    assert_eq!(rc, 0);
    let yaml = std::fs::read_to_string(d.join("proj/hydo.yaml")).unwrap();
    assert!(yaml.contains("arm64:"), "{yaml}");
    assert!(yaml.contains("8080"), "the arm64 build's EXPOSE: {yaml}");
    assert!(yaml.contains("/app"), "the arm64 build's Cmd: {yaml}");
    let fetch = std::fs::read_to_string(d.join("proj/fetch-images.sh")).unwrap();
    assert!(
        fetch.contains("has no linux/amd64 build, so it does not run on this machine"),
        "{fetch}"
    );
    assert!(!fetch.contains("NOT pinned"), "{fetch}");
}
