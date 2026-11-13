//! The machine's layout and constants: guest RAM from physical address 0 (the low block, at most
//! 3 GiB, below the devices) and, for a guest of more than 3 GiB, the rest from 4 GiB (the high
//! block; [`ram_blocks`], [`ram_offset`]), the virtio-mmio devices at 0xd000_0000 (the Mac engine's devices, moved), the
//! KVM scratch pages Intel needs, the I/O ports of hydo's clock (hv/kvm/linux/hydo-kvm-guest.patch,
//! `asm/hydo.h`), the PIC and the UART, and the IRQ lines.

/// Guest RAM in MiB: the bounds and the default.
pub const RAM_MIN_MIB: u64 = 256;
pub const RAM_MAX_MIB: u64 = 8192;
pub const RAM_DEFAULT_MIB: u64 = 1024;

/// Guest RAM is one host buffer. Its first [`RAM_LOW_TOP`] bytes (all of it for a guest of 3 GiB
/// or less, whose layout is the one it always was) are at guest-physical 0, below the devices;
/// the rest is at guest-physical [`RAM_HIGH_BASE`] (hvx's layout: the hole 3..4 GiB holds the
/// devices, KVM's pages and, on hvx, the PCI and APIC windows).
pub const RAM_LOW_TOP: u64 = 0xC000_0000;
pub const RAM_HIGH_BASE: u64 = 0x1_0000_0000;

/// The blocks of `len` bytes of guest RAM: (guest-physical address, offset in the buffer, size).
/// One block for a guest of 3 GiB or less, two above.
pub fn ram_blocks(len: u64) -> Vec<(u64, u64, u64)> {
    if len <= RAM_LOW_TOP {
        vec![(0, 0, len)]
    } else {
        vec![
            (0, 0, RAM_LOW_TOP),
            (RAM_HIGH_BASE, RAM_LOW_TOP, len - RAM_LOW_TOP),
        ]
    }
}

/// The offset in the RAM buffer (of `len` bytes) of the `n` guest-physical bytes at `gpa`, or
/// None when they are not all inside one block of RAM. Every translation of a guest-physical
/// address to the buffer goes through this.
pub fn ram_offset(len: u64, gpa: u64, n: u64) -> Option<usize> {
    for (at, off, size) in ram_blocks(len) {
        if gpa >= at && gpa - at <= size && n <= size - (gpa - at) {
            return Some((off + (gpa - at)) as usize);
        }
    }
    None
}

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
/// Several vCPUs: the SMP guest kernel's `cpu_relax()` writes it; the engine ends the vCPU's turn
/// (docs/design/kvm-vcpus.md, hv/kvm/linux/hydo-kvm-smp.patch).
pub const PORT_RELAX: u16 = 0x5f0;
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

// the low block never reaches the devices, the devices never reach KVM's pages, and those stay
// below the high block
const _: () = assert!(RAM_LOW_TOP <= VIRTIO_BASE);
const _: () = assert!(KVM_TSS_ADDR + 3 * 4096 <= RAM_HIGH_BASE);
const _: () = assert!((RAM_MAX_MIB << 20) - RAM_LOW_TOP <= (1 << 36) - RAM_HIGH_BASE);
const _: () = assert!(VIRTIO_BASE + 2 * 0x200 <= KVM_IDENTITY_MAP_ADDR);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_up_to_3_gib_is_one_block_from_0() {
        for mib in [256u64, 1024, 3072] {
            let len = mib << 20;
            assert_eq!(ram_blocks(len), vec![(0, 0, len)]);
            assert_eq!(ram_offset(len, 0, len), Some(0));
            assert_eq!(ram_offset(len, len - 8, 8), Some(len as usize - 8));
            assert_eq!(ram_offset(len, len - 4, 8), None);
            assert_eq!(ram_offset(len, len, 1), None);
            assert_eq!(ram_offset(len, RAM_HIGH_BASE, 1), None);
        }
    }

    #[test]
    fn ram_above_3_gib_continues_at_4_gib() {
        let len = 8192u64 << 20;
        let hi = len - RAM_LOW_TOP;
        assert_eq!(
            ram_blocks(len),
            vec![(0, 0, RAM_LOW_TOP), (RAM_HIGH_BASE, RAM_LOW_TOP, hi)]
        );
        assert_eq!(ram_offset(len, 0x1000, 8), Some(0x1000));
        assert_eq!(
            ram_offset(len, RAM_LOW_TOP - 8, 8),
            Some(RAM_LOW_TOP as usize - 8)
        );
        // nothing in the hole, and no range across it
        assert_eq!(ram_offset(len, RAM_LOW_TOP, 1), None);
        assert_eq!(ram_offset(len, VIRTIO_BASE, 4), None);
        assert_eq!(ram_offset(len, RAM_LOW_TOP - 4, 8), None);
        assert_eq!(ram_offset(len, RAM_HIGH_BASE - 1, 2), None);
        assert_eq!(
            ram_offset(len, RAM_HIGH_BASE, 8),
            Some(RAM_LOW_TOP as usize)
        );
        let last = RAM_HIGH_BASE + hi - 4096;
        assert_eq!(ram_offset(len, last, 4096), Some(len as usize - 4096));
        assert_eq!(ram_offset(len, last, 4097), None);
        assert_eq!(ram_offset(len, RAM_HIGH_BASE + hi, 1), None);
    }

    #[test]
    fn devices_are_named_on_the_command_line() {
        assert_eq!(
            cmdline("x", 2),
            "x virtio_mmio.device=512@0xd0000000:5 virtio_mmio.device=512@0xd0000200:6"
        );
    }
}
