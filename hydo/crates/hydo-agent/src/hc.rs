//! The host-call port (M138; `hv/hvx/hvx_abi.h`, HVX_VM_HOSTCALL; docs/design/guest-control.md,
//! "Phase 2"): the agent's lines go to the host as records on an I/O port instead of the serial
//! line, and the host's lines come back as bytes the host staged on the same port.
//!
//! The engine offers it with `hydo.hc=1` on the kernel command line; the agent takes it only
//! when the port then answers its magic (a module or an engine without the port leaves the agent
//! on the serial line, as before). With the port:
//!
//! - every line the agent writes (one frame, `HYDO-AG <len> <crc32> <json>\n`) is one record:
//!   4-byte OUTs of its bytes to the data port (the last one padded with zeros), then one OUT of
//!   its length to the control port; the engine's run call returns at that instruction, so the
//!   host sees the line exactly when it was written, and never a line cut in two;
//! - the host's lines are staged by the host; one byte on the serial input wakes the agent (a
//!   doorbell: the bytes on the serial line carry nothing else), which then reads every staged
//!   byte with 4-byte INs (the available count first) and handles the lines as if they came on
//!   the serial line.
//!
//! The port needs I/O permission (`ioperm`, the agent is root); children never get it (the
//! permission is dropped before they exec). Not on aarch64 (no I/O ports; the Mac engine has no
//! port yet): there `Port::open` always says no.

#![cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]

/// The ports and the magic of `hvx_abi.h`.
pub const DATA: u16 = 0x05f0;
pub const CTL: u16 = 0x05f4;
pub const AVAIL: u16 = 0x05f8;
pub const MAGIC: u32 = 0x4348_5948;
/// The kernel command line's offer.
pub const TOKEN: &str = "hydo.hc=1";
/// The span of ports the agent asks permission for.
pub const SPAN: u64 = (AVAIL - DATA) as u64 + 4;

/// The port, once the agent has permission and the module answered.
pub struct Port(());

impl Port {
    /// The port, when the kernel command line offers it and it answers (x86_64 only).
    pub fn open() -> Option<Port> {
        let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
        if !cmdline.split_whitespace().any(|t| t == TOKEN) {
            return None;
        }
        imp::open().then_some(Port(()))
    }

    /// One record: the bytes of one line (at most the module's buffer, 64 KiB).
    pub fn write(&self, b: &[u8]) {
        imp::write(b)
    }

    /// Every byte the host has staged.
    pub fn read_all(&self) -> Vec<u8> {
        imp::read_all()
    }
}

/// In a child before it execs: no port permission (the fork kept the agent's).
pub fn drop_permission() {
    imp::drop_permission()
}

#[cfg(target_arch = "x86_64")]
mod imp {
    use super::*;

    fn ioperm(on: bool) -> bool {
        // SAFETY: a plain system call on the agent's own I/O permission bitmap
        unsafe { libc::syscall(libc::SYS_ioperm, DATA as u64, SPAN, on as u64) == 0 }
    }

    fn outl(port: u16, v: u32) {
        // SAFETY: the agent has permission for these ports (open() checked it); an OUT to them
        // only reaches the module's port model
        unsafe {
            std::arch::asm!("out dx, eax", in("dx") port, in("eax") v,
                options(nomem, nostack, preserves_flags));
        }
    }

    fn inl(port: u16) -> u32 {
        let v: u32;
        // SAFETY: as outl
        unsafe {
            std::arch::asm!("in eax, dx", out("eax") v, in("dx") port,
                options(nomem, nostack, preserves_flags));
        }
        v
    }

    pub fn open() -> bool {
        if !ioperm(true) {
            return false;
        }
        if inl(CTL) == MAGIC {
            return true;
        }
        ioperm(false);
        false
    }

    pub fn write(b: &[u8]) {
        for c in b.chunks(4) {
            let mut w = [0u8; 4];
            w[..c.len()].copy_from_slice(c);
            outl(DATA, u32::from_le_bytes(w));
        }
        outl(CTL, b.len() as u32);
    }

    pub fn read_all() -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let n = inl(AVAIL) as usize;
            if n == 0 {
                return out;
            }
            let mut left = n;
            while left > 0 {
                let w = inl(DATA).to_le_bytes();
                let k = left.min(4);
                out.extend_from_slice(&w[..k]);
                left -= k;
            }
        }
    }

    pub fn drop_permission() {
        ioperm(false);
    }
}

#[cfg(not(target_arch = "x86_64"))]
mod imp {
    pub fn open() -> bool {
        false
    }
    pub fn write(_: &[u8]) {}
    pub fn read_all() -> Vec<u8> {
        Vec::new()
    }
    pub fn drop_permission() {}
}
