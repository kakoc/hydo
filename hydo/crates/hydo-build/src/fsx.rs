//! File operations with the semantics of the Python calls hydo-build made (`shutil.copy`,
//! `shutil.copy2`, `shutil.copytree(symlinks=True)`, `os.makedirs`, `open(.., "w")`, `os.walk`),
//! because the staged tree is what mke2fs -d and the cpio writer read: its names, file modes and
//! links must come out the same. Times do not matter (fix_mtimes sets every one afterwards).

use crate::Fail;
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub fn io_fail(what: &str, p: &Path, e: io::Error) -> Fail {
    Fail::Crash(format!("{what} {}: {e}", p.display()))
}

pub fn lexists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

pub fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// `os.makedirs(p, exist_ok=True)`.
pub fn makedirs(p: &Path) -> Result<(), Fail> {
    fs::create_dir_all(p).map_err(|e| io_fail("cannot create directory", p, e))
}

/// `open(p, "w").write(text)`.
pub fn write_text(p: &Path, text: &str) -> Result<(), Fail> {
    fs::write(p, text.as_bytes()).map_err(|e| io_fail("cannot write", p, e))
}

pub fn write_bytes(p: &Path, b: &[u8]) -> Result<(), Fail> {
    fs::write(p, b).map_err(|e| io_fail("cannot write", p, e))
}

pub fn chmod(p: &Path, mode: u32) -> Result<(), Fail> {
    fs::set_permissions(p, fs::Permissions::from_mode(mode))
        .map_err(|e| io_fail("cannot chmod", p, e))
}

/// `shutil.copyfile(src, dst)` (src followed) then `copymode`.
fn copy_content_and_mode(src: &Path, dst: &Path) -> Result<(), Fail> {
    let meta = fs::metadata(src).map_err(|e| io_fail("cannot read", src, e))?;
    {
        let mut fi = fs::File::open(src).map_err(|e| io_fail("cannot read", src, e))?;
        let mut fo = fs::File::create(dst).map_err(|e| io_fail("cannot write", dst, e))?;
        io::copy(&mut fi, &mut fo).map_err(|e| io_fail("cannot copy to", dst, e))?;
    }
    chmod(dst, meta.mode() & 0o7777)
}

/// `shutil.copy(src, dst)`: content and mode bits (into dst/basename(src) when dst is a directory).
pub fn copy_file(src: &Path, dst: &Path) -> Result<(), Fail> {
    let dst = if dst.is_dir() {
        dst.join(src.file_name().unwrap_or_default())
    } else {
        dst.to_path_buf()
    };
    copy_content_and_mode(src, &dst)
}

