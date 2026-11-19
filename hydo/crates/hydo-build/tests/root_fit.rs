//! Milestone 324: the image services' roots against where the engine keeps them. A user's four
//! services of one 1.2 GB image gave four roots in the initramfs, which did not fit in the Mac's
//! guest RAM, and hvx takes at most 4 GiB of root and data disk together. The estimate reads the
//! size of each layer without unpacking it (a gzip layer records its unpacked size in its last 4
//! bytes), so `--check` gives the same warning as a build, before anything is unpacked.

use crate::tmpdir::TmpDir;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

/// A `docker save` archive of one image whose one gzip layer holds a file of `mib` MiB of
/// zeros, for both architectures (`big-amd64.docker.tar`, `big-arm64.docker.tar`).
fn big_image(dir: &Path, mib: u64) {
    let tree = dir.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    // written, not sparse: a tar of a sparse file holds only its data
    std::fs::write(tree.join("blob"), vec![0u8; (mib << 20) as usize]).unwrap();
    let st = Command::new("tar")
        .arg("-C")
        .arg(&tree)
        .arg("-czf")
        .arg(dir.join("layer.tar.gz"))
        .arg("blob")
        .status()
        .unwrap();
    assert!(st.success());
    for (arch, file) in [("amd64", "big-amd64"), ("arm64", "big-arm64")] {
        let a = dir.join(format!("a-{arch}"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::copy(dir.join("layer.tar.gz"), a.join("layer.tar.gz")).unwrap();
        let config = json!({
            "architecture": arch, "os": "linux",
            "config": {"Cmd": ["/bin/sh"]},
            "rootfs": {"type": "layers", "diff_ids": ["sha256:0000"]}
        });
        std::fs::write(a.join("config.json"), config.to_string()).unwrap();
        let man =
            json!([{"Config": "config.json", "RepoTags": ["big:1"], "Layers": ["layer.tar.gz"]}]);
        std::fs::write(a.join("manifest.json"), man.to_string()).unwrap();
        let st = Command::new("tar")
            .arg("-C")
            .arg(&a)
            .arg("-cf")
            .arg(dir.join(format!("{file}.docker.tar")))
            .args(["manifest.json", "config.json", "layer.tar.gz"])
            .status()
            .unwrap();
        assert!(st.success());
    }
}

fn service(name: &str) -> Value {
    json!({
        "name": name,
        "image": {"x86_64": "big-amd64.docker.tar", "arm64": "big-arm64.docker.tar"},
        "image_ref": "big:1",
        "ready": "true"
    })
}

/// The warnings of a manifest of three services of the image, with `extra` merged in.
fn warnings(dir: &Path, arch: &str, extra: Value) -> Vec<String> {
    let mut m = json!({
        "name": "fit",
        "services": [service("scheduler"), service("executor_1"), service("executor_2")]
    });
    for (k, v) in extra.as_object().unwrap() {
        m[k] = v.clone();
    }
    let mpath = dir.join("fit.json");
    std::fs::write(&mpath, m.to_string()).unwrap();
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let mut io = hydo_build::Io {
        out: &mut o,
        err: &mut e,
    };
    let svcs = hydo_build::oci::prepare(&mut m, &mpath.to_string_lossy(), arch, &mut io).unwrap();
    hydo_build::oci::root_fit_warnings(&svcs, &m, arch)
}

#[test]
fn roots_that_do_not_fit_are_named_before_the_build_unpacks_them() {
    let tmp = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-build-root-fit-{}", std::process::id())),
    );
    // 48 MiB of content in a tar: a few KiB more than 48 MiB, so 49 MiB rounded up
    big_image(&tmp, 48);
    let got = hydo_build::oci::Image::new(
        &tmp.join("big-arm64.docker.tar").to_string_lossy(),
        Some("big:1"),
        "arm64",
    )
    .unwrap()
    .unpacked_bytes()
    .unwrap();
    assert!((48 << 20..(48 << 20) + 65536).contains(&got), "{got}");

    // the Mac: three roots of 49 MiB in a guest of 256 MiB, whose tmpfs root holds 128 MiB
    let ram = json!({"guest": {"ram_mib": 256}});
    let w = warnings(&tmp, "arm64", ram.clone());
    assert_eq!(w.len(), 1, "{w:?}");
    let w = &w[0];
    for part in [
        "the image services' roots need about 147 MiB unpacked (scheduler, executor_1 and \
         executor_2: 49 MiB each)",
        "half of guest.ram_mib (128 of 256 MiB)",
        "scheduler, executor_1 and executor_2 run one image: root.share_images: true unpacks it \
         once, 49 MiB in all",
    ] {
        assert!(w.contains(part), "{part}\n{w}");
    }
    assert!(!w.contains("guest.ram_mib: "), "{w}");
    // shared, the one tree fits; in a smaller guest only more RAM helps
    let shared = json!({"guest": {"ram_mib": 256}, "root": {"share_images": true}});
    assert_eq!(warnings(&tmp, "arm64", shared), Vec::<String>::new());
    // the root on the disk is the disk's matter, and 147 MiB fit there
    let disk = json!({"guest": {"ram_mib": 256, "root": "disk"}});
    assert_eq!(warnings(&tmp, "arm64", disk), Vec::<String>::new());

    // hvx: root and data disk together at most 4096 MiB
    let data = json!({"data": {"size_mib": 4000}});
    let w = warnings(&tmp, "x86_64", data);
    assert_eq!(w.len(), 1, "{w:?}");
    let w = &w[0];
    for part in [
        "so the disk would be about 4192 MiB (the root partition at 1.3 times its content, and \
         the data disk of 4000 MiB)",
        "more than the 4096 MiB of root and data disk together that the hvx engine takes",
        "root.share_images: true unpacks it once, a disk of about 4064 MiB",
        "guest.ram_mib does not help",
    ] {
        assert!(w.contains(part), "{part}\n{w}");
    }
    assert_eq!(
        warnings(&tmp, "x86_64", json!({"data": {"size_mib": 3800}})),
        Vec::<String>::new()
    );
}
