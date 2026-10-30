//! The arm64 initramfs: a newc cpio archive of the staged root, entries sorted by path, owners
//! 0:0 (or an image service's owners and modes), one fixed mtime, gzip-compressed exactly as
//! Python's `gzip.GzipFile(filename="", mtime=0, compresslevel=9)` writes it (zlib's deflate at
//! level 9, the header with XFL=2 and OS=255). Port of hydo_build_arm64.py's `_cpio`.

use crate::fsx::{self, io_fail};
use crate::Fail;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// An image service's file: (S_IFMT kind, permission bits, uid, gid).
pub type Meta = HashMap<String, (u32, u32, u32, u32)>;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFCHR: u32 = 0o020000;

struct Writer {
    buf: Vec<u8>,
    ino: u32,
    mtime: u64,
}

impl Writer {
    #[allow(clippy::too_many_arguments)]
    fn ent(
        &mut self,
        name: &[u8],
        mut mode: u32,
        data: &[u8],
        nlink: u32,
        rdev: (u32, u32),
        meta: Option<&Meta>,
    ) {
        let mut nb = name.to_vec();
        nb.push(0);
        let (mut uid, mut gid) = (0u32, 0u32);
        if let Some(m) = meta.and_then(|m| std::str::from_utf8(name).ok().and_then(|n| m.get(n))) {
            let (kind, perm, u, g) = *m;
            mode = (mode & S_IFMT) | if kind != S_IFLNK { perm } else { 0o777 };
            uid = u;
            gid = g;
        }
        let fields = [
            self.ino,
            mode,
            uid,
            gid,
            nlink,
            self.mtime as u32,
            data.len() as u32,
            0,
            0,
            rdev.0,
            rdev.1,
            nb.len() as u32,
            0,
        ];
        let mut hdr = String::from("070701");
        for v in fields {
            hdr.push_str(&format!("{v:08x}"));
        }
        self.ino += 1;
        self.buf.extend_from_slice(hdr.as_bytes());
        self.buf.extend_from_slice(&nb);
        let pad = (4 - (110 + nb.len()) % 4) % 4;
        self.buf.extend(std::iter::repeat_n(0u8, pad));
        self.buf.extend_from_slice(data);
        let pad = (4 - data.len() % 4) % 4;
        self.buf.extend(std::iter::repeat_n(0u8, pad));
    }
}

/// The relative paths `_cpio` archives: every directory, file and link under root (links to
/// directories as links, not entered), sorted.
fn paths_of(root: &Path) -> Result<Vec<Vec<u8>>, Fail> {
    let mut paths: Vec<Vec<u8>> = Vec::new();
    for w in fsx::walk(root)? {
        let rel = w.dir.strip_prefix(root).unwrap_or(Path::new(""));
        let relb = rel.as_os_str().as_bytes();
        let join = |n: &std::ffi::OsString| -> Vec<u8> {
            if relb.is_empty() {
                n.as_bytes().to_vec()
            } else {
                let mut v = relb.to_vec();
                v.push(b'/');
                v.extend_from_slice(n.as_bytes());
                v
            }
        };
        if !relb.is_empty() {
            paths.push(relb.to_vec());
        }
        for d in &w.dirs {
            if fsx::is_link(&w.dir.join(d)) {
                paths.push(join(d));
            }
        }
        for f in &w.files {
            paths.push(join(f));
        }
    }
    paths.sort();
    Ok(paths)
}

/// `_cpio(root, out_gz, mtime, meta)`.
pub fn write_cpio_gz(
    root: &Path,
    out_gz: &Path,
    mtime: u64,
    meta: Option<&Meta>,
    any_zlib: bool,
) -> Result<(), Fail> {
    let mut w = Writer {
        buf: Vec::new(),
        ino: 1,
        mtime,
    };
    let mut have_console = false;
    for p in paths_of(root)? {
        let full = root.join(std::ffi::OsStr::from_bytes(&p));
        if std::str::from_utf8(&p).is_err() {
            return Err(Fail::Exit(format!(
                "hydo-build: {}: a file name that is not UTF-8 cannot go into the initramfs",
                full.display()
            )));
        }
        let st = fs::symlink_metadata(&full).map_err(|e| io_fail("cannot stat", &full, e))?;
        if p == b"dev/console" {
            have_console = true;
        }
        let ft = st.file_type();
        if ft.is_symlink() {
            let t = fs::read_link(&full).map_err(|e| io_fail("cannot read link", &full, e))?;
            w.ent(
                &p,
                S_IFLNK | 0o777,
                t.as_os_str().as_bytes(),
                1,
                (0, 0),
                meta,
            );
        } else if ft.is_dir() {
            w.ent(&p, S_IFDIR | (st.mode() & 0o7777), &[], 2, (0, 0), meta);
        } else {
            let data = fs::read(&full).map_err(|e| io_fail("cannot read", &full, e))?;
            w.ent(&p, S_IFREG | (st.mode() & 0o7777), &data, 1, (0, 0), meta);
        }
    }
    if !have_console {
        w.ent(b"dev/console", S_IFCHR | 0o600, &[], 1, (5, 1), meta);
    }
    w.ent(b"TRAILER!!!", 0, &[], 1, (0, 0), meta);
    let gz = gzip_python(&w.buf, any_zlib)?;
    fsx::write_bytes(out_gz, &gz)
}

/// Python's `gzip.GzipFile(filename="", mode="wb", mtime=0, compresslevel=9).write(data)`.
/// `need_zlib`: the bytes must be zlib's (an image); false for a scratch archive whose bytes do
/// not matter (the Mac's mkfs helper VM).
pub fn gzip_python(data: &[u8], need_zlib: bool) -> Result<Vec<u8>, Fail> {
    if need_zlib && !crate::HAVE_ZLIB {
        return Err(Fail::Exit(
            "hydo-build: this build of hydo-build has no zlib (a static musl build), so it cannot \
             write an arm64 initramfs with the recorded bytes; build arm64 images with the Mac \
             release or a hydo-build built for the host"
                .into(),
        ));
    }
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 2, 255];
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(9));
    enc.write_all(data)
        .map_err(|e| Fail::Crash(format!("deflate: {e}")))?;
    let body = enc
        .finish()
        .map_err(|e| Fail::Crash(format!("deflate: {e}")))?;
    out.extend_from_slice(&body);
    let mut crc = flate2::Crc::new();
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    Ok(out)
}
