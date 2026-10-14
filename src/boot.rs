//! Linux/x86-64 boot support: load a `bzImage`, build the `boot_params` "zero
//! page", the e820 memory map, the kernel command line, an optional initramfs,
//! and a minimal MP table, then hand the vCPU the 64-bit entry point.
//!
//! This is the NON-deterministic bring-up path (M-Linux): it uses KVM's in-kernel
//! irqchip + PIT and the host clock, purely to get a real kernel booting to a
//! serial console. The deterministic engine (verify/snapshot/instruction clock)
//! is untouched and lives in the existing run/verify paths.

use std::io;
use std::path::Path;

use crate::memory::GuestMemory;

// Guest-physical layout for the Linux boot. These sit in low RAM, clear of the
// page tables/GDT the VMM builds (0x1000..0x5000) and clear of the kernel image.
pub const BOOT_PARAMS_ADDR: u64 = 0x1_0000; // zero page
pub const CMDLINE_ADDR: u64 = 0x2_0000; // kernel command line
pub const KERNEL_LOAD_ADDR: u64 = 0x10_0000; // protected-mode kernel @ 1 MiB
/// The 64-bit entry point is the load address + 0x200 (Linux boot protocol).
pub const KERNEL_ENTRY_OFF: u64 = 0x200;

// MP table location: the last KiB of conventional (639 KiB) base memory, one of
// the regions the kernel scans for the "_MP_" floating-pointer signature.
const MPF_ADDR: u64 = 0x9_FC00;
const MPC_ADDR: u64 = 0x9_FC10;

// e820 memory types.
const E820_RAM: u32 = 1;
#[allow(dead_code)]
const E820_RESERVED: u32 = 2;

// Conventional-memory top (EBDA start); RAM below this is usable, the window from
// here to 1 MiB is left reserved (BIOS/video/MP-table area).
const EBDA_START: u64 = 0x9_FC00;
const HIMEM_START: u64 = 0x10_0000;

// setup_header field offsets inside boot_params (hdr begins at 0x1f1).
const OFF_SETUP_SECTS: usize = 0x1f1;
const OFF_TYPE_OF_LOADER: usize = 0x210;
const OFF_LOADFLAGS: usize = 0x211;
const OFF_RAMDISK_IMAGE: usize = 0x218;
const OFF_RAMDISK_SIZE: usize = 0x21c;
const OFF_HEAP_END_PTR: usize = 0x224;
const OFF_CMD_LINE_PTR: usize = 0x228;
const OFF_CMDLINE_SIZE: usize = 0x238;
const OFF_E820_ENTRIES: usize = 0x1e8;
const OFF_E820_TABLE: usize = 0x2d0;

// boot_params fields we copy from the image (the setup header) span this range.
const HDR_COPY_START: usize = 0x1f1;
const HDR_COPY_END: usize = 0x268; // through kernel_info_offset

// loadflags bits.
const LOADED_HIGH: u8 = 1 << 0;
const QUIET_FLAG: u8 = 1 << 5;
const KEEP_SEGMENTS: u8 = 1 << 6;
const CAN_USE_HEAP: u8 = 1 << 7;

/// Where the kernel wants to be entered and where its boot_params live.
pub struct BootInfo {
    pub entry: u64,
    pub boot_params: u64,
}

/// Options controlling a Linux boot.
pub struct LinuxBootOpts<'a> {
    pub kernel: &'a Path,
    pub initramfs: Option<&'a Path>,
    pub cmdline: &'a str,
    pub mem_size: usize,
}

fn rd_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

