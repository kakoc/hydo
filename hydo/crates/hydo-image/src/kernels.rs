//! The guest kernels this hydo knows, the record an image gets for them, and how a run finds the
//! kernel an image names.
//!
//! The kernels are the releases' pinned files (`hydo/dist/linux/assets.sha256`: `guest/bzImage`
//! and `guest/initramfs-kafka.cpio.gz`; `hydo/dist/mac/assets.sha256`: `Image-explore` and
//! `Image-explore-netns`; docs/BUILD-PINS.md). `tests/kernels.rs` checks this table against those
//! pin files and the command lines against the engines'.
//!
//! A run looks the image's kernel up by sha256 ([`find`]): among the files with a known kernel
//! name (and the name the image records) in the directories the caller gives (a release's
//! `guest/`, a tree's `build/` or `hv/hvm/explore/out/`); then [`obtain`], which asks the
//! [`Source`] the program registered with [`set_source`] (the `hydo` command registers its
//! install cache, Milestone 162: the content store, then the release that shipped a file with
//! that sha256, fetched by its pinned archive; docs/design/install-cache.md); else the run is
//! refused with a message that names the kernel, the hydo that built the image and why the
//! source could not provide it.

use crate::{read_sidecar, FileRef, ImageRecord, Kernel, EPOCH};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What a known file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Kernel,
    Initrd,
}

/// One guest kernel (or initramfs) this hydo knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Known {
    pub arch: &'static str,
    pub role: Role,
    pub file: &'static str,
    pub sha256: &'static str,
    pub version: &'static str,
}

/// The x86_64 guest kernel (`hydo/dist/linux/assets.sha256` `guest/bzImage`).
pub const X86_KERNEL: Known = Known {
    arch: "x86_64",
    role: Role::Kernel,
    file: "bzImage",
    sha256: "0a9ede352483e1718a8583e35afa3e82791c8223e40c0a888d39b0530158e9d3",
    version: "6.6.35",
};

/// The x86_64 initramfs that mounts an image's root (`guest/initramfs-kafka.cpio.gz`).
pub const X86_INITRD: Known = Known {
    arch: "x86_64",
    role: Role::Initrd,
    file: "initramfs-kafka.cpio.gz",
    sha256: "d074d764c76ce11932d5ff445cd468de38fdca8dd0db02f85fde3817ff997230",
    version: "6.6.35",
};

/// The default arm64 guest kernel (`hydo/dist/mac/assets.sha256` `Image-explore`).
pub const ARM64_KERNEL: Known = Known {
    arch: "arm64",
    role: Role::Kernel,
    file: "Image-explore",
    sha256: "f80c565589f1fecf9ccd49d33cb0717c7a15d5cddf3bb3bec7f2dd2b335b51ac",
    version: "6.6.35",
};

/// The second arm64 guest kernel, for `network: bridge`, `addr:` and the freeze (M99).
pub const ARM64_NETNS_KERNEL: Known = Known {
    arch: "arm64",
    role: Role::Kernel,
    file: "Image-explore-netns",
    sha256: "806807e7be5f55dae342f796ccc6e20c35514b3684a8c0d4633901a8ac0aa609",
    version: "6.6.35",
};

/// Every file of the table.
pub const KNOWN: [Known; 4] = [X86_KERNEL, X86_INITRD, ARM64_KERNEL, ARM64_NETNS_KERNEL];

/// The x86_64 kernel command line of an agent image's boot (hydo-hvx's `ROOTFS_CMDLINE` and
/// ` quiet loglevel=1`).
pub const X86_CMDLINE: &str = "root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 \
loglevel=8 nokaslr acpi=off x2apic_phys quiet loglevel=1";

/// The arm64 kernel command line of an agent image's boot (hydo-hvm's `BOOTARGS` and
/// ` quiet loglevel=1`).
pub const ARM64_CMDLINE: &str = "console=ttyS0 earlycon=uart8250,mmio,0x09000000 \
clocksource.arm_arch_timer.evtstrm=0 nokaslr rdinit=/init loglevel=7 random.trust_bootloader=on \
quiet loglevel=1";

