//! The kernel table is the releases' pins, and the recorded command lines are the engines'.

use crate::tmpdir::TmpDir;
use hydo_image::kernels::{self, Known, ARM64_CMDLINE, KNOWN, X86_CMDLINE};
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn pinned(file: &str, name: &str) -> String {
    let t = std::fs::read_to_string(repo().join(file)).unwrap();
    t.lines()
        .filter_map(|l| l.split_once("  "))
        .find(|(_, p)| p.trim_start_matches("./") == name)
        .map(|(s, _)| s.to_string())
        .unwrap_or_else(|| panic!("{name} not pinned in {file}"))
}

#[test]
fn the_table_is_the_release_pins() {
    for k in KNOWN {
        let Known {
            arch, file, sha256, ..
        } = k;
        let got = if arch == "x86_64" {
            pinned("hydo/dist/linux/assets.sha256", &format!("guest/{file}"))
        } else {
            pinned("hydo/dist/mac/assets.sha256", file)
        };
        assert_eq!(got, sha256, "{file}");
    }
}

/// The text of a `&str` constant in a source file (string continuation lines joined).
fn constant(src: &str, name: &str) -> String {
    let t = std::fs::read_to_string(repo().join(src)).unwrap();
    let at = t
        .find(&format!("pub const {name}: &str ="))
        .unwrap_or_else(|| panic!("{name} not in {src}"));
    let rest = &t[at..];
    let a = rest.find('"').unwrap() + 1;
    let b = a + rest[a..].find("\";").unwrap();
    let mut s = String::new();
    let mut lines = rest[a..b].split('\n');
    s.push_str(lines.next().unwrap());
    for l in lines {
        s = s.trim_end_matches('\\').to_string();
        s.push_str(l.trim_start());
    }
    s
}

#[test]
fn the_command_lines_are_the_engines() {
    assert_eq!(
        format!(
            "{} quiet loglevel=1",
            constant("hydo/crates/hydo-hvx/src/boot.rs", "ROOTFS_CMDLINE")
        ),
        X86_CMDLINE
    );
    assert_eq!(
        format!(
            "{} quiet loglevel=1",
            constant("hydo/crates/hydo-hvm/src/layout.rs", "BOOTARGS")
        ),
        ARM64_CMDLINE
    );
}

#[test]
fn records() {
    let x = kernels::record_for("x86_64", false);
    assert_eq!(x.kernel.file, "bzImage");
    assert_eq!(x.initrd.as_ref().unwrap().file, "initramfs-kafka.cpio.gz");
    // since the fingerprint epoch 3 (M234) the arm64 images get the kernels with cgroups, and
    // since epoch 7 (M376) with /proc/<pid>/io too; the x86_64 record still names hvx's kernel,
    // which has always had both
    let a = kernels::record_for("arm64", false);
    assert_eq!(a.kernel.file, "Image-explore-io");
    assert_eq!(a.kernel.sha256, kernels::ARM64_KERNEL_IO.sha256);
    let n = kernels::record_for("arm64", true);
    assert_eq!(n.kernel.file, "Image-explore-netns-io");
    assert_eq!(n.kernel.sha256, kernels::ARM64_NETNS_KERNEL_IO.sha256);
    assert!(n.initrd.is_none());
    assert_eq!(a.epoch, hydo_image::EPOCH);
    assert!(hydo_image::defaults(a.epoch, "arm64").cgroups);
    // without guest.kernel and guest.proc_io the record has no variant and no proc_io, so its
    // fields are those of before M256
    for r in [&x, &a, &n] {
        assert!(r.kernel_variant.is_none() && r.proc_io.is_none());
        let t = hydo_image::canonical(r);
        assert!(
            !t.contains("kernel_variant") && !t.contains("proc_io"),
            "{t}"
        );
        assert!(r.proc_io());
    }
    assert_eq!(kernels::record_with("arm64", true, None), n);
    assert_eq!(kernels::record_opts("arm64", true, None, true), n);
}

