//! One VM on `/dev/hvx` (Linux): the device file, guest RAM and the disk window mapped into this
//! process, typed ioctls, and the console as `hvx_stack_verify.c`'s `struct boot` keeps it.

use crate::abi::*;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// hvx_stack_verify.c's `UBUF_CAP`: the reconstructed console is at most 1 MiB.
pub const UBUF_CAP: usize = 1 << 20;

pub struct Vm {
    fd: OwnedFd,
    mem: *mut u8,
    mem_size: usize,
    disk: *mut u8,
    disk_len: usize,
    /// M138: the VM has the host-call port (made with HVX_VM_HOSTCALL by a module that has it).
    pub hc: bool,
    /// M138: the VM keeps the dirty-page log (HVX_VM_DIRTY_LOG by a module that has it).
    pub dl: bool,
}

// SAFETY: the mappings belong to this value only; the module serialises ioctls per VM.
unsafe impl Send for Vm {}

fn ret(r: libc::c_int) -> io::Result<()> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl Vm {
    /// Open the device (a new VM per open file).
    pub fn open(dev: &str) -> io::Result<Vm> {
        let c = std::ffi::CString::new(dev).map_err(|_| io::Error::other("device path"))?;
        // SAFETY: a plain open(2)
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Vm {
            // SAFETY: fd is a fresh descriptor we own
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            mem: std::ptr::null_mut(),
            mem_size: 0,
            disk: std::ptr::null_mut(),
            disk_len: 0,
            hc: false,
            dl: false,
        })
    }

    /// A raw ioctl with a pointer argument.
    ///
    /// # Safety
    /// `arg` must point to the struct the request number encodes (or a larger buffer the
    /// module's handler copies, as for HVX_GET_CONSOLE).
    pub unsafe fn ioctl_ptr<T>(&self, req: u64, arg: *mut T) -> io::Result<()> {
        loop {
            // SAFETY: by the caller
            let r = unsafe { libc::ioctl(self.fd.as_raw_fd(), req as _, arg) };
            if r < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return ret(r);
        }
    }

    /// ioctl with a typed in/out struct (EINTR retried).
    pub fn io<T: Pod>(&self, req: u64, arg: &mut T) -> io::Result<()> {
        // SAFETY: req is one of the ABI's numbers for T (the callers use the typed wrappers)
        unsafe { self.ioctl_ptr(req, arg as *mut T) }
    }

    /// ioctl that must not be retried on EINTR (the run calls: the caller decides).
    pub fn io_once<T: Pod>(&self, req: u64, arg: &mut T) -> io::Result<()> {
        // SAFETY: as io()
        ret(unsafe { libc::ioctl(self.fd.as_raw_fd(), req as _, arg as *mut T) })
    }

    pub fn create(&mut self, cfg: &hvx_config) -> io::Result<()> {
        let mut c = *cfg;
        self.io(HVX_CREATE_VM, &mut c)
    }

    /// mmap guest RAM (offset 0).
    pub fn map_ram(&mut self, size: usize) -> io::Result<()> {
        self.mem = self.map(size, 0)?;
        self.mem_size = size;
        Ok(())
    }

    /// mmap the disk window.
    pub fn map_disk(&mut self, len: usize) -> io::Result<()> {
        self.disk = self.map(len, HVX_DISK_MMAP_PGOFF * 4096)?;
        self.disk_len = len;
        Ok(())
    }

    /// Map every page of guest RAM and of `[off, off + len)` of the disk window into this process
    /// now (MADV_POPULATE_WRITE, Linux 5.14), so that a later restore and fingerprint take no
    /// page faults. Only the page tables change; the pages and their bytes are the module's.
    pub fn populate(&self, disk_off: usize, disk_len: usize) {
        const MADV_POPULATE_WRITE: libc::c_int = 23;
        // SAFETY: madvise on our own mappings; failure (an older kernel) is harmless
        unsafe {
            if !self.mem.is_null() {
                libc::madvise(self.mem as *mut _, self.mem_size, MADV_POPULATE_WRITE);
            }
            if !self.disk.is_null() && disk_off + disk_len <= self.disk_len {
                let page = disk_off & !4095;
                let len = disk_off + disk_len - page;
                libc::madvise(self.disk.add(page) as *mut _, len, MADV_POPULATE_WRITE);
            }
        }
    }

    fn map(&self, len: usize, off: u64) -> io::Result<*mut u8> {
        // SAFETY: a shared mapping of the device file
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.fd.as_raw_fd(),
                off as libc::off_t,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(p as *mut u8)
    }

    pub fn mem(&self) -> &[u8] {
        if self.mem.is_null() {
            return &[];
        }
        // SAFETY: mapped with mem_size bytes for the life of self
        unsafe { std::slice::from_raw_parts(self.mem, self.mem_size) }
    }

    pub fn mem_mut(&mut self) -> &mut [u8] {
        if self.mem.is_null() {
            return &mut [];
        }
        // SAFETY: as mem(); &mut self gives exclusive access from this process
        unsafe { std::slice::from_raw_parts_mut(self.mem, self.mem_size) }
    }

    pub fn disk(&self) -> &[u8] {
        if self.disk.is_null() {
            return &[];
        }
        // SAFETY: mapped with disk_len bytes for the life of self
        unsafe { std::slice::from_raw_parts(self.disk, self.disk_len) }
    }

    pub fn disk_mut(&mut self) -> &mut [u8] {
        if self.disk.is_null() {
            return &mut [];
        }
        // SAFETY: as disk()
        unsafe { std::slice::from_raw_parts_mut(self.disk, self.disk_len) }
    }

    pub fn get_snapshot(&self, s: &mut hvx_snapshot) -> io::Result<()> {
        // SAFETY: HVX_GET_SNAPSHOT takes a struct hvx_snapshot
        unsafe { self.ioctl_ptr(HVX_GET_SNAPSHOT, s as *mut hvx_snapshot) }
    }

    pub fn set_snapshot(&self, s: &hvx_snapshot) -> io::Result<()> {
        // SAFETY: the module only reads the struct
        unsafe {
            self.ioctl_ptr(
                HVX_SET_SNAPSHOT,
                s as *const hvx_snapshot as *mut hvx_snapshot,
            )
        }
    }

    pub fn set_vcpu(&self, i: u32) -> io::Result<()> {
        let mut i = i;
        // SAFETY: HVX_SET_VCPU takes a __u32
        unsafe { self.ioctl_ptr(HVX_SET_VCPU, &mut i as *mut u32) }
    }

    /// HVX_GET_CONSOLE into `buf` (retried on EINTR only; the caller retries on ENOMEM).
    pub fn get_console(&self, buf: &mut hvx_console) -> io::Result<()> {
        // SAFETY: the handler writes at most sizeof(struct hvx_console) bytes
        unsafe { self.ioctl_ptr(HVX_GET_CONSOLE, buf as *mut hvx_console) }
    }

    pub fn reset_ftstat(&self) -> io::Result<()> {
        // SAFETY: an _IO request without an argument
        ret(unsafe { libc::ioctl(self.fd.as_raw_fd(), HVX_RESET_FTSTAT as _, 0) })
    }

    /// HVX_RESET_VM (M138): the VM as HVX_CREATE_VM left it, RAM and disk contents aside.
    pub fn reset(&self) -> io::Result<()> {
        // SAFETY: an _IO request without an argument
        ret(unsafe { libc::ioctl(self.fd.as_raw_fd(), HVX_RESET_VM as _, 0) })
    }

    /// HVX_GET_DIRTY (M138): the RAM and disk dirty bitmaps (one bit per 4 KiB page), cleared
    /// after with `clear`. `ram`/`disk` must have room for every page (or be empty to skip).
    pub fn get_dirty(
        &self,
        ram: &mut [u64],
        disk: &mut [u64],
        clear: bool,
    ) -> io::Result<hvx_dirty> {
        let mut d = hvx_dirty {
            ram_addr: if ram.is_empty() {
                0
            } else {
                ram.as_mut_ptr() as u64
            },
            ram_bytes: (ram.len() * 8) as u64,
            disk_addr: if disk.is_empty() {
                0
            } else {
                disk.as_mut_ptr() as u64
            },
            disk_bytes: (disk.len() * 8) as u64,
            flags: if clear { HVX_DIRTY_CLEAR } else { 0 },
            ..Default::default()
        };
        self.io(HVX_GET_DIRTY, &mut d)?;
        Ok(d)
    }

    /// HVX_HC_GET (M138): append the committed host-call records to `out`.
    pub fn hc_get(&self, out: &mut Vec<u8>) -> io::Result<hvx_hc_io> {
        let mut buf = vec![0u8; HVX_HC_SZ];
        let mut io = hvx_hc_io {
            addr: buf.as_mut_ptr() as u64,
            len: buf.len() as u32,
            ..Default::default()
        };
        self.io(HVX_HC_GET, &mut io)?;
        out.extend_from_slice(&buf[..io.n as usize]);
        Ok(io)
    }

    /// HVX_HC_PUT (M138): stage bytes for the guest.
    pub fn hc_put(&self, p: &[u8]) -> io::Result<()> {
        let mut io = hvx_hc_io {
            addr: p.as_ptr() as u64,
            len: p.len() as u32,
            ..Default::default()
        };
        self.io(HVX_HC_PUT, &mut io)
    }

    /// HVX_HC_GET_STATE (M138): the port's buffers (output, then the unread staged bytes).
    pub fn hc_state(&self) -> io::Result<(hvx_hc_state, Vec<u8>)> {
        let mut buf = vec![0u8; 2 * HVX_HC_SZ];
        let mut st = hvx_hc_state {
            addr: buf.as_mut_ptr() as u64,
            cap: buf.len() as u32,
            ..Default::default()
        };
        self.io(HVX_HC_GET_STATE, &mut st)?;
        buf.truncate((st.out_len + st.in_len) as usize);
        Ok((st, buf))
    }

    /// HVX_HC_SET_STATE (M138).
    pub fn hc_set_state(&self, st: &hvx_hc_state, bytes: &[u8]) -> io::Result<()> {
        let mut s = *st;
        s.addr = bytes.as_ptr() as u64;
        s.cap = bytes.len() as u32;
        self.io(HVX_HC_SET_STATE, &mut s)
    }
}