fn kernel_of(k: &Known, cmdline: &str) -> Kernel {
    Kernel {
        file: k.file.into(),
        sha256: k.sha256.into(),
        version: k.version.into(),
        cmdline: cmdline.into(),
    }
}

/// The record `hydo build` writes into an image of `arch` (`netns`: an arm64 image that needs the
/// second kernel).
pub fn record_for(arch: &str, netns: bool) -> ImageRecord {
    if arch == "arm64" {
        ImageRecord {
            arch: "arm64".into(),
            epoch: EPOCH,
            kernel: kernel_of(
                if netns {
                    &ARM64_NETNS_KERNEL
                } else {
                    &ARM64_KERNEL
                },
                ARM64_CMDLINE,
            ),
            initrd: None,
            ..ImageRecord::default()
        }
    } else {
        ImageRecord {
            arch: "x86_64".into(),
            epoch: EPOCH,
            kernel: kernel_of(&X86_KERNEL, X86_CMDLINE),
            initrd: Some(FileRef {
                file: X86_INITRD.file.into(),
                sha256: X86_INITRD.sha256.into(),
            }),
            ..ImageRecord::default()
        }
    }
}

/// (path, size, mtime) of a hashed file.
type FileKey = (PathBuf, u64, std::time::SystemTime);

static SHA_CACHE: Mutex<Option<HashMap<FileKey, String>>> = Mutex::new(None);

/// Lower-case hex SHA-256 of a file, remembered per (path, size, mtime) for this process.
pub fn sha256_of(p: &Path) -> std::io::Result<String> {
    let md = std::fs::metadata(p)?;
    let key = (p.to_path_buf(), md.len(), md.modified()?);
    if let Some(s) = SHA_CACHE
        .lock()
        .expect("lock")
        .get_or_insert_with(HashMap::new)
        .get(&key)
    {
        return Ok(s.clone());
    }
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    let s: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    SHA_CACHE
        .lock()
        .expect("lock")
        .get_or_insert_with(HashMap::new)
        .insert(key, s.clone());
    Ok(s)
}

/// Where a guest file that is not in the searched directories comes from (Milestone 162): a
/// program registers one with [`set_source`]. The `hydo` command's is its install cache
/// (hydo-cli `src/cache.rs`).
pub trait Source: Send + Sync {
    /// A path to a file with `want`'s sha256 (its name may differ), or why there is none (one
    /// or two sentences, for the refusal's message).
    fn obtain(&self, want: &FileRef) -> Result<PathBuf, String>;
}

static SOURCE: std::sync::OnceLock<Box<dyn Source>> = std::sync::OnceLock::new();

/// Register this process's [`Source`] (once, at a program's start; a second call is ignored and
/// returns false).
pub fn set_source(s: Box<dyn Source>) -> bool {
    SOURCE.set(s).is_ok()
}

/// A file with `want`'s sha256 from the registered [`Source`]. Err holds why there is none
/// (empty when no source is registered: programs other than `hydo`, and tests).
pub fn obtain(want: &FileRef) -> Result<PathBuf, String> {
    match SOURCE.get() {
        Some(s) => s.obtain(want),
        None => Err(String::new()),
    }
}

/// The first file in `dirs` with one of the known names for `role` (or `want.file`) whose sha256
/// is `want.sha256`, else [`obtain`]'s, else why there is none ([`obtain`]'s reason).
pub fn locate_or_why(want: &FileRef, role: Role, dirs: &[PathBuf]) -> Result<PathBuf, String> {
    match search(want, role, dirs) {
        Some(p) => Ok(p),
        None => obtain(want),
    }
}

/// [`locate_or_why`] without the reason.
pub fn locate(want: &FileRef, role: Role, dirs: &[PathBuf]) -> Option<PathBuf> {
    locate_or_why(want, role, dirs).ok()
}

fn search(want: &FileRef, role: Role, dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut names: Vec<&str> = vec![want.file.as_str()];
    for k in KNOWN.iter().filter(|k| k.role == role) {
        if !names.contains(&k.file) {
            names.push(k.file);
        }
    }
    for d in dirs {
        for n in &names {
            let p = d.join(n);
            if p.is_file() && sha256_of(&p).ok().as_deref() == Some(want.sha256.as_str()) {
                return Some(p);
            }
        }
    }
    None
}

