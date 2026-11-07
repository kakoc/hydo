//! `root.share_images: true`: two image services with the same image share one unpacked tree
//! (the libSQL onboarding: a primary and a replica of one 63 MB image made a 1.1 GB disk image,
//! the image unpacked twice). The first service's tree is staged as without the option; the
//! second gets an empty /oci/<svc> that its launcher bind-mounts the first one's tree onto before
//! its own binds and mounts, and its mount points are made in the one tree. Without the option
//! nothing changes (tests/oci.rs keeps the recorded answers).

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

fn service(name: &str, dst: &str) -> Value {
    json!({
        "name": name,
        "image": {"x86_64": "demo-amd64.docker.tar", "arm64": "demo-arm64.docker.tar"},
        "ready": "true",
        "image_ref": "demo:1.0",
        "mounts": [{"src": format!("/data/vol/{name}"), "dst": dst}, {"tmpfs": true, "dst": "/cache"}]
    })
}

/// prepare + stage of a primary and a replica of one image; the meta keys and the staged root.
fn stage(tmp: &Path, share: bool, arch: &str) -> (Vec<String>, PathBuf) {
    let mpath = tmp.join(format!("share-{share}-{arch}.json"));
    let mut m = json!({
        "name": "share",
        "services": [service("primary", "/var/lib/sqld"), service("replica", "/var/lib/replica")]
    });
    std::fs::write(&mpath, m.to_string()).unwrap();
    let rr = tmp.join(format!("root-{share}-{arch}"));
    std::fs::create_dir_all(rr.join("bin")).unwrap();
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let mut io = hydo_build::Io {
        out: &mut o,
        err: &mut e,
    };
    let mut svcs =
        hydo_build::oci::prepare(&mut m, &mpath.to_string_lossy(), arch, &mut io).unwrap();
    let meta = hydo_build::oci::stage(
        &mut svcs,
        &rr,
        &tmp.join("busybox"),
        &tmp.join(format!("tmp-{share}-{arch}")),
        false,
        share,
        &mut io,
    )
    .unwrap_or_else(|f| panic!("{}", f.message()));
    let mut keys: Vec<String> = meta.keys().cloned().collect();
    keys.sort();
    (keys, rr)
}

#[test]
fn one_tree_for_two_services_of_one_image() {
    // SAFETY: umask only sets the process's file creation mask.
    unsafe {
        libc::umask(0o022);
    }
    let tmp = std::env::temp_dir().join(format!("hydo-build-oci-share-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    images(&tmp);
    std::fs::write(tmp.join("busybox"), b"#!/bin/sh\n").unwrap();
    for arch in ["x86_64", "arm64"] {
        let (two, _) = stage(&tmp, false, arch);
        let (one, rr) = stage(&tmp, true, arch);
        // without the option: two trees
        assert!(two.iter().any(|k| k.starts_with("oci/replica/")), "{arch}");
        // with it: the replica's root is one empty directory; the tree holds both mount points
        assert!(one.contains(&"oci/replica".to_string()), "{arch}");
        assert!(!one.iter().any(|k| k.starts_with("oci/replica/")), "{arch}");
        assert!(
            one.contains(&"oci/primary/var/lib/sqld".to_string()),
            "{arch}"
        );
        assert!(
            one.contains(&"oci/primary/var/lib/replica".to_string()),
            "{arch}"
        );
        assert_eq!(
            std::fs::read_dir(rr.join("oci/replica")).unwrap().count(),
            0,
            "{arch}"
        );
        // the primary's tree is the tree it had alone, plus the replica's mount point
        let only_primary: Vec<&String> = two
            .iter()
            .filter(|k| k.starts_with("oci/primary"))
            .collect();
        let shared_primary: Vec<&String> = one
            .iter()
            .filter(|k| k.starts_with("oci/primary") && *k != "oci/primary/var/lib/replica")
            .collect();
        assert_eq!(only_primary, shared_primary, "{arch}");
        // the replica's launcher mounts the shared tree first; the primary's is unchanged
        let run = std::fs::read_to_string(rr.join("etc/hydo/oci/replica/run")).unwrap();
        let bind = "$BB grep -q \" $R \" /proc/mounts || $BB mount --bind /oci/primary $R || exit 125\nfor d in";
        assert!(run.contains(bind), "{run}");
        assert!(run.contains("R=/oci/replica"), "{run}");
        let ex = std::fs::read_to_string(rr.join("etc/hydo/oci/replica/exec")).unwrap();
        assert!(ex.contains("mount --bind /oci/primary $R"), "{ex}");
        let prun = std::fs::read_to_string(rr.join("etc/hydo/oci/primary/run")).unwrap();
        assert!(!prun.contains("mount --bind /oci/"), "{prun}");
        let ij: Value = serde_json::from_str(
            &std::fs::read_to_string(rr.join("etc/hydo/oci/replica/image.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(ij["shares_tree_of"], "primary");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
