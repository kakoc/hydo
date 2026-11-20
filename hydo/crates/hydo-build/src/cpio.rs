//! The arm64 initramfs: a newc cpio archive of the staged root, entries sorted by path, owners
//! 0:0 (or an image service's owners and modes), one fixed mtime, gzip-compressed exactly as
//! Python's `gzip.GzipFile(filename="", mtime=0, compresslevel=9)` writes it (zlib's deflate at
//! level 9, the header with XFL=2 and OS=255). Port of hydo_build_arm64.py's `_cpio`.
//!
//! Hard links (Milestone 338, from the fingerprint epoch 5,
//! [`hydo_image::EpochDefaults::cpio_hard_links`]): the paths of the tree that are one file get
//! one inode number and their count of links, and the first of them in path order carries the
//! bytes. The kernel's unpacker (`init/initramfs.c`, `maybe_link`) and busybox's `cpio -i` (which
//! unpacks the root of `guest.root: disk`) make the later paths links to the first. Before, each
//! path was a file of its own: the busybox image's 400 applets were 400 copies of busybox in the
//! guest's RAM.

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
pub const S_IFIFO: u32 = 0o010000;

struct Writer<W: Write> {
    buf: W,
    ino: u32,
    mtime: u64,
    /// the first write error (the archive is then incomplete)
    err: Option<std::io::Error>,
}

impl<W: Write> Writer<W> {
    fn put(&mut self, b: &[u8]) {
        if self.err.is_none() {
            if let Err(e) = self.buf.write_all(b) {
                self.err = Some(e);
            }
        }
    }

    fn ent(
        &mut self,
        name: &[u8],
        mode: u32,
        data: &[u8],
        nlink: u32,
        rdev: (u32, u32),
        meta: Option<&Meta>,
    ) {
        let ino = self.ino;
        self.ino += 1;
        self.ent_ino(ino, name, mode, data, nlink, rdev, meta);
    }

