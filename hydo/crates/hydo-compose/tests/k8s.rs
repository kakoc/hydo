//! `hydo k8s` end to end without a registry (Milestone 243): two sets of manifests become the
//! hydo.yaml recorded in tests/golden/ (HYDO_BLESS=1 records it again), the written manifest is
//! one hydo-build accepts, and no value of a Secret reaches it.
//! - tests/fixtures/k8s/voting/: the Kubernetes manifests of Docker's example voting app
//!   (dockersamples/example-voting-app, k8s-specifications/, Apache-2.0), as they are published;
//! - tests/fixtures/k8s/cluster.yaml: a small rendered chart written for this test (a StatefulSet
//!   of three replicas, ConfigMaps, a Secret, an init container, a Helm test, an Ingress).

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

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/k8s")
}

fn copy_dir(a: &Path, b: &Path) {
    std::fs::create_dir_all(b).unwrap();
    for e in std::fs::read_dir(a).unwrap() {
        let e = e.unwrap();
        if e.path().is_dir() {
            copy_dir(&e.path(), &b.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), b.join(e.file_name())).unwrap();
        }
    }
}

/// Import `input` (under a scratch folder) into `<scratch>/hydo/hydo.yaml`; the scratch folder.
fn import(tag: &str, input: &str, extra: &[&str]) -> TmpDir {
    let d =
        TmpDir::fresh(std::env::temp_dir().join(format!("hydo-k8s-{}-{tag}", std::process::id())));
    copy_dir(&fixtures(), &d.join("m"));
    let mut a: Vec<String> = vec![
        d.join("m").join(input).display().to_string(),
        "--out".into(),
        d.join("hydo/hydo.yaml").display().to_string(),
        "--no-resolve".into(),
    ];
    a.extend(extra.iter().map(|x| x.to_string()));
    assert_eq!(
        hydo_compose::k8s::main_with(&a, &mut Refuses, Some(1794096000)),
        0
    );
    d
}

fn golden(name: &str, text: &str) {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1") {
        std::fs::write(&p, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&p).unwrap_or_default();
    assert!(
        want == text,
        "{} differs from the import; HYDO_BLESS=1 cargo test -p hydo-compose --test it k8s:: records it again\n{text}",
        p.display()
    );
}

#[test]
fn the_voting_app_manifests() {
    let d = import("voting", "voting", &[]);
    let m = std::fs::read_to_string(d.join("hydo/hydo.yaml")).unwrap();
    golden("k8s-voting.yaml", &m);
    assert!(d.join("hydo/fetch-images.sh").is_file());
    // the vote Service sends port 8080 to the container's 80: said once
    assert!(
        m.contains("Service vote: port 8080 goes to the container's port 80"),
        "{m}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_rendered_chart_with_a_statefulset_and_secrets() {
    let d = import("cluster", "cluster.yaml", &[]);
    let m = std::fs::read_to_string(d.join("hydo/hydo.yaml")).unwrap();
    golden("k8s-cluster.yaml", &m);
    // the Secret's value is in the git-ignored folder only
    assert!(!m.contains("s3cr3t-password"), "{m}");
    let env = std::fs::read_to_string(d.join("hydo/k8s-import/secrets/kv-0.env")).unwrap();
    assert!(env.contains("KV_PASSWORD=s3cr3t-password\n"), "{env}");
    let gi = std::fs::read_to_string(d.join("hydo/k8s-import/secrets/.gitignore")).unwrap();
    assert!(gi.lines().any(|l| l == "*"));
    // the ConfigMap's script keeps its mode
    use std::os::unix::fs::PermissionsExt;
    let cm = std::fs::read_dir(d.join("hydo/k8s-import/configmap"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.join("start.sh").is_file())
        .expect("the ConfigMap's folder");
    let mode = std::fs::metadata(cm.join("start.sh"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755);
    // a group per StatefulSet, and the manifest is valid for hydo-build
    assert!(m.contains("groups:\n"), "{m}");
    let mut v = hydo_manifest::yaml::parse_yaml(&m, "hydo.yaml").unwrap();
    hydo_manifest::validate_data(&mut v).unwrap();
    // a second import gives the same file
    let a = std::fs::read_to_string(d.join("hydo/hydo.yaml")).unwrap();
    let d2 = import("cluster2", "cluster.yaml", &[]);
    let b = std::fs::read_to_string(d2.join("hydo/hydo.yaml")).unwrap();
    assert_eq!(
        a.replace(&*d.display().to_string(), ""),
        b.replace(&*d2.display().to_string(), "")
    );
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&d2);
}
