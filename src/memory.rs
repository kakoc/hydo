//! Guest physical memory: a single mmap-backed region starting at guest-physical
//! address 0. The backing pages are host anonymous memory handed to KVM via
//! `set_user_memory_region`. We keep the raw pointer stable for the VM lifetime.

use std::io;
use std::ptr;

/// A contiguous region of guest RAM mapped at guest-physical address 0.
pub struct GuestMemory {
    ptr: *mut u8,
    size: usize,
}

// The pointer is owned exclusively by this struct and only handed to KVM (which
// treats it as guest RAM). Access from the VMM is single-threaded per vCPU.
unsafe impl Send for GuestMemory {}

impl GuestMemory {
    /// Allocate `size` bytes of zeroed, page-aligned guest RAM.
    pub fn new(size: usize) -> io::Result<Self> {
        assert!(size % 0x1000 == 0, "guest memory size must be page-aligned");
        // SAFETY: standard anonymous private mapping request.
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(GuestMemory {
            ptr: ptr as *mut u8,
            size,
        })
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Host userspace address of the mapping (for KVM_SET_USER_MEMORY_REGION).
    pub fn host_addr(&self) -> u64 {
        self.ptr as u64
    }

    /// Whole region as a host-side slice.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: ptr/size describe a live mapping owned by self.
        unsafe { std::slice::from_raw_parts(self.ptr, self.size) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: ptr/size describe a live mapping owned by self; &mut self gives
        // exclusive access on the host side.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.size) }
    }

    /// Copy `data` into guest memory at guest-physical address `gpa`.
    pub fn write(&mut self, gpa: u64, data: &[u8]) {
        let start = gpa as usize;
        let end = start
            .checked_add(data.len())
            .expect("write overflows address space");
        assert!(end <= self.size, "write {gpa:#x}+{} exceeds RAM", data.len());
        self.as_mut_slice()[start..end].copy_from_slice(data);
    }
}

impl Drop for GuestMemory {
    fn drop(&mut self) {
        // SAFETY: unmapping our own live mapping exactly once.
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.size);
        }
    }
}
