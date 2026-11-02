//! Reading files out of a built image without booting it.
//!
//! - an x86_64 image (`*-disk.img`): the root filesystem is ext4 at LBA 2048; it is read with
//!   `debugfs -R "cat <path>" <image>?offset=1048576` (the release bundles debugfs in
//!   `libexec/bin`, first on PATH), as manifest_space.py does;
//! - an arm64 image (`*.cpio.gz`): a gzip'd newc cpio, read directly (hvm_backend.embedded_file).

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

/// hydo-build puts the root filesystem at LBA 2048.
pub const ROOT_OFFSET: u64 = 2048 * 512;

#[derive(Debug)]
pub enum ImageError {
    /// The file is not in the image (or the image could not be read).
    Missing(String),
    /// The image (its path) has no `/etc/hydo/control`: a shell-init image ([`control`]).
    ShellImage(String),
    Io(std::io::Error),
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageError::Missing(m) => f.write_str(m),
            ImageError::ShellImage(p) => write!(
                f,
                "{p}: a shell-init image (no /etc/hydo/control), built before Milestone 136; this \
                 hydo runs agent images only: rebuild it with hydo build"
            ),
            ImageError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ImageError {}

impl From<std::io::Error> for ImageError {
    fn from(e: std::io::Error) -> Self {
        ImageError::Io(e)
    }
}

/// One file of an image's root, by its path without the leading slash (`etc/hydo/hydo.yaml`).
pub fn embedded_file(image: &Path, path: &str) -> Result<String, ImageError> {
    if image.to_string_lossy().ends_with(".cpio.gz") {
        cpio_file(image, path)
    } else {
        debugfs_cat(image, path)
    }
}

/// `debugfs -R "cat /<path>" <image>?offset=...`; an empty answer counts as missing.
pub fn debugfs_cat(image: &Path, path: &str) -> Result<String, ImageError> {
    let out = Command::new("debugfs")
        .arg("-R")
        .arg(format!("cat /{path}"))
        .arg(format!("{}?offset={}", image.display(), ROOT_OFFSET))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() || text.trim().is_empty() {
        return Err(ImageError::Missing(format!(
            "no /{path} in {}",
            image.display()
        )));
    }
    Ok(text)
}

/// One file from a gzip'd newc cpio (hvm_backend.embedded_file).
pub fn cpio_file(image: &Path, path: &str) -> Result<String, ImageError> {
    let mut data = Vec::new();
    GzDecoder::new(std::fs::File::open(image)?).read_to_end(&mut data)?;
    let hex = |b: &[u8]| -> Option<usize> {
        usize::from_str_radix(std::str::from_utf8(b).ok()?, 16).ok()
    };
    let mut i = 0usize;
    while data.len() >= i + 110 && &data[i..i + 6] == b"070701" {
        let field = |k: usize| hex(&data[i + 6 + 8 * k..i + 14 + 8 * k]);
        let (Some(fsz), Some(nsz)) = (field(6), field(11)) else {
            break;
        };
        if nsz == 0 || data.len() < i + 110 + nsz {
            break;
        }
        let name = String::from_utf8_lossy(&data[i + 110..i + 110 + nsz - 1]).into_owned();
        let o = (i + 110 + nsz + 3) & !3;
        if name == path {
            let end = (o + fsz).min(data.len());
            return Ok(String::from_utf8_lossy(&data[o..end]).into_owned());
        }
        if name == "TRAILER!!!" {
            break;
        }
        i = (o + fsz + 3) & !3;
    }
    Err(ImageError::Missing(format!(
        "no {path} in {}",
        image.display()
    )))
}

/// The test composer's plan an image carries (`/etc/hydo/compose.json`), or None.
pub fn embedded_compose(image: &Path) -> Option<String> {
    let t = embedded_file(image, "etc/hydo/compose.json").ok()?;
    t.trim().starts_with('{').then_some(t)
}

/// The agent protocol of an agent image: its `/etc/hydo/control` = `agent <protocol>` (every
/// image `hydo build` makes since Milestone 136). An image without the file is a shell-init image,
/// whose engine path was deleted in Milestone 145: it is refused with [`ImageError::ShellImage`]. A
/// control file with any other text is an error too: an image this hydo does not know how to boot.
pub fn control(image: &Path) -> Result<u64, ImageError> {
    std::fs::metadata(image)?; // a missing image is not a shell-init image
    let text = match embedded_file(image, "etc/hydo/control") {
        Ok(t) => t,
        Err(ImageError::Missing(_)) => {
            return Err(ImageError::ShellImage(image.display().to_string()))
        }
        Err(e) => return Err(e),
    };
    parse_control(&text).ok_or_else(|| {
        ImageError::Missing(format!(
            "{}: /etc/hydo/control says {:?}; this hydo boots \"agent <protocol>\" images",
            image.display(),
            text.trim()
        ))
    })
}

/// The text of `/etc/hydo/control`: `agent <protocol>` and a newline; the protocol.
pub fn parse_control(text: &str) -> Option<u64> {
    let mut w = text.split_whitespace();
    match (w.next(), w.next().and_then(|p| p.parse().ok()), w.next()) {
        (Some("agent"), Some(proto), None) => Some(proto),
        _ => None,
    }
}

/// The manifest an image carries (`/etc/hydo/hydo.yaml`, whatever its format).
pub fn embedded_manifest(image: &Path) -> Result<String, ImageError> {
    embedded_file(image, "etc/hydo/hydo.yaml")
}

/// Lower-case hex sha256 of a file.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::{parse_control, ImageError};

    #[test]
    fn control_file() {
        assert_eq!(parse_control("agent 1\n"), Some(1));
        assert_eq!(parse_control("agent 7"), Some(7));
        for bad in ["", "agent", "agent x", "agent 1 2", "shell", "Agent 1"] {
            assert_eq!(parse_control(bad), None, "{bad:?}");
        }
        assert_eq!(
            ImageError::ShellImage("build/explore-disk.img".into()).to_string(),
            "build/explore-disk.img: a shell-init image (no /etc/hydo/control), built before \
             Milestone 136; this hydo runs agent images only: rebuild it with hydo build"
        );
    }
}