fn set_times_nofollow(p: &Path, at: (i64, i64), mt: (i64, i64)) -> Result<(), Fail> {
    let c =
        CString::new(p.as_os_str().as_bytes()).map_err(|_| Fail::Crash("NUL in a path".into()))?;
    let ts = [
        libc::timespec {
            tv_sec: at.0 as _,
            tv_nsec: at.1 as _,
        },
        libc::timespec {
            tv_sec: mt.0 as _,
            tv_nsec: mt.1 as _,
        },
    ];
    let r = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            ts.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if r != 0 {
        return Err(io_fail(
            "cannot set the times of",
            p,
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

/// `shutil.copystat(src, dst, follow_symlinks=...)`: mode (not of a symlink on Linux) and times.
fn copystat(src: &Path, dst: &Path, follow: bool) -> Result<(), Fail> {
    let m = if follow {
        fs::metadata(src)
    } else {
        fs::symlink_metadata(src)
    }
    .map_err(|e| io_fail("cannot stat", src, e))?;
    let link = m.file_type().is_symlink();
    if !link {
        chmod(dst, m.mode() & 0o7777)?;
    } else {
        #[cfg(target_os = "macos")]
        {
            let c = CString::new(dst.as_os_str().as_bytes())
                .map_err(|_| Fail::Crash("NUL in a path".into()))?;
            unsafe { libc::lchmod(c.as_ptr(), (m.mode() & 0o7777) as libc::mode_t) };
        }
    }
    set_times_nofollow(
        dst,
        (m.atime(), m.atime_nsec()),
        (m.mtime(), m.mtime_nsec()),
    )
}

/// `shutil.copy2(src, dst, follow_symlinks=False)`.
pub fn copy2_nofollow(src: &Path, dst: &Path) -> Result<(), Fail> {
    if is_link(src) {
        let t = fs::read_link(src).map_err(|e| io_fail("cannot read link", src, e))?;
        std::os::unix::fs::symlink(&t, dst).map_err(|e| io_fail("cannot create link", dst, e))?;
        return copystat(src, dst, false);
    }
    copy_content_and_mode(src, dst)?;
    copystat(src, dst, true)
}

/// `shutil.copytree(src, dst, symlinks=True, dirs_exist_ok=...)`.
pub fn copytree(src: &Path, dst: &Path) -> Result<(), Fail> {
    makedirs(dst)?;
    let mut ents: Vec<_> = fs::read_dir(src)
        .map_err(|e| io_fail("cannot list", src, e))?
        .filter_map(Result::ok)
        .collect();
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let s = e.path();
        let d = dst.join(e.file_name());
        let ft = e
            .file_type()
            .map_err(|err| io_fail("cannot stat", &s, err))?;
        if ft.is_symlink() {
            let t = fs::read_link(&s).map_err(|err| io_fail("cannot read link", &s, err))?;
            std::os::unix::fs::symlink(&t, &d)
                .map_err(|err| io_fail("cannot create link", &d, err))?;
            copystat(&s, &d, false)?;
        } else if ft.is_dir() {
            copytree(&s, &d)?;
        } else {
            copy2_nofollow(&s, &d)?;
        }
    }
    copystat(src, dst, true)
}

/// hydo-build's `copy_into(src, dst)`.
pub fn copy_into(src: &Path, dst: &Path) -> Result<(), Fail> {
    if src.is_dir() && !is_link(src) {
        return copytree(src, dst);
    }
    if let Some(p) = dst.parent() {
        makedirs(p)?;
    }
    if lexists(dst) {
        if !is_link(dst) {
            chmod(dst, 0o755)?;
        }
        fs::remove_file(dst).map_err(|e| io_fail("cannot remove", dst, e))?;
    }
    copy2_nofollow(src, dst)
}

/// `chmod -R u+w p; shutil.rmtree(p)` (or unlink a file or link).
pub fn force_remove(p: &Path) -> Result<(), Fail> {
    let _ = std::process::Command::new("chmod")
        .arg("-R")
        .arg("u+w")
        .arg(p)
        .status();
    if p.is_dir() && !is_link(p) {
        fs::remove_dir_all(p).map_err(|e| io_fail("cannot remove", p, e))
    } else {
        fs::remove_file(p).map_err(|e| io_fail("cannot remove", p, e))
    }
}

/// One step of `os.walk(top)` (top-down, links to directories listed with the directories but
/// not entered): (dir, dirs, files), every list in the order `sort_names` gives.
pub struct WalkEntry {
    pub dir: PathBuf,
    pub dirs: Vec<std::ffi::OsString>,
    pub files: Vec<std::ffi::OsString>,
}

/// `os.walk(top)` with `dirs.sort()` applied at each step (so the traversal is sorted); `files`
/// are sorted too (callers that care sort them anyway).
pub fn walk(top: &Path) -> Result<Vec<WalkEntry>, Fail> {
    let mut out = Vec::new();
    walk_into(top, &mut out)?;
    Ok(out)
}

fn walk_into(d: &Path, out: &mut Vec<WalkEntry>) -> Result<(), Fail> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let rd = match fs::read_dir(d) {
        Ok(r) => r,
        Err(_) => return Ok(()), // os.walk ignores directories it cannot list
    };
    for e in rd.filter_map(Result::ok) {
        let p = e.path();
        // os.scandir's is_dir() follows a symlink
        if p.is_dir() {
            dirs.push(e.file_name());
        } else {
            files.push(e.file_name());
        }
    }
    dirs.sort();
    files.sort();
    let sub: Vec<PathBuf> = dirs
        .iter()
        .map(|n| d.join(n))
        .filter(|p| !is_link(p))
        .collect();
    out.push(WalkEntry {
        dir: d.to_path_buf(),
        dirs,
        files,
    });
    for p in sub {
        walk_into(&p, out)?;
    }
    Ok(())
}

pub const FIXED_MTIME: i64 = 1600000000;

/// hydo-build's `fix_mtimes(root)`: every entry's (and the root's) atime and mtime to FIXED_MTIME,
/// links themselves (not their targets).
pub fn fix_mtimes(root: &Path) -> Result<(), Fail> {
    for w in walk(root)? {
        for n in w.dirs.iter().chain(w.files.iter()) {
            set_times_nofollow(&w.dir.join(n), (FIXED_MTIME, 0), (FIXED_MTIME, 0))?;
        }
    }
    set_times_nofollow(root, (FIXED_MTIME, 0), (FIXED_MTIME, 0))
}

/// `hashlib.sha256` of a file, hex.
pub fn sha(p: &Path) -> Result<String, Fail> {
    use sha2::{Digest, Sha256};
    let mut f = fs::File::open(p).map_err(|e| io_fail("cannot read", p, e))?;
    let mut h = Sha256::new();
    io::copy(&mut f, &mut h).map_err(|e| io_fail("cannot read", p, e))?;
    Ok(hex(&h.finalize()))
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn basename_str(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