/// Load a bzImage (and optional initramfs) into guest memory, construct the zero
/// page + e820 + command line + MP table, and return the 64-bit entry point.
pub fn load_linux(mem: &mut GuestMemory, opts: &LinuxBootOpts) -> io::Result<BootInfo> {
    let image = std::fs::read(opts.kernel)?;
    if image.len() < 0x1000 {
        return Err(err("kernel image too small"));
    }
    // Validate the bzImage magic and 64-bit entry capability.
    let boot_flag = rd_u16(&image, 0x1fe);
    let hdr_magic = &image[0x202..0x206];
    if boot_flag != 0xAA55 || hdr_magic != b"HdrS" {
        return Err(err("not a bzImage (bad boot_flag/HdrS magic)"));
    }
    let version = rd_u16(&image, 0x206);
    let xloadflags = rd_u16(&image, 0x236);
    if xloadflags & 0x1 == 0 {
        return Err(err("kernel lacks a 64-bit entry point (XLF_KERNEL_64 clear)"));
    }

    // Setup code size: (setup_sects + 1) * 512, with setup_sects==0 meaning 4.
    let setup_sects = if image[OFF_SETUP_SECTS] == 0 {
        4usize
    } else {
        image[OFF_SETUP_SECTS] as usize
    };
    let setup_size = (setup_sects + 1) * 512;
    if setup_size >= image.len() {
        return Err(err("bzImage smaller than its declared setup area"));
    }

    // The protected-mode kernel (the part we run in 64-bit) follows the setup
    // code in the file and loads at 1 MiB.
    let kernel = &image[setup_size..];
    let kend = KERNEL_LOAD_ADDR as usize + kernel.len();
    if kend >= opts.mem_size {
        return Err(err("guest RAM too small for kernel image"));
    }
    mem.write(KERNEL_LOAD_ADDR, kernel);

    // --- boot_params (zero page) ---
    let mut bp = [0u8; 4096];
    // Copy the setup header from the image into the zero page.
    bp[HDR_COPY_START..HDR_COPY_END].copy_from_slice(&image[HDR_COPY_START..HDR_COPY_END]);

    // Bootloader identity + flags. type_of_loader=0xFF (undefined); keep
    // LOADED_HIGH, allow heap, and CLEAR the QUIET bit so the decompressor prints.
    bp[OFF_TYPE_OF_LOADER] = 0xff;
    let mut lf = bp[OFF_LOADFLAGS];
    lf |= LOADED_HIGH | CAN_USE_HEAP;
    lf &= !(QUIET_FLAG | KEEP_SEGMENTS);
    bp[OFF_LOADFLAGS] = lf;
    // heap_end_ptr is relative to the real-mode setup; unused on the 64-bit path
    // but set to a sane value.
    wr_u16(&mut bp, OFF_HEAP_END_PTR, 0xfe00);

    // Command line.
    let cmd = opts.cmdline.as_bytes();
    let mut cbuf = cmd.to_vec();
    cbuf.push(0);
    mem.write(CMDLINE_ADDR, &cbuf);
    wr_u32(&mut bp, OFF_CMD_LINE_PTR, CMDLINE_ADDR as u32);
    wr_u32(&mut bp, OFF_CMDLINE_SIZE, (cbuf.len() as u32).saturating_sub(1));

    // --- initramfs ---
    if let Some(initrd) = opts.initramfs {
        let blob = std::fs::read(initrd)?;
        // Place it high in low RAM, page-aligned, but within the region the
        // bootloader identity-maps (< 1 GiB) and within actual RAM.
        let cap = opts.mem_size.min(0x4000_0000); // 1 GiB identity-map limit
        if blob.len() + 0x10_0000 >= cap {
            return Err(err("guest RAM too small for initramfs"));
        }
        let addr = ((cap - blob.len()) & !0xfff) as u64;
        mem.write(addr, &blob);
        wr_u32(&mut bp, OFF_RAMDISK_IMAGE, addr as u32);
        wr_u32(&mut bp, OFF_RAMDISK_SIZE, blob.len() as u32);
    }

    // --- e820 map ---
    let mut e820: Vec<(u64, u64, u32)> = Vec::new();
    e820.push((0, EBDA_START, E820_RAM));
    if (opts.mem_size as u64) > HIMEM_START {
        e820.push((HIMEM_START, opts.mem_size as u64 - HIMEM_START, E820_RAM));
    }
    bp[OFF_E820_ENTRIES] = e820.len() as u8;
    for (i, (addr, size, typ)) in e820.iter().enumerate() {
        let base = OFF_E820_TABLE + i * 20;
        wr_u64(&mut bp, base, *addr);
        wr_u64(&mut bp, base + 8, *size);
        wr_u32(&mut bp, base + 16, *typ);
    }

    mem.write(BOOT_PARAMS_ADDR, &bp);

    // --- MP table (so the kernel finds its CPU/LAPIC/IOAPIC without ACPI) ---
    write_mptable(mem);

    let _ = version; // retained for diagnostics/debugging
    Ok(BootInfo {
        entry: KERNEL_LOAD_ADDR + KERNEL_ENTRY_OFF,
        boot_params: BOOT_PARAMS_ADDR,
    })
}

