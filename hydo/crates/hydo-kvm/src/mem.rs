//! Page-aligned anonymous memory for guest RAM (zeroed, unmapped on drop). With `crate::sys`
//! the only `unsafe` code of the crate.

/// A private anonymous mapping of `len` bytes.
pub struct Mem {
    p: *mut u8,
    len: usize,
}

// SAFETY: the mapping is plain memory owned by this value; it moves with its owner
unsafe impl Send for Mem {}

impl Mem {
    pub fn anon(len: usize) -> std::io::Result<Mem> {
        // SAFETY: a fresh private anonymous mapping, owned by the returned value
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANONYMOUS | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
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
        // SAFETY: as above, borrowed mutably through `self`
        unsafe { std::slice::from_raw_parts_mut(self.p, self.len) }
    }
}

impl Drop for Mem {
    fn drop(&mut self) {
        // SAFETY: the mapping made in the constructor; no vCPU uses it any more (the VMM drops
        // its vCPU first)
        unsafe {
            libc::munmap(self.p.cast(), self.len);
        }
    }
}
