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

    // the Mac: three roots of 49 MiB in a guest of 256 MiB. Its tmpfs root holds about 82 MiB:
    // half of what the kernel leaves while it holds the compressed root (Milestone 338)
    let ram = json!({"guest": {"ram_mib": 256}});
    let w = warnings(&tmp, "arm64", ram.clone());
    assert_eq!(w.len(), 1, "{w:?}");
    let w = &w[0];
    for part in [
        "the image services' roots need about 147 MiB unpacked (scheduler, executor_1 and \
         executor_2: 49 MiB each)",
        "the whole root about 171 MiB of the guest's RAM with hydo's own files",
        "about 82 MiB of guest.ram_mib: 256",
        "scheduler, executor_1 and executor_2 run one image: root.share_images: true unpacks it \
         once, 49 MiB in all",
    ] {
        assert!(w.contains(part), "{part}\n{w}");
    }
    assert!(!w.contains("or more gives the roots the room"), "{w}");
    // shared, the one tree fits; in a smaller guest only more RAM helps
    let shared = json!({"guest": {"ram_mib": 256}, "root": {"share_images": true}});
    assert_eq!(warnings(&tmp, "arm64", shared), Vec::<String>::new());
    // the root on the disk is the disk's matter, and 147 MiB fit there
    let disk = json!({"guest": {"ram_mib": 256, "root": "disk"}});
    assert_eq!(warnings(&tmp, "arm64", disk), Vec::<String>::new());
    // Milestone 338: three roots of 49 MiB fit in half of 1024 MiB, and also with the archive
    assert_eq!(
        warnings(&tmp, "arm64", json!({"guest": {"ram_mib": 1024}})),
        Vec::<String>::new()
    );

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

/// Milestone 338: the room of an initramfs root against what the Mac engine showed. PostgreSQL
/// 16.4's root took 467 MiB of `Shmem` beside a 142 MiB archive: it did not fit in 1,024 MiB of
/// guest RAM (`Initramfs unpacking failed: write error`) and fitted in 2,048. The busybox image's
/// root as epoch 4 wrote it, 446 MiB beside 247 MiB, did not fit in 1,024 either and fitted in
/// 2,048.
#[test]
fn the_room_of_an_initramfs_root() {
    use hydo_build::oci::{initramfs_room, initramfs_warning, ram_for_initramfs};
    assert!(initramfs_room(1024, 142) < 467);
    assert!(initramfs_room(2048, 142) >= 467);
    assert!(initramfs_room(1024, 247) < 446);
    assert!(initramfs_room(2048, 247) >= 446);
    assert_eq!(ram_for_initramfs(467, 142), 1280);
    // the busybox image's root as epoch 5 writes it: 14 MiB beside 7 MiB
    assert_eq!(initramfs_warning(14, 7, 1024, "x"), None);
    let w = initramfs_warning(467, 142, 1024, "measured on the staged root").unwrap();
    for part in [
        "about 467 MiB of the guest's RAM unpacked (measured on the staged root)",
        "about 142 MiB",
        "guest.ram_mib: 1280 or more gives the root the room (now 1024)",
    ] {
        assert!(w.contains(part), "{part}\n{w}");
    }
    let w = initramfs_warning(20000, 9000, 1024, "x").unwrap();
    assert!(w.contains("guest.root: disk"), "{w}");
}

/// Milestone 338: a staged root's tmpfs size counts each file once, in whole pages.
#[test]
fn a_staged_root_counts_each_file_once_in_pages() {
    let tmp = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-build-tmpfs-mib-{}", std::process::id())),
    );
    let r = tmp.join("root");
    std::fs::create_dir_all(r.join("bin")).unwrap();
    std::fs::write(r.join("bin/busybox"), vec![1u8; 3 << 20]).unwrap();
    for a in 0..100 {
        std::fs::hard_link(r.join("bin/busybox"), r.join(format!("bin/a{a}"))).unwrap();
    }
    for f in 0..512 {
        std::fs::write(r.join(format!("f{f}")), "x").unwrap();
    }
    std::os::unix::fs::symlink("busybox", r.join("bin/sh")).unwrap();
    // 3 MiB once, and 512 files of one byte in a page each: 2 MiB
    assert_eq!(hydo_build::oci::tmpfs_mib(&r).unwrap(), 5);
}

/// Milestone 367: the services' own file systems come on top of the data disk. The SeaweedFS
/// onboarding gave the data disk 1,792 MiB beside eight file systems; `--check` printed `OK`, and
/// the build stopped after it had unpacked the images.
#[test]
fn the_services_own_file_systems_count_against_the_disk() {
    let tmp = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-build-root-fit-vols-{}", std::process::id())),
    );
    big_image(&tmp, 48);
    let with_vols = |data: u64, mib: u64| -> Value {
        let mut m = json!({
            "name": "fit",
            "data": {"size_mib": data},
            "services": [service("scheduler"), service("executor_1"), service("executor_2")]
        });
        for (k, s) in m["services"].as_array_mut().unwrap().iter_mut().enumerate() {
            s["storage"] = json!({"dir": format!("v{k}"), "size_mib": mib});
        }
        m
    };
    let fit = |mut m: Value| -> Vec<String> {
        let mpath = tmp.join("vols.json");
        std::fs::write(&mpath, m.to_string()).unwrap();
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let mut io = hydo_build::Io {
            out: &mut o,
            err: &mut e,
        };
        let svcs =
            hydo_build::oci::prepare(&mut m, &mpath.to_string_lossy(), "x86_64", &mut io).unwrap();
        hydo_build::oci::root_fit_warnings(&svcs, &m, "x86_64")
    };
    // three file systems of 512 MiB, each with 1 MiB before it
    assert_eq!(
        hydo_build::image::volumes_mib(&with_vols(2048, 512)),
        3 * 513
    );
    // the roots (192 MiB at 1.3 times), data 2048 and the file systems fit: 3779 MiB
    assert_eq!(fit(with_vols(2048, 512)), Vec::<String>::new());
    assert_eq!(hydo_build::image::disk_refusal(&with_vols(2048, 512)), None);
    // data 2400: the known parts fit (3941 MiB), the estimate with the roots does not (4131)
    let m = with_vols(2400, 512);
    assert_eq!(hydo_build::image::disk_refusal(&m), None);
    let w = fit(m);
    assert_eq!(w.len(), 1, "{w:?}");
    for part in [
        "so the disk would be about 4131 MiB",
        "and the services' own file systems of 1539 MiB on top of it",
        "smaller storage size_mib values",
    ] {
        assert!(w[0].contains(part), "{part}\n{}", w[0]);
    }
    // without the file systems the same data disk gives no warning
    assert_eq!(
        warnings(&tmp, "x86_64", json!({"data": {"size_mib": 2400}})),
        Vec::<String>::new()
    );
    // the known parts alone pass the disk: the build refuses before it unpacks anything
    let r = hydo_build::image::disk_refusal(&with_vols(1792, 768)).unwrap();
    for part in [
        "is at least 4101 MiB (data.size_mib 1792 and the storage file systems 2307 MiB",
        "more than the 4096 MiB an engine maps",
        "come on top of data.size_mib",
    ] {
        assert!(r.contains(part), "{part}\n{r}");
    }
}
