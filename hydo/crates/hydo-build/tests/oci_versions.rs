//! Versions of an image service (Milestone 392): each version of a service with `image:` names an
//! image of its own, unpacked into `/oci/<service>@<version>` with a launcher of its own, and the
//! version's `cmd` becomes that launcher. The service's mounts, workdir and user carry over, a
//! version's `env:` comes over the service's, and every version's root has the service's own
//! `/tmp`. The launchers of such a service write which tree runs, and `hydo-oci` reads it. An
//! image without versions keeps its scripts byte for byte (tests/oci.rs keeps the recorded
//! answers).

use crate::tmpdir::TmpDir;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn images(tmp: &Path) {
    let g = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/oci/images.tar.gz");
    std::fs::create_dir_all(tmp).unwrap();
    let st = Command::new("tar")
        .arg("-C")
        .arg(tmp)
        .arg("-xzf")
        .arg(g)
        .status()
        .unwrap();
    assert!(st.success());
}

/// prepare + stage of the manifest `m`; the manifest after prepare and the staged root.
fn stage(tmp: &Path, tag: &str, mut m: Value) -> (Value, PathBuf) {
    let mpath = tmp.join(format!("{tag}.json"));
    std::fs::write(&mpath, m.to_string()).unwrap();
    let rr = tmp.join(format!("root-{tag}"));
    std::fs::create_dir_all(rr.join("bin")).unwrap();
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let mut io = hydo_build::Io {
        out: &mut o,
        err: &mut e,
    };
    let mut svcs =
        hydo_build::oci::prepare(&mut m, &mpath.to_string_lossy(), "x86_64", &mut io).unwrap();
    hydo_build::oci::stage(
        &mut svcs,
        &rr,
        &tmp.join("busybox"),
        &tmp.join(format!("tmp-{tag}")),
        true,
        false,
        &mut io,
    )
    .unwrap_or_else(|f| panic!("{}", f.message()));
    (m, rr)
}

fn read(p: PathBuf) -> String {
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn each_version_has_its_own_tree_and_launcher() {
    // SAFETY: umask only sets the process's file creation mask.
    unsafe {
        libc::umask(0o022);
    }
    let tmp = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-build-oci-versions-{}", std::process::id())),
    );
    images(&tmp);
    std::fs::write(tmp.join("busybox"), b"#!/bin/sh\n").unwrap();
    let web = |versions: Value| {
        let mut s = json!({
            "name": "web",
            "image": "demo-amd64.docker.tar",
            "image_ref": "demo:1.0",
            "cmd": ["serve", "--port", "80"],
            "env": {"A": "1", "B": "2"},
            "ready": "true",
            "mounts": [{"src": "/data/vol/web", "dst": "/var/lib/web"}, {"tmpfs": true, "dst": "/run"}]
        });
        if !versions.is_null() {
            s["version"] = json!("1.0");
            s["versions"] = versions;
        }
        json!({"name": "up", "services": [s]})
    };
    let (m, rr) = stage(
        &tmp,
        "versions",
        web(json!([{"name": "2.0", "image": "demo-amd64.docker.tar.gz", "env": {"B": "3"}}])),
    );
    // the manifest: the service's cmd and the version's are their launchers
    let s = &m["services"][0];
    assert_eq!(s["cmd"], "/etc/hydo/oci/web/run");
    assert_eq!(s["versions"][0]["cmd"], "/etc/hydo/oci/web@2.0/run");
    assert!(s["versions"][0].get("image").is_none(), "{s}");
    // two trees, two launchers
    assert!(rr.join("oci/web/var/lib/web").is_dir());
    assert!(rr.join("oci/web@2.0/var/lib/web").is_dir());
    let run = read(rr.join("etc/hydo/oci/web/run"));
    let vrun = read(rr.join("etc/hydo/oci/web@2.0/run"));
    assert!(run.contains("echo web > /tmp/hydo/oci-ver.web"), "{run}");
    assert!(
        vrun.contains("echo web@2.0 > /tmp/hydo/oci-ver.web"),
        "{vrun}"
    );
    assert!(vrun.contains("R=/oci/web@2.0"), "{vrun}");
    // the version's root has the service's own /tmp and its own mounts' marker
    assert!(vrun.contains("/tmp/oci/web "), "{vrun}");
    assert!(vrun.contains("/tmp/hydo/oci-mnt.web@2.0.done"), "{vrun}");
    assert!(vrun.contains("/tmp/hydo/oci-tmpfs/web@2.0/1"), "{vrun}");
    // the service's command line and env carry over, the version's env over them
    assert!(
        vrun.contains(r"'\''serve'\'' '\''--port'\'' '\''80'\''"),
        "{vrun}"
    );
    assert!(vrun.contains("'A=1'") && vrun.contains("'B=3'"), "{vrun}");
    assert!(run.contains("'B=2'"), "{run}");
    // the marker is in the launcher only, not in hydo-oci's helper
    assert!(!read(rr.join("etc/hydo/oci/web@2.0/exec")).contains("oci-ver"));
    let ij: Value =
        serde_json::from_str(&read(rr.join("etc/hydo/oci/web@2.0/image.json"))).unwrap();
    assert_eq!(
        (ij["service"].clone(), ij["version"].clone()),
        (json!("web"), json!("2.0"))
    );
    // hydo-oci picks the tree that runs (and enters the service's network namespace)
    let hb = read(rr.join("bin/hydo-oci"));
    assert!(hb.contains("t=$s@$HYDO_VERSION"), "{hb}");
    assert!(hb.contains("/tmp/hydo/oci-ver.$s"), "{hb}");
    assert!(
        hb.contains("nsenter -n/tmp/hydo/ns/$s /etc/hydo/oci/$t/exec"),
        "{hb}"
    );
    // without versions: no marker, and the hydo-oci of before
    let (_, plain) = stage(&tmp, "plain", web(Value::Null));
    assert!(!read(plain.join("etc/hydo/oci/web/run")).contains("oci-ver"));
    let hb = read(plain.join("bin/hydo-oci"));
    assert!(!hb.contains("oci-ver") && !hb.contains("$t"), "{hb}");
    assert!(!plain.join("oci/web@2.0").exists());
    let _ = std::fs::remove_dir_all(&tmp);
}
