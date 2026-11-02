//! Page-aligned host memory: guest RAM (anonymous, locked when asked) and the snapshot buffers
//! (an unlinked file in `HVM_SNAP_DIR`, mapped shared, as the C engine kept them, so a snapshot
//! is paged to its file instead of the swap). The only `unsafe` besides [`crate::hv`].

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
}

impl Drop for Mem {
    fn drop(&mut self) {
        // SAFETY: the mapping made in the constructor, not used afterwards
        unsafe {
            libc::munmap(self.p.cast(), self.len);
        }
    }
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
