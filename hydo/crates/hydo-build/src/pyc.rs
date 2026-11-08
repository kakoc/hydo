//! Python bytecode caches in an image (`root.fix_pyc: true`, Milestone 196).
//!
//! hydo-build gives every file of an image one fixed time ([`crate::fsx::FIXED_MTIME`]), so that
//! two builds give the same bytes. A timestamp-checked `.pyc` file (PEP 3147's
//! `__pycache__/<module>.<tag>.pyc`, PEP 552's flags 0) records its source's modification time
//! and size; Python compares both with the source's and compiles the source again when either
//! differs. After the fixed time every such cache looked stale, so Python compiled the module in
//! every run, which costs guest time and, where the cache directory is writable, writes it
//! again; a test suite that turns warnings into errors failed on the compile warning (the
//! flaky-verdict experiment, docs/design/flaky-verdicts.md).
//!
//! [`fix_tree`] rewrites the header of every timestamp-checked cache whose source is in the
//! tree: the recorded time becomes the fixed time and the recorded size the source's size, so
//! Python takes the cache as it is. A hash-based cache, a cache without its source, a cache of
//! another layout (Python 2, or before 3.3) and a file that is not a cache are left alone. The
//! key is opt-in because it changes the bytes of every image that holds such a cache.

use crate::fsx::{io_fail, walk};
use crate::Fail;
use std::fs;
use std::path::{Path, PathBuf};

/// The first magic number with PEP 552's flags word (3.7), and the end of the range read here.
const MAGIC_FLAGS: u16 = 3390;
/// The first magic number with the source size in the header (3.3).
const MAGIC_SIZE: u16 = 3230;
const MAGIC_END: u16 = 4000;

/// Where a cache header keeps the source time and size, or None when this is not a
/// timestamp-checked cache this code knows.
fn fields(head: &[u8]) -> Option<usize> {
    if head.len() < 16 || &head[2..4] != b"\r\n" {
        return None;
    }
    let magic = u16::from_le_bytes([head[0], head[1]]);
    match magic {
        MAGIC_FLAGS..MAGIC_END => {
            let flags = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
            (flags == 0).then_some(8)
        }
        MAGIC_SIZE..MAGIC_FLAGS => Some(4),
        _ => None,
    }
}

/// A path of the guest's tree, resolved through symbolic links as the guest would (an absolute
/// target is inside the tree); None when it does not end at a regular file.
fn resolve(root: &Path, p: &Path) -> Option<PathBuf> {
    let mut cur = p.to_path_buf();
    for _ in 0..16 {
        let m = fs::symlink_metadata(&cur).ok()?;
        if m.is_file() {
            return Some(cur);
        }
        if !m.file_type().is_symlink() {
            return None;
        }
        let t = fs::read_link(&cur).ok()?;
        cur = if t.is_absolute() {
            root.join(t.strip_prefix("/").ok()?)
        } else {
            cur.parent()?.join(t)
        };
    }
    None
}

