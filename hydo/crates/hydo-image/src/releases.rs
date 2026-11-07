//! The release index (Milestone 162, docs/design/install-cache.md): which published hydo release
//! has which archive, and which guest kernels and initramfs files are inside each archive, all by
//! sha256. The index is `hydo/dist/releases.txt`, compiled in ([`INDEX_TEXT`]); the running hydo's
//! own version is added from the kernels it pins ([`crate::kernels::KNOWN`]), because its own
//! archive cannot be in its own program.
//!
//! The format (one entry per line, `#` comments and empty lines skipped, fields separated by
//! blanks):
//!
//! ```text
//! archive <version> <platform> <sha256> hydo-<platform>-<version>.tar.gz
//! file    <version> <platform> <sha256> <path below the archive's top directory>
//! ```
//!
//! `<platform>` is `linux` or `mac`. Additions only: a published release's lines never change.

use crate::kernels::{Known, Role, KNOWN};
use std::sync::OnceLock;

/// `hydo/dist/releases.txt`, read through the link `pins/releases.txt` in this crate, so that the
/// crate published to crates.io carries it (docs/PUBLISHING.md).
pub const INDEX_TEXT: &str = include_str!("../pins/releases.txt");

/// A release archive the index pins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Archive {
    pub version: String,
    pub platform: String,
    /// None: not pinned (the running hydo's own version); the release's `SHA256SUMS` asset
    /// gives it.
    pub sha256: Option<String>,
    pub file: String,
}

/// A file inside a release archive (a guest kernel or initramfs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub version: String,
    pub platform: String,
    pub sha256: String,
    /// The path below the archive's top directory (`guest/bzImage`).
    pub path: String,
}

impl Member {
    /// The file's name (the path's last part).
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// The releases one hydo knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    pub archives: Vec<Archive>,
    pub files: Vec<Member>,
}

/// `hydo-<platform>-<version>.tar.gz`.
pub fn archive_name(platform: &str, version: &str) -> String {
    format!("hydo-{platform}-{version}.tar.gz")
}

/// The archive's top directory, `hydo-<platform>-<version>`.
pub fn top_dir(platform: &str, version: &str) -> String {
    format!("hydo-{platform}-{version}")
}

/// (platform, version) of an archive file name `hydo-<platform>-<version>.tar.gz`.
pub fn parse_archive_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("hydo-")?.strip_suffix(".tar.gz")?;
    let (p, v) = rest.split_once('-')?;
    (matches!(p, "linux" | "mac") && valid_version(v) && !v.ends_with("-sources"))
        .then(|| (p.to_string(), v.to_string()))
}

