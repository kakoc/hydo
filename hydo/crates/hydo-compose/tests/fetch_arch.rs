//! fetch-images.sh fetches only the architecture the machine builds (Milestone 196): x86_64 on
//! Linux, arm64 on a Mac, or the one named as its argument (`all` for both). Before, it fetched
//! both, so a Linux user downloaded every arm64 image too (the YugabyteDB and libSQL
//! onboardings). The import reads the YugabyteDB example's lock file, so no registry is asked.

use hydo_compose::registry::Registry;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../onboard/examples/yugabyte")
}

/// The import in a fresh directory, and a fetcher that records what it was asked and makes the
/// file (or the OCI layout directory).
fn imported(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-compose-arch-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("p")).unwrap();
    std::fs::copy(
        example().join("docker-compose.yaml"),
        d.join("p/docker-compose.yaml"),
    )
    .unwrap();
    std::fs::copy(
        example().join("hydo/hydo-compose.lock.json"),
        d.join("p/hydo-compose.lock.json"),
    )
    .unwrap();
    let a = vec![
        d.join("p/docker-compose.yaml").display().to_string(),
        "--out".into(),
        d.join("p/hydo.yaml").display().to_string(),
    ];
    assert_eq!(hydo_compose::main_with_clock(&a, &mut Refuses, Some(0)), 0);
    std::fs::create_dir_all(d.join("bin")).unwrap();
    let f = d.join("bin/hydo-fetch-image");
    std::fs::write(
        &f,
        "#!/bin/sh\necho \"$4\" >> \"$(dirname \"$0\")/../asked\"\ncase \"$4\" in *-oci) mkdir -p \"$4\";; *) : > \"$4\";; esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    d
}

fn fetch(d: &Path, arg: Option<&str>) -> (i32, Vec<String>) {
    let _ = std::fs::remove_file(d.join("asked"));
    let _ = std::fs::remove_dir_all(d.join("p/images"));
    let mut c = Command::new("sh");
    c.arg(d.join("p/fetch-images.sh")).env(
        "PATH",
        format!(
            "{}:{}",
            d.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    if let Some(a) = arg {
        c.arg(a);
    }
    let o = c.output().unwrap();
    let asked = std::fs::read_to_string(d.join("asked")).unwrap_or_default();
    (
        o.status.code().unwrap_or(-1),
        asked.lines().map(str::to_string).collect(),
    )
}

#[test]
fn only_the_named_architecture_is_fetched() {
    let d = imported("named");
    let x86 = "images/yugabytedb-yugabyte-2025.2.8.1-b8-amd64.docker.tar";
    let arm = "images/yugabytedb-yugabyte-2025.2.8.1-b8-arm64-oci";
    assert_eq!(fetch(&d, Some("x86_64")), (0, vec![x86.to_string()]));
    assert_eq!(fetch(&d, Some("arm64")), (0, vec![arm.to_string()]));
    let (rc, mut all) = fetch(&d, Some("all"));
    all.sort();
    assert_eq!((rc, all), (0, vec![x86.to_string(), arm.to_string()]));
    assert_eq!(fetch(&d, Some("riscv")).0, 2);
    // without an argument: this machine's architecture
    let host = if cfg!(target_os = "macos") { arm } else { x86 };
    assert_eq!(fetch(&d, None), (0, vec![host.to_string()]));
    let _ = std::fs::remove_dir_all(&d);
}