/// Write an Intel MP Floating Pointer + MP Configuration Table describing a
/// single CPU, one ISA bus, one IOAPIC, and the 16 legacy ISA interrupts. This
/// lets a kernel booted with `acpi=off` discover the LAPIC/IOAPIC. Modeled on
/// kvmtool's mptable.
fn write_mptable(mem: &mut GuestMemory) {
    const APIC_DEFAULT_PHYS_BASE: u32 = 0xfee0_0000;
    const IOAPIC_DEFAULT_PHYS_BASE: u32 = 0xfec0_0000;
    const APIC_VERSION: u8 = 0x14;
    const CPU_STEPPING: u32 = 0x600;
    const CPU_FEATURE_APIC: u32 = 0x200;
    const CPU_FEATURE_FPU: u32 = 0x001;

    // MP Configuration Table body is assembled first so we can length/checksum it.
    let mut mpc: Vec<u8> = Vec::new();

    // MPC header (44 bytes); fields patched after we know the length/count.
    // signature "PCMP"
    mpc.extend_from_slice(b"PCMP");
    mpc.extend_from_slice(&0u16.to_le_bytes()); // length (patched)
    mpc.push(4); // spec revision 1.4
    mpc.push(0); // checksum (patched)
    let mut oemid = [b' '; 8];
    oemid[..4].copy_from_slice(b"HYDO");
    mpc.extend_from_slice(&oemid);
    let mut prodid = [b' '; 12];
    prodid[..4].copy_from_slice(b"MPTB");
    mpc.extend_from_slice(&prodid);
    mpc.extend_from_slice(&0u32.to_le_bytes()); // oem table ptr
    mpc.extend_from_slice(&0u16.to_le_bytes()); // oem table size
    mpc.extend_from_slice(&0u16.to_le_bytes()); // entry count (patched)
    mpc.extend_from_slice(&APIC_DEFAULT_PHYS_BASE.to_le_bytes()); // LAPIC address
    mpc.extend_from_slice(&0u16.to_le_bytes()); // extended table length
    mpc.push(0); // extended table checksum
    mpc.push(0); // reserved

    let mut entry_count: u16 = 0;

    // Processor entry (type 0, 20 bytes) for the single boot CPU (LAPIC id 0).
    mpc.push(0); // entry type: processor
    mpc.push(0); // local APIC id
    mpc.push(APIC_VERSION); // local APIC version
    mpc.push(0x03); // cpu flags: enabled | bootstrap processor
    mpc.extend_from_slice(&CPU_STEPPING.to_le_bytes()); // cpu signature
    mpc.extend_from_slice(&(CPU_FEATURE_APIC | CPU_FEATURE_FPU).to_le_bytes()); // features
    mpc.extend_from_slice(&[0u8; 8]); // reserved
    entry_count += 1;

    // Bus entry (type 1, 8 bytes): one ISA bus, id 0.
    mpc.push(1);
    mpc.push(0); // bus id
    mpc.extend_from_slice(b"ISA   "); // bus type string (6 bytes)
    entry_count += 1;

    // IOAPIC entry (type 2, 8 bytes): id 0, enabled.
    let ioapic_id: u8 = 0;
    mpc.push(2);
    mpc.push(ioapic_id);
    mpc.push(APIC_VERSION);
    mpc.push(1); // flags: enabled
    mpc.extend_from_slice(&IOAPIC_DEFAULT_PHYS_BASE.to_le_bytes());
    entry_count += 1;

    // I/O interrupt entries (type 3, 8 bytes): ISA IRQ n -> IOAPIC INTIN n.
    for irq in 0u8..16 {
        mpc.push(3);
        mpc.push(0); // interrupt type: INT
        mpc.extend_from_slice(&0u16.to_le_bytes()); // flags (conforming)
        mpc.push(0); // source bus id (ISA)
        mpc.push(irq); // source bus irq
        mpc.push(ioapic_id); // dest IOAPIC id
        mpc.push(irq); // dest IOAPIC INTIN
        entry_count += 1;
    }

    // Patch length and entry count, then checksum the whole table to 0.
    let len = mpc.len() as u16;
    mpc[4..6].copy_from_slice(&len.to_le_bytes());
    mpc[0x22..0x24].copy_from_slice(&entry_count.to_le_bytes());
    let cksum = checksum(&mpc);
    mpc[7] = cksum;

    mem.write(MPC_ADDR, &mpc);

    // MP Floating Pointer Structure (16 bytes).
    let mut mpf = [0u8; 16];
    mpf[0..4].copy_from_slice(b"_MP_");
    mpf[4..8].copy_from_slice(&(MPC_ADDR as u32).to_le_bytes()); // physical addr of MPC
    mpf[8] = 1; // length in 16-byte units
    mpf[9] = 4; // spec revision 1.4
    mpf[10] = 0; // checksum (patched)
    // bytes 11..16: feature bytes (0 = MP config table present)
    let ck = checksum(&mpf);
    mpf[10] = ck;
    mem.write(MPF_ADDR, &mpf);
}

/// Two's-complement checksum byte so the structure sums to zero (mod 256).
fn checksum(bytes: &[u8]) -> u8 {
    let sum = bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    (0u8).wrapping_sub(sum)
}

fn wr_u16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn wr_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn wr_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}