/// A version as releases use them: digits and dots, optionally `-` and a pre-release word of
/// letters, digits and dots (`0.1.0`, `0.2.0-rc.1`).
pub fn valid_version(v: &str) -> bool {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let core_ok = !core.is_empty()
        && core.split('.').count() == 3
        && core
            .split('.')
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let pre_ok = pre
        .is_none_or(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.'));
    core_ok && pre_ok
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn safe_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

/// The platform of a kernel's architecture (x86_64 kernels ship in the Linux release, arm64
/// kernels in the Mac release).
pub fn platform_of_arch(arch: &str) -> &'static str {
    if arch == "arm64" {
        "mac"
    } else {
        "linux"
    }
}

/// Where a kernel this hydo knows lies in its release's archive (below the top directory), as
/// the release scripts place it (hydo-cli's `asset_release_path`).
pub fn release_path(k: &Known) -> String {
    match (k.arch, k.role) {
        ("arm64", _) => format!("hv/hvm/explore/out/{}", k.file),
        (_, Role::Kernel | Role::Initrd) => format!("guest/{}", k.file),
    }
}

impl Index {
    /// Parse the index text.
    pub fn parse(text: &str) -> Result<Index, String> {
        let mut ix = Index::default();
        for (i, l) in text.lines().enumerate() {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = l.split_whitespace().collect();
            let bad = |why: &str| format!("line {}: {why}: {l}", i + 1);
            if f.len() != 5 {
                return Err(bad("not `<kind> <version> <platform> <sha256> <path>`"));
            }
            let (kind, version, platform, sha, path) = (f[0], f[1], f[2], f[3], f[4]);
            if !valid_version(version) {
                return Err(bad("not a version"));
            }
            if !matches!(platform, "linux" | "mac") {
                return Err(bad("the platform is linux or mac"));
            }
            if !is_sha256(sha) {
                return Err(bad("not a lower-case sha256"));
            }
            match kind {
                "archive" => {
                    if path != archive_name(platform, version) {
                        return Err(bad(&format!(
                            "the archive of {platform} {version} is {}",
                            archive_name(platform, version)
                        )));
                    }
                    if ix.archive(platform, version).is_some() {
                        return Err(bad("a second archive line for one release"));
                    }
                    ix.archives.push(Archive {
                        version: version.into(),
                        platform: platform.into(),
                        sha256: Some(sha.into()),
                        file: path.into(),
                    });
                }
                "file" => {
                    if !safe_path(path) {
                        return Err(bad("not a relative path inside the archive"));
                    }
                    if ix
                        .files
                        .iter()
                        .any(|m| m.version == version && m.platform == platform && m.path == path)
                    {
                        return Err(bad("a second line for one file"));
                    }
                    ix.files.push(Member {
                        version: version.into(),
                        platform: platform.into(),
                        sha256: sha.into(),
                        path: path.into(),
                    });
                }
                _ => return Err(bad("the kind is archive or file")),
            }
        }
        for m in &ix.files {
            if ix.archive(&m.platform, &m.version).is_none() {
                return Err(format!(
                    "{} of {} {} has no archive line",
                    m.path, m.platform, m.version
                ));
            }
        }
        Ok(ix)
    }

    /// This index with `version` (the running hydo's) added from the kernels it pins, when the
    /// index has no lines for that version yet. Its archives are not pinned.
    pub fn with_own(mut self, version: &str, known: &[Known]) -> Index {
        for platform in ["linux", "mac"] {
            if self.archive(platform, version).is_some() {
                continue;
            }
            self.archives.push(Archive {
                version: version.into(),
                platform: platform.into(),
                sha256: None,
                file: archive_name(platform, version),
            });
            for k in known
                .iter()
                .filter(|k| platform_of_arch(k.arch) == platform)
            {
                self.files.push(Member {
                    version: version.into(),
                    platform: platform.into(),
                    sha256: k.sha256.into(),
                    path: release_path(k),
                });
            }
        }
        self
    }

    /// The archive of a release.
    pub fn archive(&self, platform: &str, version: &str) -> Option<&Archive> {
        self.archives
            .iter()
            .find(|a| a.platform == platform && a.version == version)
    }

    /// The files of `platform`'s releases with this sha256, the newest release first.
    pub fn with_sha256(&self, platform: &str, sha256: &str) -> Vec<&Member> {
        let mut v: Vec<&Member> = self
            .files
            .iter()
            .filter(|m| m.platform == platform && m.sha256 == sha256)
            .collect();
        v.sort_by(|a, b| version_key(&b.version).cmp(&version_key(&a.version)));
        v
    }

    /// The guest files of one release.
    pub fn files_of(&self, platform: &str, version: &str) -> Vec<&Member> {
        self.files
            .iter()
            .filter(|m| m.platform == platform && m.version == version)
            .collect()
    }

    /// The versions with an archive for `platform`, oldest first.
    pub fn versions(&self, platform: &str) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .archives
            .iter()
            .filter(|a| a.platform == platform)
            .map(|a| a.version.as_str())
            .collect();
        v.sort_by_key(|x| version_key(x));
        v
    }
}

/// A sort key for versions (numeric parts; a pre-release before its release).
pub fn version_key(v: &str) -> (Vec<u64>, bool, String) {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    (
        core.split('.').map(|n| n.parse().unwrap_or(0)).collect(),
        pre.is_none(),
        pre.unwrap_or("").to_string(),
    )
}