/// A file mapped read-only (the kernel, the initramfs, the base disk image).
pub struct Mapped {
    p: *const u8,
    len: usize,
}

// SAFETY: a read-only private mapping, never written
unsafe impl Send for Mapped {}
unsafe impl Sync for Mapped {}

impl Mapped {
    pub fn open(path: &std::path::Path) -> io::Result<Mapped> {
        let f = std::fs::File::open(path)?;
        let len = f.metadata()?.len() as usize;
        if len == 0 {
            return Ok(Mapped {
                p: std::ptr::null(),
                len: 0,
            });
        }
        // SAFETY: a private read-only mapping of a regular file
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                f.as_raw_fd(),
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Mapped {
            p: p as *const u8,
            len,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        if self.p.is_null() {
            return &[];
        }
        // SAFETY: mapped with len bytes for the life of self
        unsafe { std::slice::from_raw_parts(self.p, self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        if !self.p.is_null() {
            // SAFETY: our own mapping
            unsafe { libc::munmap(self.p as *mut _, self.len) };
        }
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        // SAFETY: unmapping our own mappings; the fd closes after (OwnedFd)
        unsafe {
            if !self.disk.is_null() {
                libc::munmap(self.disk as *mut _, self.disk_len);
            }
            if !self.mem.is_null() {
                libc::munmap(self.mem as *mut _, self.mem_size);
            }
        }
    }
}