/// The kernel (and the x86_64 initramfs) a run boots an image with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
}

/// Who built an image, in words, from its build record (`<image>.json`).
fn built_by(image: &Path) -> (String, String) {
    match read_sidecar(image) {
        Some(b) => {
            let c = b
                .hydo
                .commit
                .as_deref()
                .map(|c| format!(", commit {}", &c[..c.len().min(12)]))
                .unwrap_or_default();
            (
                format!("hydo {}{c}", b.hydo.version),
                format!("hydo {}'s", b.hydo.version),
            )
        }
        None => (
            format!(
                "a hydo whose version is unknown (its build record {} is not there)",
                crate::sidecar_path(image).display()
            ),
            "that hydo's".into(),
        ),
    }
}

fn dirs_text(dirs: &[PathBuf]) -> String {
    let v: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    if v.is_empty() {
        "nowhere".into()
    } else {
        v.join(", ")
    }
}

/// The refusal for a file the image names that this hydo does not have.
pub fn missing_message(
    image: &Path,
    what: &str,
    want: &FileRef,
    version: &str,
    dirs: &[PathBuf],
) -> String {
    let (by, whose) = built_by(image);
    let ver = if version.is_empty() {
        String::new()
    } else {
        format!("Linux {version}, ")
    };
    format!(
        "{} was built for the guest {what} {} ({ver}sha256 {}), which this hydo ({}) does not \
         have (looked in {}). The image was built by {by}. Install {whose} guest {what} (the file {} \
         with that sha256) into one of those directories, or rebuild the image with this hydo \
         (hydo build).",
        image.display(),
        want.file,
        want.sha256,
        crate::hydo_version(),
        dirs_text(dirs),
        want.file,
    )
}

/// [`missing_message`] with why the registered [`Source`] could not provide the file (nothing
/// is added when there is no reason).
pub fn missing_message_why(
    image: &Path,
    what: &str,
    want: &FileRef,
    version: &str,
    dirs: &[PathBuf],
    why: &str,
) -> String {
    let m = missing_message(image, what, want, version, dirs);
    if why.is_empty() {
        m
    } else {
        format!("{m} The install cache could not provide it: {why}")
    }
}

/// The kernel (and initramfs) of `image` from its record `rec`, looked up in `dirs`; `given` is a
/// kernel the user named (it must be the recorded one).
pub fn find(
    image: &Path,
    rec: &ImageRecord,
    dirs: &[PathBuf],
    given: Option<&Path>,
) -> Result<Found, String> {
    let want = FileRef {
        file: rec.kernel.file.clone(),
        sha256: rec.kernel.sha256.clone(),
    };
    let kernel = match given {
        Some(k) => {
            let got = sha256_of(k).map_err(|e| format!("{}: {e}", k.display()))?;
            if got != want.sha256 {
                return Err(format!(
                    "{} was built for the guest kernel {} (Linux {}, sha256 {}), but {} has \
                     sha256 {}",
                    image.display(),
                    want.file,
                    rec.kernel.version,
                    want.sha256,
                    k.display(),
                    got
                ));
            }
            k.to_path_buf()
        }
        None => locate_or_why(&want, Role::Kernel, dirs).map_err(|why| {
            missing_message_why(image, "kernel", &want, &rec.kernel.version, dirs, &why)
        })?,
    };
    let initrd = match &rec.initrd {
        None => None,
        Some(i) => Some(
            locate_or_why(i, Role::Initrd, dirs)
                .map_err(|why| missing_message_why(image, "initramfs", i, "", dirs, &why))?,
        ),
    };
    Ok(Found { kernel, initrd })
}

/// The warning for an image without the record.
pub fn no_record_warning(image: &Path) -> String {
    format!(
        "warning: {} has no /{} (built before Milestone 155): it boots this hydo's default guest \
         kernel; rebuild it with hydo build to record the kernel it needs",
        image.display(),
        crate::RECORD_PATH
    )
}
