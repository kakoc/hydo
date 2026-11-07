//! The install cache's layout and its content store (Milestone 162,
//! docs/design/install-cache.md). Files only: no network here (the `hydo` command fetches, and
//! puts what it fetched in with [`Home::put_reader`]).
//!
//! ```text
//! <home>/store/<sha256>/<name>      one verified, read-only file per sha256
//! <home>/releases/<version>/SHA256SUMS
//! <home>/downloads/<sha256>.part
//! <home>/versions/<version>/
//! <home>/bin/hydo -> ../versions/<version>/bin/hydo
//! <home>/.lock
//! ```

use crate::kernels::sha256_of;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The cache's root (`$HYDO_HOME`, default `~/.hydo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Home {
    pub root: PathBuf,
}

static TMP: AtomicU64 = AtomicU64::new(0);

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn safe_name(n: &str) -> bool {
    !n.is_empty() && n != "." && n != ".." && !n.contains('/') && !n.starts_with(".tmp")
}

/// Lower-case hex of a digest.
pub fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lower-case hex SHA-256 of a file, read now (not remembered: for files that are being written,
/// such as downloads, where a remembered hash of an earlier file of the same size could be wrong).
pub fn sha256_file_now(p: &Path) -> std::io::Result<String> {
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
    Ok(hex(&h.finalize()))
}

impl Home {
    pub fn new(root: impl Into<PathBuf>) -> Home {
        Home { root: root.into() }
    }

    pub fn store(&self) -> PathBuf {
        self.root.join("store")
    }

    pub fn downloads(&self) -> PathBuf {
        self.root.join("downloads")
    }

    pub fn versions(&self) -> PathBuf {
        self.root.join("versions")
    }

    pub fn version(&self, v: &str) -> PathBuf {
        self.versions().join(v)
    }

    /// A release's `SHA256SUMS` asset as fetched.
    pub fn release_sums(&self, v: &str) -> PathBuf {
        self.root.join("releases").join(v).join("SHA256SUMS")
    }

    /// `<home>/bin`, the directory of the current version's link.
    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root.join(".lock")
    }

    /// The part file of a download of the file with this sha256.
    pub fn part(&self, sha256: &str) -> PathBuf {
        self.downloads().join(format!("{sha256}.part"))
    }

    /// The store entry with this sha256, verified: the one file in `store/<sha256>/` whose
    /// sha256 is that. An entry whose file does not match is deleted (it is a cache; the caller
    /// fetches it again).
    pub fn get(&self, sha256: &str) -> Option<PathBuf> {
        if !is_sha256(sha256) {
            return None;
        }
        let dir = self.store().join(sha256);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .ok()?
            .filter_map(Result::ok)
            .filter(|e| {
                !e.file_name().to_string_lossy().starts_with(".tmp")
                    && e.file_type().is_ok_and(|t| t.is_file())
            })
            .map(|e| e.path())
            .collect();
        files.sort();
        let f = files.into_iter().next()?;
        if sha256_of(&f).ok().as_deref() == Some(sha256) {
            return Some(f);
        }
        let _ = std::fs::remove_file(&f);
        let _ = std::fs::remove_dir(&dir);
        None
    }

    fn tmp_in(&self, dir: &Path) -> PathBuf {
        dir.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TMP.fetch_add(1, Ordering::SeqCst)
        ))
    }

    /// Move a verified temporary file into `store/<sha256>/<name>`, read-only.
    fn commit(&self, tmp: &Path, name: &str, sha256: &str) -> Result<PathBuf, String> {
        let dir = self.store().join(sha256);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let dest = dir.join(name);
        let _ = std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o444));
        std::fs::rename(tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        Ok(dest)
    }

    /// Put the bytes of `r` into the store as `name`, if their sha256 is `sha256`. Returns the
    /// entry; a mismatch is an error that names both sha256 values, and nothing is kept.
    pub fn put_reader(
        &self,
        r: &mut dyn Read,
        name: &str,
        sha256: &str,
    ) -> Result<PathBuf, String> {
        if !is_sha256(sha256) || !safe_name(name) {
            return Err(format!("not a store entry: {name} {sha256}"));
        }
        if let Some(p) = self.get(sha256) {
            return Ok(p);
        }
        let store = self.store();
        std::fs::create_dir_all(&store).map_err(|e| format!("{}: {e}", store.display()))?;
        let tmp = self.tmp_in(&store);
        let res = (|| {
            let mut f =
                std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
            let mut h = Sha256::new();
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let n = r.read(&mut buf).map_err(|e| format!("{name}: {e}"))?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
                f.write_all(&buf[..n])
                    .map_err(|e| format!("{}: {e}", tmp.display()))?;
            }
            f.sync_all()
                .map_err(|e| format!("{}: {e}", tmp.display()))?;
            let got = hex(&h.finalize());
            if got != sha256 {
                return Err(format!(
                    "{name} has sha256 {got}, not the expected {sha256}; refused"
                ));
            }
            self.commit(&tmp, name, sha256)
        })();
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        res
    }

    /// Put a file into the store as `name` (a hard link when it is on the same file system,
    /// else a copy), if its sha256 is `sha256`.
    pub fn put_file(&self, src: &Path, name: &str, sha256: &str) -> Result<PathBuf, String> {
        if let Some(p) = self.get(sha256) {
            return Ok(p);
        }
        if sha256_file_now(src).map_err(|e| format!("{}: {e}", src.display()))? != sha256 {
            return Err(format!(
                "{} does not have sha256 {sha256}; refused",
                src.display()
            ));
        }
        let store = self.store();
        std::fs::create_dir_all(&store).map_err(|e| format!("{}: {e}", store.display()))?;
        let tmp = self.tmp_in(&store);
        // a hard link shares the inode: only for a file nothing changes in place (the caller's
        // business), and it keeps the file's mode, so the link is not made read-only
        if std::fs::hard_link(src, &tmp).is_ok() {
            let dir = self.store().join(sha256);
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let dest = dir.join(name);
            return std::fs::rename(&tmp, &dest)
                .map(|_| dest.clone())
                .map_err(|e| {
                    let _ = std::fs::remove_file(&tmp);
                    format!("{}: {e}", dest.display())
                });
        }
        let mut f = std::fs::File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
        self.put_reader(&mut f, name, sha256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hydo-store-{tag}-{}", std::process::id()));
        if d.exists() {
            for e in walk(&d) {
                let _ = std::fs::set_permissions(&e, std::fs::Permissions::from_mode(0o755));
            }
            let _ = std::fs::remove_dir_all(&d);
        }
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn walk(d: &Path) -> Vec<PathBuf> {
        let mut v = vec![d.to_path_buf()];
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    v.extend(walk(&e.path()));
                } else {
                    v.push(e.path());
                }
            }
        }
        v
    }

    fn sha(b: &[u8]) -> String {
        hex(&Sha256::digest(b))
    }

    #[test]
    fn entries_are_verified_in_and_out() {
        let d = scratch("rw");
        let h = Home::new(d.join("home"));
        let s = sha(b"kernel");
        assert_eq!(h.get(&s), None);
        // wrong bytes are refused and leave nothing
        let e = h
            .put_reader(&mut &b"kernel?"[..], "bzImage", &s)
            .unwrap_err();
        assert!(e.contains("refused") && e.contains(&s), "{e}");
        assert_eq!(std::fs::read_dir(h.store()).unwrap().count(), 0);
        let p = h.put_reader(&mut &b"kernel"[..], "bzImage", &s).unwrap();
        assert_eq!(p, h.store().join(&s).join("bzImage"));
        assert_eq!(h.get(&s), Some(p.clone()));
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o444);
        // a file put in again is the same entry
        let src = d.join("other-name");
        std::fs::write(&src, b"kernel").unwrap();
        assert_eq!(h.put_file(&src, "other-name", &s).unwrap(), p);
        // a damaged entry is not returned, and is gone
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&p, b"damaged").unwrap();
        assert_eq!(h.get(&s), None);
        assert!(!p.exists());
        // put_file of a file with other bytes is refused
        std::fs::write(&src, b"not it").unwrap();
        assert!(h.put_file(&src, "x", &s).is_err());
        assert!(h.put_reader(&mut &b""[..], "../x", &s).is_err());
    }
}