    /// An entry with the inode number `ino`; a later link of a file reuses the first's.
    #[allow(clippy::too_many_arguments)]
    fn ent_ino(
        &mut self,
        ino: u32,
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
            ino,
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
        self.put(hdr.as_bytes());
        self.put(&nb);
        let pad = (4 - (110 + nb.len()) % 4) % 4;
        self.put(&[0u8; 4][..pad]);
        self.put(data);
        let pad = (4 - data.len() % 4) % 4;
        self.put(&[0u8; 4][..pad]);
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

/// `_cpio(root, out_gz, mtime, meta)`. `links`: keep the tree's hard links as links (from the
/// fingerprint epoch 5, [`hydo_image::EpochDefaults::cpio_hard_links`]); false writes each path
/// as a file of its own, as before.
pub fn write_cpio_gz(
    root: &Path,
    out_gz: &Path,
    mtime: u64,
    meta: Option<&Meta>,
    any_zlib: bool,
    links: bool,
) -> Result<(), Fail> {
    let mut w = Writer {
        buf: Vec::new(),
        ino: 1,
        mtime,
        err: None,
    };
    archive(root, &mut w, meta, links)?;
    let gz = gzip_python(&w.buf, any_zlib)?;
    fsx::write_bytes(out_gz, &gz)
}

/// The same newc archive as [`write_cpio_gz`], not compressed and written to `out` as it is
/// made: the root of an image whose root file system is on its disk (`guest.root: disk`,
/// Milestone 284), which the Mac's mkfs helper VM unpacks into the new file system.
pub fn write_cpio_file(
    root: &Path,
    out: &Path,
    mtime: u64,
    meta: Option<&Meta>,
    links: bool,
) -> Result<u64, Fail> {
    let f = fs::File::create(out).map_err(|e| io_fail("cannot write", out, e))?;
    let mut w = Writer {
        buf: CountingWriter {
            inner: std::io::BufWriter::with_capacity(1 << 20, f),
            n: 0,
        },
        ino: 1,
        mtime,
        err: None,
    };
    archive(root, &mut w, meta, links)?;
    if let Some(e) = w.err.take() {
        return Err(io_fail("cannot write", out, e));
    }
    w.buf
        .inner
        .flush()
        .map_err(|e| io_fail("cannot write", out, e))?;
    Ok(w.buf.n)
}

struct CountingWriter<W: Write> {
    inner: W,
    n: u64,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let k = self.inner.write(b)?;
        self.n += k as u64;
        Ok(k)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Does an image service's meta say that this path is a FIFO?
fn is_fifo(meta: Option<&Meta>, p: &[u8]) -> bool {
    meta.zip(std::str::from_utf8(p).ok())
        .and_then(|(m, n)| m.get(n))
        .is_some_and(|e| e.0 == S_IFIFO)
}

/// How many paths of the tree each file with more than one link has, by device and inode: the
/// count its entries carry. A link that leads out of the tree (a file of the host's store linked
/// into it) does not count.
fn link_counts(root: &Path, paths: &[Vec<u8>]) -> Result<HashMap<(u64, u64), u32>, Fail> {
    let mut n: HashMap<(u64, u64), u32> = HashMap::new();
    for p in paths {
        let full = root.join(std::ffi::OsStr::from_bytes(p));
        let st = fs::symlink_metadata(&full).map_err(|e| io_fail("cannot stat", &full, e))?;
        if st.file_type().is_file() && st.nlink() > 1 {
            *n.entry((st.dev(), st.ino())).or_default() += 1;
        }
    }
    n.retain(|_, c| *c > 1);
    Ok(n)
}

/// Every entry of `root`, sorted, then `dev/console` when the tree has none, then the trailer.
/// `links`: a file of several paths is one inode with its count of links, and only its first
/// path carries the bytes ([`write_cpio_gz`]).
fn archive<W: Write>(
    root: &Path,
    w: &mut Writer<W>,
    meta: Option<&Meta>,
    links: bool,
) -> Result<(), Fail> {
    let mut have_console = false;
    let paths = paths_of(root)?;
    let counts = if links {
        link_counts(root, &paths)?
    } else {
        HashMap::new()
    };
    // the inode number that each linked file's first entry got
    let mut first: HashMap<(u64, u64), u32> = HashMap::new();
    for p in paths {
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
        } else if is_fifo(meta, &p) {
            // an image service's FIFO (Milestone 316): an empty file in the staged tree
            w.ent(&p, S_IFIFO | (st.mode() & 0o7777), &[], 1, (0, 0), meta);
        } else if let Some(&n) = counts.get(&(st.dev(), st.ino())) {
            let mode = S_IFREG | (st.mode() & 0o7777);
            match first.get(&(st.dev(), st.ino())) {
                Some(&ino) => w.ent_ino(ino, &p, mode, &[], n, (0, 0), meta),
                None => {
                    let data = fs::read(&full).map_err(|e| io_fail("cannot read", &full, e))?;
                    first.insert((st.dev(), st.ino()), w.ino);
                    w.ent(&p, mode, &data, n, (0, 0), meta);
                }
            }
        } else {
            let data = fs::read(&full).map_err(|e| io_fail("cannot read", &full, e))?;
            w.ent(&p, S_IFREG | (st.mode() & 0o7777), &data, 1, (0, 0), meta);
        }
    }
    if !have_console {
        w.ent(b"dev/console", S_IFCHR | 0o600, &[], 1, (5, 1), meta);
    }
    w.ent(b"TRAILER!!!", 0, &[], 1, (0, 0), meta);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn the_plain_archive_has_the_bytes_of_the_compressed_one() {
        let d = crate::tempdir("hydo-cpio-test-").unwrap();
        let r = d.join("root");
        fs::create_dir_all(r.join("etc/hydo")).unwrap();
        fs::write(r.join("etc/hydo/control"), "agent 1\n").unwrap();
        fs::write(r.join("init"), "#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink("init", r.join("sbin-init")).unwrap();
        let mut meta = Meta::new();
        meta.insert("init".into(), (S_IFREG, 0o750, 7, 8));
        write_cpio_gz(&r, &d.join("a.cpio.gz"), 1, Some(&meta), false, true).unwrap();
        let n = write_cpio_file(&r, &d.join("a.cpio"), 1, Some(&meta), true).unwrap();
        let mut unz = Vec::new();
        flate2::read::GzDecoder::new(fs::File::open(d.join("a.cpio.gz")).unwrap())
            .read_to_end(&mut unz)
            .unwrap();
        let plain = fs::read(d.join("a.cpio")).unwrap();
        let _ = fs::remove_dir_all(&d);
        assert_eq!(plain.len() as u64, n);
        assert_eq!(plain, unz);
    }

    /// Milestone 316: an image service's FIFO is an empty file in the staged tree and a FIFO
    /// entry in the archive, with the layer's mode and owners and no data.
    #[test]
    fn an_image_fifo_is_a_fifo_entry() {
        let d = crate::tempdir("hydo-cpio-fifo-").unwrap();
        let r = d.join("root");
        fs::create_dir_all(r.join("oci/s/run")).unwrap();
        fs::write(r.join("oci/s/run/pipe"), "").unwrap();
        fs::write(r.join("oci/s/run/file"), "").unwrap();
        let mut meta = Meta::new();
        meta.insert("oci/s/run/pipe".into(), (S_IFIFO, 0o666, 999, 999));
        let n = write_cpio_file(&r, &d.join("a.cpio"), 1, Some(&meta), true).unwrap();
        let a = fs::read(d.join("a.cpio")).unwrap();
        let _ = fs::remove_dir_all(&d);
        assert_eq!(a.len() as u64, n);
        // each newc header: magic, then inode, mode, uid, gid, nlink, mtime, size as 8 hex digits
        let entry = |name: &str| -> (u32, u32, u32) {
            let at = a
                .windows(name.len() + 1)
                .position(|w| w == [name.as_bytes(), b"\0"].concat())
                .unwrap()
                - 110;
            let field = |i: usize| {
                u32::from_str_radix(std::str::from_utf8(&a[at + 6 + 8 * i..][..8]).unwrap(), 16)
                    .unwrap()
            };
            (field(1), field(2), field(6))
        };
        assert_eq!(entry("oci/s/run/pipe"), (S_IFIFO | 0o666, 999, 0));
        assert_eq!(entry("oci/s/run/file").0 & S_IFMT, S_IFREG);
    }

    /// The newc header fields of `name`'s entry: inode, mode, nlink and size.
    fn fields(a: &[u8], name: &str) -> (u32, u32, u32, u32) {
        let at = a
            .windows(name.len() + 1)
            .position(|w| w == [name.as_bytes(), b"\0"].concat())
            .unwrap()
            - 110;
        let field = |i: usize| {
            u32::from_str_radix(std::str::from_utf8(&a[at + 6 + 8 * i..][..8]).unwrap(), 16)
                .unwrap()
        };
        (field(0), field(1), field(4), field(6))
    }

    /// Milestone 338: with `links`, the paths of one file are one inode with their count of
    /// links, and only the first in path order carries the bytes; without, each path is a whole
    /// copy, as before epoch 5. A link to a file outside the tree counts only the tree's paths.
    #[test]
    fn hard_links_stay_links() {
        let d = crate::tempdir("hydo-cpio-links-").unwrap();
        let r = d.join("root");
        fs::create_dir_all(r.join("bin")).unwrap();
        fs::write(r.join("bin/busybox"), vec![7u8; 5000]).unwrap();
        for a in ["ls", "sh", "cat"] {
            fs::hard_link(r.join("bin/busybox"), r.join("bin").join(a)).unwrap();
        }
        fs::write(d.join("outside"), "out\n").unwrap();
        fs::hard_link(d.join("outside"), r.join("bin/alone")).unwrap();
        fs::write(r.join("bin/other"), "x\n").unwrap();
        let old = write_cpio_file(&r, &d.join("old.cpio"), 1, None, false).unwrap();
        let new = write_cpio_file(&r, &d.join("new.cpio"), 1, None, true).unwrap();
        let (a, b) = (
            fs::read(d.join("old.cpio")).unwrap(),
            fs::read(d.join("new.cpio")).unwrap(),
        );
        let _ = fs::remove_dir_all(&d);
        assert_eq!((a.len() as u64, b.len() as u64), (old, new));
        // before: four copies of 5000 bytes, each its own inode with one link
        assert!(old > 20000 && new < 7000, "{old} {new}");
        let olds: Vec<_> = ["bin/busybox", "bin/cat", "bin/ls", "bin/sh"]
            .iter()
            .map(|n| fields(&a, n))
            .collect();
        assert!(olds.iter().all(|f| f.2 == 1 && f.3 == 5000));
        // now: bin/busybox is first in path order and has the bytes; cat, ls and sh link to it
        let first = fields(&b, "bin/busybox");
        assert_eq!((first.2, first.3), (4, 5000));
        for n in ["bin/cat", "bin/ls", "bin/sh"] {
            assert_eq!(fields(&b, n), (first.0, first.1, 4, 0));
        }
        assert_eq!(fields(&b, "bin/alone").2, 1);
        assert_eq!(fields(&b, "bin/alone").3, 4);
        // the inode numbers after the links go on without a gap
        assert_eq!(fields(&b, "bin/other").0, first.0 + 1);
        // a tree without links gives the same bytes either way
        let d = crate::tempdir("hydo-cpio-nolinks-").unwrap();
        fs::create_dir_all(d.join("root/etc")).unwrap();
        fs::write(d.join("root/etc/a"), "a\n").unwrap();
        write_cpio_file(&d.join("root"), &d.join("x"), 1, None, false).unwrap();
        write_cpio_file(&d.join("root"), &d.join("y"), 1, None, true).unwrap();
        let same = fs::read(d.join("x")).unwrap() == fs::read(d.join("y")).unwrap();
        let _ = fs::remove_dir_all(&d);
        assert!(same);
    }
}
