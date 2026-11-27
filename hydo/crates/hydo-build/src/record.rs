//! The image records (Milestone 155, crate `hydo-image`): `/etc/hydo/image.json` inside every
//! image (the guest kernel by sha256 and the fingerprint epoch; the same bytes from every hydo of
//! one epoch) and the build record `<image file>.json` next to it (the hydo version and commit,
//! the manifest's sha256, the service images' digests).

use crate::{fsx, oci::OciService, outln, Fail, Io};
use std::path::Path;

/// The kernel variant the manifest `m` asks for (`guest.kernel`, Milestone 256), if any.
pub fn kernel_variant(m: &serde_json::Value) -> Option<String> {
    hydo_manifest::Guest::from_value(m).kernel
}

/// Whether the manifest `m` keeps the Mac and KVM guest kernels with `/proc/<pid>/io`
/// (`guest.proc_io`, Milestone 376; true unless it says false).
pub fn proc_io(m: &serde_json::Value) -> bool {
    hydo_manifest::Guest::from_value(m).proc_io
}

/// The image record of the manifest `m` for `arch` (`netns_kernel`: an arm64 image that needs the
/// second kernel): its kernel variant and `guest.proc_io` (`hydo_image::kernels::record_opts`).
pub fn record_of(m: &serde_json::Value, arch: &str, netns_kernel: bool) -> hydo_image::ImageRecord {
    hydo_image::kernels::record_opts(arch, netns_kernel, kernel_variant(m).as_deref(), proc_io(m))
}

/// Write `/etc/hydo/image.json` into the staged root `r` (the record [`record_of`] gives).
pub fn write_inside(r: &Path, rec: &hydo_image::ImageRecord) -> Result<(), Fail> {
    fsx::makedirs(&r.join("etc/hydo"))?;
    fsx::write_text(
        &r.join(hydo_image::RECORD_PATH),
        &hydo_image::canonical(rec),
    )
}

fn file_ref(p: &Path) -> Result<hydo_image::FileRef, Fail> {
    Ok(hydo_image::FileRef {
        file: fsx::basename_str(p).to_string(),
        sha256: fsx::sha(p)?,
    })
}

/// Write `<image>.json` next to the finished image (`disk`: arm64's data disk; `record`: the image
/// record [`write_inside`] wrote, [`record_of`]).
pub fn write_sidecar(
    image: &Path,
    disk: Option<&Path>,
    record: hydo_image::ImageRecord,
    manifest_path: &str,
    oci: &[OciService],
    io: &mut Io,
) -> Result<(), Fail> {
    // an image file next to the manifest is named relative to it (no build machine's paths)
    let mdir = crate::abspath(Path::new(manifest_path));
    let mdir = if mdir.is_dir() {
        mdir
    } else {
        mdir.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    let services = oci
        .iter()
        .map(|s| hydo_image::ServiceImage {
            service: s.svc.clone(),
            image: crate::abspath(Path::new(&s.path))
                .strip_prefix(&mdir)
                .map(|r| r.display().to_string())
                .unwrap_or_else(|_| s.path.clone()),
            digest: s.img.manifest_digest.clone(),
            config_digest: Some(s.img.config_digest.clone()).filter(|d| !d.is_empty()),
        })
        .collect();
    let rec = hydo_image::BuildRecord {
        image: file_ref(image)?,
        disk: disk.map(file_ref).transpose()?,
        record,
        hydo: hydo_image::Hydo {
            version: hydo_image::hydo_version().into(),
            commit: hydo_image::hydo_commit().map(str::to_string),
        },
        manifest: hydo_image::Manifest {
            path: manifest_path.to_string(),
            sha256: {
                let m = Path::new(manifest_path);
                fsx::sha(&if m.is_dir() {
                    m.join("hydo.yaml")
                } else {
                    m.to_path_buf()
                })?
            },
        },
        services,
        ..hydo_image::BuildRecord::default()
    };
    let p = hydo_image::sidecar_path(image);
    fsx::write_text(&p, &hydo_image::document(&rec))?;
    outln(
        io,
        &format!(
            "hydo-build: {} (the build record; the image records guest kernel {} sha256 {}{}, fingerprint epoch {})",
            p.display(),
            rec.record.kernel.file,
            &rec.record.kernel.sha256[..16],
            rec.record
                .kernel_variant
                .as_deref()
                .map(|v| format!(", variant {v}"))
                .unwrap_or_default(),
            rec.record.epoch
        ),
    );
    Ok(())
}