#[test]
fn proc_io_false_keeps_the_kernels_of_epoch_6() {
    // Milestone 376: guest.proc_io: false names the kernels with cgroups of epochs 3 to 6
    let a = kernels::record_opts("arm64", false, None, false);
    assert_eq!(a.kernel.sha256, kernels::ARM64_KERNEL_CG.sha256);
    let n = kernels::record_opts("arm64", true, None, false);
    assert_eq!(n.kernel.sha256, kernels::ARM64_NETNS_KERNEL_CG.sha256);
    let c = kernels::record_opts("arm64", true, Some(kernels::CONTAINERS), false);
    assert_eq!(c.kernel.sha256, kernels::ARM64_KERNEL_CONTAINERS.sha256);
    let x = kernels::record_opts("x86_64", false, None, false);
    assert_eq!(x.kernel, kernels::record_for("x86_64", false).kernel);
    for r in [&a, &n, &c, &x] {
        assert_eq!(r.epoch, hydo_image::EPOCH);
        assert_eq!(r.proc_io, Some(false));
        assert!(!r.proc_io());
        let t = hydo_image::canonical(r);
        assert!(t.contains("\"proc_io\":false"), "{t}");
        assert_eq!(hydo_image::parse_record(&t).unwrap(), *r);
    }
    // an image built before epoch 7 never boots the new kernels, with or without the field
    let mut old = kernels::record_for("x86_64", false);
    old.epoch = 6;
    assert!(!old.proc_io());
    old.proc_io = Some(true);
    assert!(!old.proc_io());
}

#[test]
fn the_containers_variant() {
    // arm64: the record names the variant's kernel, with or without service networks
    for netns in [false, true] {
        let c = kernels::record_with("arm64", netns, Some(kernels::CONTAINERS));
        assert_eq!(c.kernel.file, "Image-explore-containers-io");
        assert_eq!(c.kernel.sha256, kernels::ARM64_KERNEL_CONTAINERS_IO.sha256);
        assert_eq!(c.kernel_variant.as_deref(), Some("containers"));
        assert_eq!(c.epoch, hydo_image::EPOCH);
        let t = hydo_image::canonical(&c);
        assert!(t.contains("\"kernel_variant\":\"containers\""), "{t}");
        assert_eq!(hydo_image::parse_record(&t).unwrap(), c);
    }
    // x86_64: hvx's kernel, and the field the KVM engine picks its build by
    let x = kernels::record_with("x86_64", false, Some(kernels::CONTAINERS));
    assert_eq!(x.kernel, kernels::record_for("x86_64", false).kernel);
    assert_eq!(x.kernel_variant.as_deref(), Some("containers"));
    assert_eq!(kernels::VARIANTS, ["containers"]);
}

#[test]
fn lookup_by_sha256_and_the_refusal() {
    let tmp =
        TmpDir::fresh(std::env::temp_dir().join(format!("hydo-image-k-{}", std::process::id())));
    let d = tmp.to_path_buf();
    // a record that names a kernel by the sha256 of a file we write
    std::fs::write(d.join("Image-explore"), b"not the kernel").unwrap();
    std::fs::write(d.join("Image-explore-netns"), b"a kernel").unwrap();
    let mut rec = kernels::record_for("arm64", true);
    rec.kernel.sha256 = kernels::sha256_of(&d.join("Image-explore-netns")).unwrap();
    let img = d.join("explore-ob-x-arm64.cpio.gz");
    let dirs = std::slice::from_ref(&d);
    let f = kernels::find(&img, &rec, dirs, None).unwrap();
    assert_eq!(f.kernel, d.join("Image-explore-netns"));
    // found under another known name too
    std::fs::rename(d.join("Image-explore-netns"), d.join("Image-explore")).unwrap();
    std::fs::write(d.join("Image-explore-netns"), b"other").unwrap();
    let f = kernels::find(&img, &rec, dirs, None).unwrap();
    assert_eq!(f.kernel, d.join("Image-explore"));
    // a given kernel must be the recorded one
    let e = kernels::find(&img, &rec, &[], Some(&d.join("Image-explore-netns"))).unwrap_err();
    assert!(e.contains("but") && e.contains(&rec.kernel.sha256), "{e}");
    // missing: the message names the file, the version, the sha256 and who built the image
    rec.kernel.sha256 = "ab".repeat(32);
    let e = kernels::find(&img, &rec, dirs, None).unwrap_err();
    for w in [
        "Image-explore-netns",
        "Linux 6.6.35",
        &"ab".repeat(32),
        "is not there",
        "hydo build",
    ] {
        assert!(e.contains(w), "{w} not in {e}");
    }
    std::fs::write(
        hydo_image::sidecar_path(&img),
        hydo_image::document(&hydo_image::BuildRecord {
            hydo: hydo_image::Hydo {
                version: "0.0.9".into(),
                commit: Some("0123456789abcdef".into()),
            },
            record: rec.clone(),
            ..Default::default()
        }),
    )
    .unwrap();
    let e = kernels::find(&img, &rec, dirs, None).unwrap_err();
    assert!(
        e.contains("built by hydo 0.0.9, commit 0123456789ab")
            && e.contains("Install hydo 0.0.9's guest kernel"),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&d);
}
