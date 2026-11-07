//! Guest RAM and the images snapshots share. With `crate::sys` the only `unsafe` code of the
//! crate.
//!
//! Guest RAM ([`Mem`]) starts as a private anonymous mapping (zeroed). A [`RamBase`] is a full
//! RAM image in a sealed memory file (Linux `memfd`; elsewhere, for the tests, a copy in memory):
//! guest RAM can be mapped from it privately ([`Mem::map_base`]), so that its pages read the base
//! until something writes them (the guest through KVM, or the engine's devices) and only the
//! written pages take memory of their own (the kernel copies a page at its first write). Which
//! pages those are is read from the kernel's page map ([`Mem::private_pages`]: a page present
//! and not the file's, or swapped out), so a snapshot copies only them, and restoring a snapshot
//! maps its base again (dropping every page of the run at once) and writes its pages back. The
//! base is never written after it is made; a page the page map does not list holds exactly the
//! base's bytes. Where the page map cannot be read, every page is compared with the base instead
//! (the same answer, slower).

use std::sync::Arc;

/// The page size the images are kept in.
pub const PAGE: usize = 4096;

/// A private mapping of `len` bytes: anonymous, or of a [`RamBase`].
pub struct Mem {
    p: *mut u8,
    len: usize,
}

// SAFETY: the mapping is plain memory owned by this value; it moves with its owner
unsafe impl Send for Mem {}

fn mmap(len: usize, prot: i32, flags: i32, fd: i32, at: *mut u8) -> std::io::Result<*mut u8> {
    // SAFETY: a fresh mapping (or one replacing `at..at+len`, owned by the caller, with
    // MAP_FIXED); the caller owns the result
    let p = unsafe { libc::mmap(at.cast(), len, prot, flags, fd, 0) };
    if p == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    Ok(p.cast())
}

impl Mem {
    pub fn anon(len: usize) -> std::io::Result<Mem> {
        let p = mmap(
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_ANONYMOUS | libc::MAP_PRIVATE,
            -1,
            std::ptr::null_mut(),
        )?;
        Ok(Mem { p, len })
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
    pub fn page(&self, k: usize) -> &[u8] {
        &self.bytes()[k * PAGE..(k + 1) * PAGE]
    }

    /// Map `b` privately over this memory: every page reads the base until it is written. No
    /// vCPU may use the memory meanwhile (the VMM drops its vCPU first).
    pub fn map_base(&mut self, b: &RamBase) -> std::io::Result<()> {
        assert_eq!(b.len, self.len, "a RAM base of another size");
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            mmap(
                self.len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_FIXED | libc::MAP_NORESERVE,
                b.fd.as_raw_fd(),
                self.p,
            )?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            let src = b.view.bytes();
            self.bytes_mut().copy_from_slice(src);
        }
        Ok(())
    }

    /// The pages that may differ from `base` (the base this memory was mapped from last; None:
    /// an anonymous mapping, whose untouched pages are zero), in ascending order. A page not
    /// listed holds exactly the base's bytes (or zeros).
    pub fn private_pages(&self, base: Option<&RamBase>) -> Vec<u32> {
        #[cfg(target_os = "linux")]
        if let Some(v) = self.pagemap_private() {
            return v;
        }
        let n = self.len / PAGE;
        (0..n)
            .filter(|&k| match base {
                Some(b) => self.page(k) != b.page(k),
                None => self.page(k).iter().any(|&x| x != 0),
            })
            .map(|k| k as u32)
            .collect()
    }

