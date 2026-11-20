//! Page-aligned host memory: guest RAM (anonymous, locked when asked), the snapshot buffers
//! (an unlinked file in `HVM_SNAP_DIR`, mapped shared, as the C engine kept them, so a snapshot
//! is paged to its file instead of the swap), and the RAM bases that guest RAM is mapped from
//! privately ([`Base`], Milestone 335). The only `unsafe` besides [`crate::hv`].
//!
//! **RAM bases** (docs/design/mac-cow.md). A [`Base`] is a snapshot's guest RAM in a file that
//! nothing writes after it is made. [`Mem::map_base`] maps guest RAM from it with `MAP_PRIVATE`:
//! every page reads the file until something writes it (the guest through Hypervisor.framework,
//! or the engine's devices), and the macOS kernel copies a page at its first write, so only the
//! written pages are the process's own memory. A restore maps the base again, which drops every
//! page the run wrote at once. A shared base is named by its length and its RAM tree hash
//! (`ram-v1-<len>-<hash>` in `HVM_SNAP_DIR`), so the workers of one exploration, which boot to
//! the same bytes, find one another's file and map it, and its pages are in the page cache once.
//! A file found by its name is used only when its bytes equal the RAM's, compared page by page;
//! otherwise the engine makes a file of its own. Each process holds a shared `flock` on every base
//! it uses; a base nobody holds is removed ([`remove_stale`]).
//!
//! A base file holds only the RAM's pages that are not zero, in extents; the others are holes.
//! The fingerprint reads every page, and a read of a hole makes a page of zeros in the file's page
//! cache, once for all the processes that map the file. Anonymous memory in the holes would cost
//! each process its own page instead: macOS gives an anonymous page that is read a page of its own.

use std::os::unix::io::AsRawFd;

/// A mapping of `len` bytes, unmapped on drop.
pub struct Mem {
    p: *mut u8,
    len: usize,
}

// SAFETY: the mapping is plain memory owned by this value; it moves with its owner
unsafe impl Send for Mem {}