/// The index this hydo knows: [`INDEX_TEXT`] plus its own version.
pub fn index() -> &'static Index {
    static IX: OnceLock<Index> = OnceLock::new();
    IX.get_or_init(|| {
        Index::parse(INDEX_TEXT)
            .expect("hydo/dist/releases.txt is checked by hydo-image's tests")
            .with_own(crate::hydo_version(), &KNOWN)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compiled_index_parses_and_knows_this_version() {
        let ix = Index::parse(INDEX_TEXT).unwrap();
        assert!(!ix.archives.is_empty());
        let all = index();
        for k in KNOWN {
            let p = platform_of_arch(k.arch);
            let m = all.with_sha256(p, k.sha256);
            assert!(
                m.iter().any(|m| m.version == crate::hydo_version()),
                "{} of this version is not in the index",
                k.file
            );
            assert!(all.archive(p, crate::hydo_version()).is_some());
        }
    }

    #[test]
    fn the_first_release_is_pinned_with_its_kernels() {
        let ix = Index::parse(INDEX_TEXT).unwrap();
        for p in ["linux", "mac"] {
            let a = ix.archive(p, "0.1.0").expect("0.1.0 is published");
            assert_eq!(a.file, archive_name(p, "0.1.0"));
            assert!(a.sha256.is_some());
        }
        // 0.1.0's guest kernels, as its tarballs' SHA256SUMS list them (released: never changes)
        let mut got: Vec<(String, String)> = ix
            .files_of("linux", "0.1.0")
            .into_iter()
            .chain(ix.files_of("mac", "0.1.0"))
            .map(|m| (m.path.clone(), m.sha256[..16].to_string()))
            .collect();
        got.sort();
        let want = [
            ("guest/bzImage", "0a9ede352483e171"),
            ("guest/initramfs-kafka.cpio.gz", "d074d764c76ce119"),
            ("hv/hvm/explore/out/Image-explore", "f80c565589f1fecf"),
            ("hv/hvm/explore/out/Image-explore-netns", "806807e7be5f55da"),
        ];
        let want: Vec<(String, String)> = want
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn malformed_lines_are_refused() {
        let s = "a".repeat(64);
        for bad in [
            format!("archive 0.1.0 linux {s} hydo-mac-0.1.0.tar.gz"),
            format!("archive 0.1 linux {s} hydo-linux-0.1.tar.gz"),
            format!("archive 0.1.0 windows {s} hydo-windows-0.1.0.tar.gz"),
            "archive 0.1.0 linux ABC hydo-linux-0.1.0.tar.gz".to_string(),
            format!("file 0.1.0 linux {s} guest/bzImage"),
            format!("archive 0.1.0 linux {s} hydo-linux-0.1.0.tar.gz\nfile 0.1.0 linux {s} ../x"),
            format!("archive 0.1.0 linux {s} hydo-linux-0.1.0.tar.gz\nfile 0.1.0 linux {s} /x"),
            format!("archive 0.1.0 linux {s} hydo-linux-0.1.0.tar.gz\narchive 0.1.0 linux {s} hydo-linux-0.1.0.tar.gz"),
            format!("blob 0.1.0 linux {s} x"),
        ] {
            assert!(Index::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_and_versions() {
        assert_eq!(
            parse_archive_name("hydo-linux-0.1.0.tar.gz"),
            Some(("linux".into(), "0.1.0".into()))
        );
        assert_eq!(
            parse_archive_name("hydo-mac-0.2.0-rc.1.tar.gz"),
            Some(("mac".into(), "0.2.0-rc.1".into()))
        );
        assert_eq!(parse_archive_name("hydo-linux-0.1.0-sources.tar.gz"), None);
        assert_eq!(parse_archive_name("hydo-linux-../x.tar.gz"), None);
        assert!(version_key("0.10.0") > version_key("0.9.1"));
        assert!(version_key("0.2.0") > version_key("0.2.0-rc.1"));
    }

    #[test]
    fn newest_release_first() {
        let s = "b".repeat(64);
        let t = format!(
            "archive 0.1.0 linux {a} hydo-linux-0.1.0.tar.gz\n\
             archive 0.10.0 linux {a} hydo-linux-0.10.0.tar.gz\n\
             archive 0.2.0 linux {a} hydo-linux-0.2.0.tar.gz\n\
             file 0.1.0 linux {s} guest/bzImage\n\
             file 0.10.0 linux {s} guest/bzImage\n\
             file 0.2.0 linux {s} guest/bzImage\n",
            a = "a".repeat(64)
        );
        let ix = Index::parse(&t).unwrap();
        let v: Vec<&str> = ix
            .with_sha256("linux", &s)
            .iter()
            .map(|m| m.version.as_str())
            .collect();
        assert_eq!(v, ["0.10.0", "0.2.0", "0.1.0"]);
        assert_eq!(ix.versions("linux"), ["0.1.0", "0.2.0", "0.10.0"]);
        assert!(ix.with_sha256("mac", &s).is_empty());
    }
}
