//! `hydo compose --upgrade-to <service>=<image>` (Milestone 392): the image of another version of
//! a service is pinned in the lock file and fetched by fetch-images.sh as the service's own, and
//! the manifest names it as a version of the service. The import reads the lock file of the
//! example `vikunja/upgrade` (2.7.1 and 2.8.0), so no registry is asked, and it gives the
//! example's fetch-images.sh and lock file again.

use crate::tmpdir::TmpDir;
use hydo_compose::registry::Registry;
use std::path::{Path, PathBuf};

struct Refuses;

impl Registry for Refuses {
    fn manifest(&mut self, _: &str, repo: &str, r: &str) -> Result<Vec<u8>, String> {
        Err(format!("no registry here: {repo}:{r}"))
    }
    fn blob(&mut self, _: &str, repo: &str, d: &str) -> Result<Vec<u8>, String> {
        Err(format!("no registry here: {repo}@{d}"))
    }
}

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../onboard/examples/vikunja/upgrade")
}

fn import(d: &Path, extra: &[&str]) -> i32 {
    std::fs::create_dir_all(d.join("hydo")).unwrap();
    std::fs::copy(
        example().join("docker-compose.yaml"),
        d.join("docker-compose.yaml"),
    )
    .unwrap();
    std::fs::copy(
        example().join("hydo/hydo-compose.lock.json"),
        d.join("hydo/hydo-compose.lock.json"),
    )
    .unwrap();
    let mut a = vec![
        d.join("docker-compose.yaml").display().to_string(),
        "--out".into(),
        d.join("hydo/hydo.yaml").display().to_string(),
    ];
    a.extend(extra.iter().map(|x| x.to_string()));
    hydo_compose::main_with_clock(&a, &mut Refuses, Some(0))
}

#[test]
fn a_version_of_a_service_is_pinned_and_fetched_as_its_image() {
    let d = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-compose-upgrade-to-{}", std::process::id())),
    );
    assert_eq!(
        import(&d, &["--upgrade-to", "vikunja=vikunja/vikunja:2.8.0"]),
        0
    );
    let m = std::fs::read_to_string(d.join("hydo/hydo.yaml")).unwrap();
    assert!(m.contains("    version: \"2.7.1\""), "{m}");
    assert!(m.contains("      - name: \"2.8.0\"\n        image: {x86_64: images/vikunja-vikunja-2.8.0-amd64.docker.tar, arm64: images/vikunja-vikunja-2.8.0-arm64-oci}\n        image_ref: \"vikunja/vikunja:2.8.0\"\n"), "{m}");
    // the example's files are what the import writes
    for f in ["fetch-images.sh", "hydo-compose.lock.json"] {
        assert_eq!(
            std::fs::read_to_string(d.join("hydo").join(f)).unwrap(),
            std::fs::read_to_string(example().join("hydo").join(f)).unwrap(),
            "{f}"
        );
    }
    let fetch = std::fs::read_to_string(d.join("hydo/fetch-images.sh")).unwrap();
    for digest in [
        "sha256:3a848c83404d59ba9df4d91b92af4c400c8876ccdbbf3b563d41019cee29df39",
        "sha256:12c860da57e1d354d3a75f01ec6b88acb35ea3a38b213a1b9ecab339d59aa243",
    ] {
        assert!(fetch.contains(digest), "{fetch}");
    }
    // a service the Compose file does not have, and a version without a tag, are refused
    let e = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-compose-upgrade-to-{}-no", std::process::id())),
    );
    assert_eq!(
        import(&e, &["--upgrade-to", "web=vikunja/vikunja:2.8.0"]),
        1
    );
    assert_eq!(import(&e, &["--upgrade-to", "vikunja"]), 2);
    // without the option the manifest has no versions
    let f = TmpDir::fresh(std::env::temp_dir().join(format!(
        "hydo-compose-upgrade-to-{}-off",
        std::process::id()
    )));
    assert_eq!(import(&f, &[]), 0);
    let m = std::fs::read_to_string(f.join("hydo/hydo.yaml")).unwrap();
    assert!(!m.contains("versions:"), "{m}");
}