    /// The page map's answer: present and not the file's page, or swapped out.
    #[cfg(target_os = "linux")]
    fn pagemap_private(&self) -> Option<Vec<u32>> {
        use std::io::{Read, Seek, SeekFrom};
        const PRESENT: u64 = 1 << 63;
        const SWAP: u64 = 1 << 62;
        const FILE: u64 = 1 << 61;
        let mut f = std::fs::File::open("/proc/self/pagemap").ok()?;
        let n = self.len / PAGE;
        f.seek(SeekFrom::Start((self.p as u64 / PAGE as u64) * 8))
            .ok()?;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 8 * 8192];
        let mut k = 0usize;
        while k < n {
            let m = (n - k).min(8192);
            f.read_exact(&mut buf[..8 * m]).ok()?;
            for i in 0..m {
                let e = u64::from_le_bytes(buf[8 * i..8 * i + 8].try_into().unwrap());
                if (e & PRESENT != 0 && e & FILE == 0) || e & SWAP != 0 {
                    out.push((k + i) as u32);
                }
            }
            k += m;
        }
        Some(out)
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

/// A full RAM image that guest RAM and snapshots share (see the module documentation). Its
/// zero pages are holes of the file and are hashed without being read ([`RamBase::nonzero`]).
pub struct RamBase {
    #[cfg(target_os = "linux")]
    fd: std::os::fd::OwnedFd,
    /// the image, read-only (Linux: a shared mapping of the file; elsewhere a copy)
    view: Mem,
    len: usize,
    /// a bit per page: the page holds a nonzero byte
    nonzero: Vec<u64>,
}

// SAFETY: the view is never written after the base is made
unsafe impl Sync for RamBase {}

impl RamBase {
    /// A base made from `len` bytes of memory: `fill(k, put)` calls `put(bytes)` with page k's
    /// bytes, or not at all for a zero page.
    pub fn build(
        len: usize,
        fill: impl Fn(usize, &mut dyn FnMut(&[u8])),
    ) -> std::io::Result<RamBase> {
        assert!(len.is_multiple_of(PAGE));
        let n = len / PAGE;
        let mut nonzero = vec![0u64; n.div_ceil(64)];
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
            use std::os::unix::fs::FileExt;
            // SAFETY: a name and flags; the returned descriptor is owned below
            let fd = unsafe {
                libc::memfd_create(
                    c"hydo-kvm-ram".as_ptr(),
                    libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: the descriptor just returned, owned from here on
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let file = std::fs::File::from(fd);
            file.set_len(len as u64)?;
            let mut err = Ok(());
            for k in 0..n {
                fill(k, &mut |b: &[u8]| {
                    if b.iter().any(|&x| x != 0) {
                        nonzero[k / 64] |= 1 << (k % 64);
                        if err.is_ok() {
                            err = file.write_all_at(b, (k * PAGE) as u64);
                        }
                    }
                });
            }
            err?;
            // never written again: the snapshots' pages rely on it (F_SEAL_FUTURE_WRITE, not
            // F_SEAL_WRITE: before Linux 6.7 the latter refuses even a read-only shared mapping)
            // SAFETY: fcntl on the descriptor we own
            unsafe {
                libc::fcntl(
                    file.as_raw_fd(),
                    libc::F_ADD_SEALS,
                    libc::F_SEAL_SHRINK
                        | libc::F_SEAL_GROW
                        | libc::F_SEAL_FUTURE_WRITE
                        | libc::F_SEAL_SEAL,
                );
            }
            let p = mmap(
                len,
                libc::PROT_READ,
                libc::MAP_SHARED | libc::MAP_NORESERVE,
                file.as_raw_fd(),
                std::ptr::null_mut(),
            )?;
            Ok(RamBase {
                fd: file.into(),
                view: Mem { p, len },
                len,
                nonzero,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut view = Mem::anon(len)?;
            for k in 0..n {
                let dst = &mut view.bytes_mut()[k * PAGE..(k + 1) * PAGE];
                fill(k, &mut |b: &[u8]| {
                    if b.iter().any(|&x| x != 0) {
                        nonzero[k / 64] |= 1 << (k % 64);
                        dst.copy_from_slice(b);
                    }
                });
            }
            Ok(RamBase { view, len, nonzero })
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Whether page k holds a nonzero byte (a zero page is a hole of the file: reading it
    /// through a mapping would give it memory, so the hash and the snapshots do not).
    pub fn nonzero(&self, k: usize) -> bool {
        self.nonzero[k / 64] & (1 << (k % 64)) != 0
    }
    /// Page k's bytes (read through the read-only view).
    pub fn page(&self, k: usize) -> &[u8] {
        self.view.page(k)
    }
    /// The number of nonzero pages (what the base costs in memory).
    pub fn pages(&self) -> usize {
        self.nonzero.iter().map(|w| w.count_ones() as usize).sum()
    }
}

/// A RAM image: a base and the pages that differ from it.
#[derive(Clone, Default)]
pub struct RamImage {
    pub base: Option<Arc<RamBase>>,
    /// ascending page numbers, and their bytes (PAGE each, in that order)
    pub pages: Vec<u32>,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for RamImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RamImage(base {} pages, {} pages of its own)",
            self.base.as_ref().map(|b| b.pages()).unwrap_or(0),
            self.pages.len()
        )
    }
}

impl RamImage {
    /// The bytes of page k of the image (None: a zero page).
    pub fn page(&self, k: usize) -> Option<&[u8]> {
        if let Ok(i) = self.pages.binary_search(&(k as u32)) {
            return Some(&self.data[i * PAGE..(i + 1) * PAGE]);
        }
        let b = self.base.as_ref()?;
        b.nonzero(k).then(|| b.page(k))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_is_shared_and_only_written_pages_are_ram_s_own() {
        let n = 64 * PAGE;
        let mut boot = Mem::anon(n).unwrap();
        for k in [1usize, 2, 9, 40] {
            boot.bytes_mut()[k * PAGE..(k + 1) * PAGE].fill(k as u8);
        }
        // an anonymous mapping: its touched pages are its own (the page map, or a nonzero test)
        let own = boot.private_pages(None);
        for k in [1u32, 2, 9, 40] {
            assert!(own.contains(&k), "{own:?}");
        }
        let base = RamBase::build(n, |k, put| {
            if own.binary_search(&(k as u32)).is_ok() {
                put(boot.page(k));
            }
        })
        .unwrap();
        assert_eq!(base.pages(), 4);
        assert!(base.nonzero(9) && !base.nonzero(10));
        let mut ram = Mem::anon(n).unwrap();
        ram.map_base(&base).unwrap();
        assert_eq!(ram.bytes(), boot.bytes());
        assert!(ram.private_pages(Some(&base)).is_empty());
        // a write makes a page RAM's own; the base is unchanged
        ram.bytes_mut()[5 * PAGE + 3] = 7;
        ram.bytes_mut()[9 * PAGE] = 0;
        assert_eq!(ram.private_pages(Some(&base)), vec![5, 9]);
        assert_eq!(base.page(9)[0], 9);
        assert_eq!(base.page(5)[3], 0);
        // an image of RAM: the base and its own two pages
        let own = ram.private_pages(Some(&base));
        let mut data = Vec::new();
        for &k in &own {
            data.extend_from_slice(ram.page(k as usize));
        }
        let base = Arc::new(base);
        let img = RamImage {
            base: Some(base.clone()),
            pages: own,
            data,
        };
        assert_eq!(img.page(5).unwrap()[3], 7);
        assert_eq!(img.page(9).unwrap()[0], 0);
        assert_eq!(img.page(40).unwrap()[0], 40);
        assert!(img.page(41).is_none());
        // mapped again: every page of RAM's own is dropped at once
        ram.map_base(&base).unwrap();
        assert_eq!(ram.bytes(), boot.bytes());
        assert!(ram.private_pages(Some(&base)).is_empty());
    }
}