impl Mem {
    /// Anonymous, zeroed memory.
    pub fn anon(len: usize) -> std::io::Result<Mem> {
        // SAFETY: a fresh private anonymous mapping
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Mem { p: p.cast(), len })
    }

    /// `len` bytes backed by `<dir>/<name>.<pid>`, created, mapped and unlinked at once.
    pub fn file_backed(dir: &str, name: &str, len: usize) -> std::io::Result<Mem> {
        let _ = std::fs::DirBuilder::new().mode_0700().create(dir);
        let path = format!("{dir}/{name}.{}", std::process::id());
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| std::io::Error::new(e.kind(), format!("{path}: {e}")))?;
        f.set_len(len as u64)
            .map_err(|e| std::io::Error::new(e.kind(), format!("{path}: {e}")))?;
        // SAFETY: a shared mapping of a file of exactly `len` bytes
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                f.as_raw_fd(),
                0,
            )
        };
        let _ = std::fs::remove_file(&path);
        if p == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Mem { p: p.cast(), len })
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn ptr(&self) -> *mut u8 {
        self.p
    }
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping is `len` bytes for this value's life
        unsafe { std::slice::from_raw_parts(self.p, self.len) }
    }
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, and borrowed mutably through `self`
        unsafe { std::slice::from_raw_parts_mut(self.p, self.len) }
    }
    /// mlock(2) the mapping; the error when it cannot be locked.
    pub fn lock(&self) -> std::io::Result<()> {
        // SAFETY: the range is this mapping
        if unsafe { libc::mlock(self.p.cast(), self.len) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Map `b` privately over this memory (`MAP_PRIVATE | MAP_FIXED`, the module documentation):
    /// afterwards every byte equals the base's, and the pages written before are given back. No
    /// vCPU may use the memory meanwhile (the VMM destroys its VM or takes the RAM from it first).
    pub fn map_base(&mut self, b: &Base) -> std::io::Result<()> {
        assert_eq!(b.len, self.len, "a RAM base of another size");
        // SAFETY: replaces exactly this value's own mapping (borrowed mutably here, so nothing
        // holds a reference into the old pages) with a private mapping of a file of `len` bytes
        let p = unsafe {
            libc::mmap(
                self.p.cast(),
                self.len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_FIXED,
                b.file.as_raw_fd(),
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        if p.cast::<u8>() != self.p {
            // MAP_FIXED maps at the address or fails; keep the value's invariant regardless
            return Err(std::io::Error::other("mmap MAP_FIXED moved the mapping"));
        }
        Ok(())
    }
}

/// The host's page size (16 KiB on Apple Silicon).
fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions
    let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if n > 0 {
        n as usize
    } else {
        0x4000
    }
}

/// Whether `p` is all zero (a comparison with zeros, which the C library does a word at a time).
fn is_zero(p: &[u8]) -> bool {
    static ZERO: [u8; 0x4000] = [0; 0x4000];
    p.chunks(ZERO.len()).all(|c| c == &ZERO[..c.len()])
}

/// A snapshot's guest RAM in a file that is never written after it is made (the module
/// documentation). Guest RAM is mapped from it with [`Mem::map_base`].
pub struct Base {
    file: std::fs::File,
    len: usize,
    /// The parts of the RAM that the file holds (offset, length; whole host pages), in order. The
    /// RAM is zero everywhere else.
    extents: Vec<(usize, usize)>,
    /// The file's name in the snapshot folder when other processes can find it (None: unlinked).
    name: Option<String>,
    /// The folder the name is in.
    dir: String,
}

/// The name prefix of a shared base.
const BASE_PREFIX: &str = "ram-v1-";

impl Base {
    /// The base of the bytes `ram` in the folder `dir`. With `share`, a base of the same length
    /// and tree hash `key` that another process made is used when its bytes equal `ram`'s (the
    /// second value is true then), and a new one gets that name; without, the file is unlinked at
    /// once. The file holds only `ram`'s pages that are not zero (the others are holes).
    pub fn make(dir: &str, ram: &[u8], key: u64, share: bool) -> std::io::Result<(Base, bool)> {
        let _ = std::fs::DirBuilder::new().mode_0700().create(dir);
        let name = format!("{BASE_PREFIX}{}-{key:016x}", ram.len());
        if share {
            if let Some(b) = Base::open_same(dir, &name, ram) {
                return Ok((b, true));
            }
        }
        let tmp = format!("{dir}/{name}.{}.tmp", std::process::id());
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| std::io::Error::new(e.kind(), format!("{tmp}: {e}")))?;
        // held before the name exists, so another process never removes it as stale; in a folder
        // whose file system has no locks a base stays this process's own
        let locked = flock(&f, libc::LOCK_SH | libc::LOCK_NB).is_ok();
        let made = (|| {
            f.set_len(ram.len() as u64)?;
            write_nonzero(&f, ram)
        })();
        let extents = match made {
            Ok(x) => x,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(std::io::Error::new(e.kind(), format!("{tmp}: {e}")));
            }
        };
        let mut named = None;
        if share && locked {
            let path = format!("{dir}/{name}");
            // link, not rename: the first complete file of a name stays, a racing twin is ours
            if std::fs::hard_link(&tmp, &path).is_ok() {
                named = Some(name);
            }
        }
        let _ = std::fs::remove_file(&tmp);
        Ok((
            Base {
                file: f,
                len: ram.len(),
                extents,
                name: named,
                dir: dir.to_string(),
            },
            false,
        ))
    }

    /// The file `<dir>/<name>` when it holds exactly `ram`'s bytes, with this process's shared
    /// lock on it.
    fn open_same(dir: &str, name: &str, ram: &[u8]) -> Option<Base> {
        let path = format!("{dir}/{name}");
        let f = std::fs::File::open(&path).ok()?;
        // a process that holds the lock exclusively is removing it
        flock(&f, libc::LOCK_SH | libc::LOCK_NB).ok()?;
        if f.metadata().ok()?.len() != ram.len() as u64 {
            return None;
        }
        let extents = data_extents(&f, ram.len())?;
        let b = Base {
            file: f,
            len: ram.len(),
            extents,
            name: Some(name.to_string()),
            dir: dir.to_string(),
        };
        b.same_as(ram).then_some(b)
    }

    /// Whether the file's bytes equal `ram`'s: each extent read from the file and compared, and
    /// the RAM zero between the extents (the file's holes, which read as zeros).
    fn same_as(&self, ram: &[u8]) -> bool {
        use std::os::unix::fs::FileExt;
        if self.len != ram.len() {
            return false;
        }
        let mut buf = vec![0u8; 1 << 20];
        let mut end = 0;
        for &(at, n) in &self.extents {
            if !is_zero(&ram[end..at]) {
                return false;
            }
            let mut k = at;
            while k < at + n {
                let m = (at + n - k).min(buf.len());
                if self.file.read_exact_at(&mut buf[..m], k as u64).is_err()
                    || buf[..m] != ram[k..k + m]
                {
                    return false;
                }
                k += m;
            }
            end = at + n;
        }
        is_zero(&ram[end..])
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// The parts of the RAM that are not zero (offset, length), which [`Mem::map_base`] maps from
    /// the file: the only bytes of a restored RAM that the host placed.
    pub fn extents(&self) -> &[(usize, usize)] {
        &self.extents
    }
    /// Whether other processes can find this base by its name.
    pub fn is_shared(&self) -> bool {
        self.name.is_some()
    }
}

impl Drop for Base {
    fn drop(&mut self) {
        // the lock goes with the file; then remove the name if nobody else holds it
        let Some(name) = self.name.take() else {
            return;
        };
        let _ = flock(&self.file, libc::LOCK_UN);
        remove_unheld(&format!("{}/{name}", self.dir));
    }
}

fn flock(f: &std::fs::File, op: i32) -> std::io::Result<()> {
    // SAFETY: flock on an open descriptor
    if unsafe { libc::flock(f.as_raw_fd(), op) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Remove the base file `path` when no process holds a lock on it (an exclusive lock that does
/// not wait; a process that opens it meanwhile finds it locked and makes its own).
fn remove_unheld(path: &str) {
    let Ok(f) = std::fs::File::open(path) else {
        return;
    };
    if flock(&f, libc::LOCK_EX | libc::LOCK_NB).is_ok() {
        let _ = std::fs::remove_file(path);
    }
}

/// Remove the shared bases of `dir` that no process holds, and the temporary files of processes
/// that ended before they named theirs (a worker that was killed keeps no lock).
pub fn remove_stale(dir: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if !n.starts_with(BASE_PREFIX) {
            continue;
        }
        let path = format!("{dir}/{n}");
        if let Some(stem) = n.strip_suffix(".tmp") {
            let pid = stem.rsplit('.').next().and_then(|p| p.parse::<i32>().ok());
            let gone = pid.is_some_and(|p| {
                // SAFETY: kill with signal 0 only asks whether the process exists
                let rc = unsafe { libc::kill(p, 0) };
                rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            });
            if gone {
                let _ = std::fs::remove_file(&path);
            }
        } else {
            remove_unheld(&path);
        }
    }
}

/// Write the pages of `ram` that are not zero at their offsets, in runs (a zero page stays a
/// hole), and give the runs (offset, length).
fn write_nonzero(f: &std::fs::File, ram: &[u8]) -> std::io::Result<Vec<(usize, usize)>> {
    use std::os::unix::fs::FileExt;
    let pg = page_size();
    let page = |k: usize| &ram[k..(k + pg).min(ram.len())];
    let mut runs = Vec::new();
    let mut k = 0;
    while k < ram.len() {
        if is_zero(page(k)) {
            k += pg;
            continue;
        }
        let mut stop = (k + pg).min(ram.len());
        while stop < ram.len() && !is_zero(page(stop)) {
            stop = (stop + pg).min(ram.len());
        }
        f.write_all_at(&ram[k..stop], k as u64)?;
        runs.push((k, stop - k));
        k = stop;
    }
    Ok(runs)
}

/// The parts of the file `f` of `len` bytes that hold data (`SEEK_DATA`, `SEEK_HOLE`), widened to
/// whole host pages; a file system without holes gives the whole file. A part that the file
/// system calls data may be zeros: the base's bytes are the file's either way.
fn data_extents(f: &std::fs::File, len: usize) -> Option<Vec<(usize, usize)>> {
    let pg = page_size();
    let fd = f.as_raw_fd();
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut at = 0usize;
    while at < len {
        // SAFETY: lseek on an open descriptor
        let d = unsafe { libc::lseek(fd, at as libc::off_t, libc::SEEK_DATA) };
        if d < 0 {
            // ENXIO: no data from here to the end
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENXIO) {
                break;
            }
            return None;
        }
        // SAFETY: as above
        let h = unsafe { libc::lseek(fd, d, libc::SEEK_HOLE) };
        if h < 0 {
            return None;
        }
        let start = (d as usize / pg) * pg;
        let end = (h as usize).min(len).div_ceil(pg) * pg;
        let end = end.min(len);
        match out.last_mut() {
            Some(last) if last.0 + last.1 >= start => last.1 = end.max(last.0 + last.1) - last.0,
            _ => out.push((start, end - start)),
        }
        if end <= at {
            return None;
        }
        at = end;
    }
    Some(out)
}

trait Mode0700 {
    fn mode_0700(&mut self) -> &mut Self;
}
impl Mode0700 for std::fs::DirBuilder {
    fn mode_0700(&mut self) -> &mut Self {
        use std::os::unix::fs::DirBuilderExt;
        self.mode(0o700)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own in the temporary directory, removed at the end.
    struct Dir(String);
    impl Dir {
        fn new(tag: &str) -> Dir {
            let d =
                std::env::temp_dir().join(format!("hydo-hvm-base-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            Dir(d.display().to_string())
        }
        fn names(&self) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(&self.0)
                .map(|r| {
                    r.flatten()
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            v
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ram(len: usize, seed: u8) -> Vec<u8> {
        let mut b = vec![0u8; len];
        // pages 0 and 3 written, the others zero
        let pg = page_size();
        for (i, x) in b[..pg].iter_mut().enumerate() {
            *x = seed.wrapping_add(i as u8) | 1;
        }
        b[3 * pg + 17] = seed;
        b
    }

    #[test]
    fn a_private_mapping_reads_the_base_and_keeps_its_writes_to_itself() {
        let d = Dir::new("map");
        let len = 8 * page_size();
        let src = ram(len, 5);
        let (b, found) = Base::make(&d.0, &src, 1, true).unwrap();
        assert!(!found && b.is_shared());
        let mut m = Mem::anon(len).unwrap();
        m.bytes_mut()[100] = 9;
        m.map_base(&b).unwrap();
        assert_eq!(m.bytes(), &src[..]);
        let mut n = Mem::anon(len).unwrap();
        n.map_base(&b).unwrap();
        m.bytes_mut()[2 * page_size()] = 0xaa;
        m.bytes_mut()[0] ^= 0xff;
        assert_eq!(n.bytes(), &src[..], "a write is the writer's own");
        assert!(b.same_as(&src), "the file is never written");
        m.map_base(&b).unwrap();
        assert_eq!(
            m.bytes(),
            &src[..],
            "mapping the base again drops the writes"
        );
    }

    #[test]
    fn the_same_bytes_share_one_file_and_other_bytes_do_not() {
        let d = Dir::new("share");
        let len = 8 * page_size();
        let a = ram(len, 1);
        let (b1, f1) = Base::make(&d.0, &a, 7, true).unwrap();
        let (b2, f2) = Base::make(&d.0, &a, 7, true).unwrap();
        assert!(!f1 && f2);
        assert_eq!(d.names(), vec![format!("ram-v1-{len}-0000000000000007")]);
        // other bytes under the same key (a collision of the hash): a file of its own, unnamed
        let c = ram(len, 2);
        let (b3, f3) = Base::make(&d.0, &c, 7, true).unwrap();
        assert!(!f3 && !b3.is_shared());
        let mut m = Mem::anon(len).unwrap();
        m.map_base(&b3).unwrap();
        assert_eq!(m.bytes(), &c[..]);
        m.map_base(&b2).unwrap();
        assert_eq!(m.bytes(), &a[..]);
        // without sharing the file is never named
        let (b4, f4) = Base::make(&d.0, &a, 7, false).unwrap();
        assert!(!f4 && !b4.is_shared());
        drop((b1, b3, b4));
        assert_eq!(d.names().len(), 1, "b2 still holds the name");
        drop(b2);
        assert!(d.names().is_empty(), "the last holder removes the name");
    }

    #[test]
    fn only_what_nobody_holds_is_removed() {
        let d = Dir::new("stale");
        let len = 4 * page_size();
        let (held, _) = Base::make(&d.0, &ram(len, 3), 1, true).unwrap();
        // a base whose process ended (no lock), and a dead process's temporary file
        let left = format!("{}/ram-v1-{len}-0000000000000002", d.0);
        std::fs::write(&left, vec![0u8; len]).unwrap();
        let tmp = format!("{}/ram-v1-{len}-0000000000000003.999999.tmp", d.0);
        std::fs::write(&tmp, b"x").unwrap();
        let other = format!("{}/ctl.1", d.0);
        std::fs::write(&other, b"x").unwrap();
        remove_stale(&d.0);
        assert_eq!(
            d.names(),
            vec![
                "ctl.1".to_string(),
                format!("ram-v1-{len}-0000000000000001")
            ]
        );
        drop(held);
        assert_eq!(d.names(), vec!["ctl.1".to_string()]);
    }

    #[test]
    fn zero_pages_stay_holes() {
        use std::os::unix::fs::MetadataExt;
        let d = Dir::new("holes");
        let len = 1024 * page_size();
        let (b, _) = Base::make(&d.0, &ram(len, 4), 1, true).unwrap();
        let bytes = b.file.metadata().unwrap().blocks() * 512;
        assert!(bytes <= 8 * page_size() as u64, "{bytes} bytes on disk");
    }
}