/// Rewrite the timestamp-checked Python caches under `root` to point at their sources' time
/// `mtime` and size. Returns how many files changed.
pub fn fix_tree(root: &Path, mtime: i64) -> Result<usize, Fail> {
    let mut n = 0;
    for w in walk(root)? {
        if w.dir.file_name().is_none_or(|d| d != "__pycache__") {
            continue;
        }
        let Some(pkg) = w.dir.parent() else { continue };
        for f in &w.files {
            let name = f.to_string_lossy();
            if !name.ends_with(".pyc") {
                continue;
            }
            let p = w.dir.join(f);
            if fs::symlink_metadata(&p).map_or(true, |m| !m.is_file()) {
                continue;
            }
            let module = name.split('.').next().unwrap_or("");
            let Some(src) = resolve(root, &pkg.join(format!("{module}.py"))) else {
                continue;
            };
            let size = fs::metadata(&src)
                .map_err(|e| io_fail("cannot read", &src, e))?
                .len();
            let mut b = fs::read(&p).map_err(|e| io_fail("cannot read", &p, e))?;
            let Some(at) = fields(&b) else { continue };
            let want_t = (mtime as u32).to_le_bytes();
            let want_s = (size as u32).to_le_bytes();
            if b[at..at + 4] == want_t && b[at + 4..at + 8] == want_s {
                continue;
            }
            b[at..at + 4].copy_from_slice(&want_t);
            b[at + 4..at + 8].copy_from_slice(&want_s);
            fs::write(&p, &b).map_err(|e| io_fail("cannot write", &p, e))?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(magic: u16, flags: Option<u32>, t: u32, s: u32) -> Vec<u8> {
        let mut h = magic.to_le_bytes().to_vec();
        h.extend_from_slice(b"\r\n");
        if let Some(f) = flags {
            h.extend_from_slice(&f.to_le_bytes());
        }
        h.extend_from_slice(&t.to_le_bytes());
        h.extend_from_slice(&s.to_le_bytes());
        h.extend_from_slice(b"\xe3code");
        h
    }

    #[test]
    fn timestamp_caches_get_the_fixed_time_and_the_sources_size() {
        let d = std::env::temp_dir().join(format!("hydo-pyc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let pkg = d.join("usr/lib/python3.11/site-packages/flask");
        fs::create_dir_all(pkg.join("__pycache__")).unwrap();
        fs::write(pkg.join("app.py"), "x = 1\n").unwrap();
        fs::write(pkg.join("hashed.py"), "y = 2\n").unwrap();
        // 3.11's magic is 3495; the source was 6 bytes at another time
        let c = pkg.join("__pycache__");
        fs::write(c.join("app.cpython-311.pyc"), header(3495, Some(0), 7, 99)).unwrap();
        fs::write(
            c.join("app.cpython-311.opt-1.pyc"),
            header(3495, Some(0), 7, 99),
        )
        .unwrap();
        // a hash-based cache, a cache without its source, a 3.6 cache (no flags word)
        fs::write(
            c.join("hashed.cpython-311.pyc"),
            header(3495, Some(1), 7, 99),
        )
        .unwrap();
        fs::write(c.join("gone.cpython-311.pyc"), header(3495, Some(0), 7, 99)).unwrap();
        fs::write(pkg.join("old.py"), "z\n").unwrap();
        fs::write(c.join("old.cpython-36.pyc"), header(3379, None, 7, 99)).unwrap();
        // a source that is a link to an absolute path inside the guest
        fs::write(d.join("real.py"), "abc\n").unwrap();
        std::os::unix::fs::symlink("/real.py", pkg.join("link.py")).unwrap();
        fs::write(c.join("link.cpython-311.pyc"), header(3495, Some(0), 7, 99)).unwrap();
        assert_eq!(fix_tree(&d, 1600000000).unwrap(), 4);
        let read = |n: &str| fs::read(c.join(n)).unwrap();
        assert_eq!(
            read("app.cpython-311.pyc"),
            header(3495, Some(0), 1600000000, 6)
        );
        assert_eq!(
            read("app.cpython-311.opt-1.pyc"),
            header(3495, Some(0), 1600000000, 6)
        );
        assert_eq!(read("hashed.cpython-311.pyc"), header(3495, Some(1), 7, 99));
        assert_eq!(read("gone.cpython-311.pyc"), header(3495, Some(0), 7, 99));
        assert_eq!(
            read("old.cpython-36.pyc"),
            header(3379, None, 1600000000, 2)
        );
        assert_eq!(
            read("link.cpython-311.pyc"),
            header(3495, Some(0), 1600000000, 4)
        );
        // a second pass changes nothing
        assert_eq!(fix_tree(&d, 1600000000).unwrap(), 0);
        let _ = fs::remove_dir_all(&d);
    }
}
