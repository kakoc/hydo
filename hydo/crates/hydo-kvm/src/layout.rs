//! The machine's layout and constants: guest RAM from physical address 0 (at most 3 GiB, below
//! the devices), the virtio-mmio devices at 0xd000_0000 (the Mac engine's devices, moved), the
//! KVM scratch pages Intel needs, the I/O ports of hydo's clock (hv/kvm/linux/hydo-kvm-guest.patch,
//! `asm/hydo.h`), the PIC and the UART, and the IRQ lines.

/// Guest RAM in MiB: the bounds and the default (hvx's: below the 0xC000_0000 hole).
pub const RAM_MIN_MIB: u64 = 256;
pub const RAM_MAX_MIB: u64 = 3072;
pub const RAM_DEFAULT_MIB: u64 = 1024;

/// The virtio-mmio devices (hydo-hvm's `virtio`, with its 0x200-byte stride): device i at
/// `VIRTIO_BASE + i * 0x200`, on IRQ `VIRTIO_IRQ[i]`.
pub const VIRTIO_BASE: u64 = 0xd000_0000;
pub const VIRTIO_IRQ: [u8; 2] = [5, 6];

/// KVM's TSS and identity-map pages (needed on Intel, ignored on AMD), above the devices.
pub const KVM_TSS_ADDR: u64 = 0xfffb_d000;
pub const KVM_IDENTITY_MAP_ADDR: u64 = 0xfffb_c000;

/// hydo's ports (asm/hydo.h).
pub const PORT_CLOCK: u16 = 0x5e0;
pub const PORT_WALL: u16 = 0x5e4;
pub const PORT_TIMER: u16 = 0x5e8;
pub const PORT_CTL: u16 = 0x5ec;
/// The timer's line (the hydo clock event device).
pub const TIMER_IRQ: u8 = 0;

/// The wall clock at boot the guest reads (2026-01-01T00:00:00Z): a constant, not the host's.
pub const WALL_EPOCH: u64 = 1_767_225_600;

/// Nanoseconds of guest time per VM exit (HYDO_KVM_STEP).
pub const STEP_DEFAULT: u64 = 2500;
/// The guest's tick period (CONFIG_HZ_250): tick jitter counts ticks of this length.
pub const TJ_PERIOD: u64 = 4_000_000;
pub const TJ_MAX_EXPLICIT: usize = 32;
/// The console capture.
pub const CONSOLE_MAX: usize = 1 << 22;

/// The kernel command line of an image's boot (hydo-hvx's `ROOTFS_CMDLINE` without what this
/// kernel does not know, plus the virtio-mmio device and no I/O delay port), before the devices
/// and ` quiet loglevel=1`. An unknown word would reach the image's /init as an argument.
pub const CMDLINE: &str =
    "root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 io_delay=none";

/// The command line with the devices: one `virtio_mmio.device=` word per device.
pub fn cmdline(base: &str, ndev: usize) -> String {
    let mut s = base.to_string();
    for i in 0..ndev.min(VIRTIO_IRQ.len()) {
        s.push_str(&format!(
            " virtio_mmio.device=512@{:#x}:{}",
            VIRTIO_BASE + i as u64 * hydo_hvm::virtio::MMIO_SIZE,
            VIRTIO_IRQ[i]
        ));
    }
    s
}

/// Stop reasons of the run loop (hydo-hvm's `STOP_*`).
pub const STOP_MARKER: i32 = 1;
pub const STOP_HALT: i32 = 2;
pub const STOP_IDLE: i32 = 3;
pub const STOP_LIMIT: i32 = 4;
pub const STOP_SPIN: i32 = 5;

// RAM never reaches the devices, and the devices never reach KVM's pages
const _: () = assert!(RAM_MAX_MIB << 20 <= VIRTIO_BASE);
const _: () = assert!(VIRTIO_BASE + 2 * 0x200 <= KVM_IDENTITY_MAP_ADDR);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_are_named_on_the_command_line() {
        assert_eq!(
            cmdline("x", 2),
            "x virtio_mmio.device=512@0xd0000000:5 virtio_mmio.device=512@0xd0000200:6"
        );
    }
}
