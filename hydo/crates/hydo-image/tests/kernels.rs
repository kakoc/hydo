//! The kernel table is the releases' pins, and the recorded command lines are the engines'.

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
    assert_eq!(
        kernels::record_for("arm64", false).kernel.file,
        "Image-explore"
    );
    let n = kernels::record_for("arm64", true);
    assert_eq!(n.kernel.file, "Image-explore-netns");
    assert!(n.initrd.is_none());
}

#[test]
fn lookup_by_sha256_and_the_refusal() {
    let d = std::env::temp_dir().join(format!("hydo-image-k-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
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
