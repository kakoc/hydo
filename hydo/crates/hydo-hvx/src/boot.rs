//! The Linux boot recipe of `hv/hvx/hvx_boot.h`, byte for byte: long-mode page tables and GDT,
//! the bzImage and initramfs in guest RAM, the zero page with its e820 map, the command line, the
//! MP table, and the initial system registers. Pure functions over a RAM buffer, so the result
//! is tested without a VM (`tests` below and `tests/boot.rs`).

use crate::abi::{hvx_segment, hvx_sregs};

pub const PML4_ADDR: u64 = 0x1000;
pub const PDPT_ADDR: u64 = 0x2000;
pub const PD_ADDR: u64 = 0x3000;
pub const GDT_ADDR: u64 = 0x4000;
pub const STACK_TOP: u64 = 0x8000;
const PTE_PRESENT: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
const PTE_PS: u64 = 1 << 7;
const CR0_PE: u64 = 1 << 0;
const CR0_NE: u64 = 1 << 5;
const CR0_PG: u64 = 1 << 31;
const CR4_PAE: u64 = 1 << 5;
const EFER_LME: u64 = 1 << 8;
const EFER_LMA: u64 = 1 << 10;
pub const BOOT_PARAMS_ADDR: u64 = 0x10000;
pub const CMDLINE_ADDR: u64 = 0x20000;
pub const KERNEL_LOAD_ADDR: u64 = 0x100000;
pub const KERNEL_ENTRY_OFF: u64 = 0x200;
const MPF_ADDR: u64 = 0x9FC00;
const MPC_ADDR: u64 = 0x9FC10;
const EBDA_START: u64 = 0x9FC00;
const HIMEM_START: u64 = 0x100000;
const E820_RAM: u32 = 1;
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
const HDR_COPY_START: usize = 0x1f1;
const HDR_COPY_END: usize = 0x268;
const LOADED_HIGH: u8 = 1 << 0;
const QUIET_FLAG: u8 = 1 << 5;
const KEEP_SEGMENTS: u8 = 1 << 6;
const CAN_USE_HEAP: u8 = 1 << 7;
const IDENTITY_MAP_CAP: u64 = 0x4000_0000;

/// hvx_stack_verify.c's `ROOTFS_CMDLINE`.
pub const ROOTFS_CMDLINE: &str =
    "root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys";

fn wr16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn wr32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn wr64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
fn rd16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn guest_write(mem: &mut [u8], gpa: u64, src: &[u8]) -> Result<(), String> {
    if gpa + src.len() as u64 > mem.len() as u64 {
        return Err(format!(
            "hvx_boot: guest_write {:#x} bytes @ GPA {:#x} exceeds guest RAM ({:#x})",
            src.len(),
            gpa,
            mem.len()
        ));
    }
    mem[gpa as usize..gpa as usize + src.len()].copy_from_slice(src);
    Ok(())
}

fn checksum(b: &[u8]) -> u8 {
    0u8.wrapping_sub(b.iter().fold(0u8, |s, &x| s.wrapping_add(x)))
}

/// Every byte range of guest RAM the boot recipe writes (start, length), for a caller that
/// must know which pages a fresh VM's setup touched.
pub type Written = Vec<(u64, u64)>;

/// `build_page_tables_and_gdt`.
pub fn build_page_tables_and_gdt(mem: &mut [u8], w: &mut Written) {
    wr64(mem, PML4_ADDR as usize, PDPT_ADDR | PTE_PRESENT | PTE_RW);
    wr64(mem, PDPT_ADDR as usize, PD_ADDR | PTE_PRESENT | PTE_RW);
    for i in 0..512u64 {
        wr64(
            mem,
            (PD_ADDR + i * 8) as usize,
            (i * 0x200000) | PTE_PRESENT | PTE_RW | PTE_PS,
        );
    }
    wr64(mem, GDT_ADDR as usize, 0);
    wr64(mem, GDT_ADDR as usize + 8, 0x00AF9A000000FFFF);
    wr64(mem, GDT_ADDR as usize + 16, 0x00CF92000000FFFF);
    w.push((PML4_ADDR, 8));
    w.push((PDPT_ADDR, 8));
    w.push((PD_ADDR, 512 * 8));
    w.push((GDT_ADDR, 24));
}

/// `build_mptable_n`.
pub fn build_mptable_n(mem: &mut [u8], ncpus: u32, w: &mut Written) -> Result<(), String> {
    const APIC_DEFAULT_PHYS_BASE: u32 = 0xfee00000;
    const IOAPIC_DEFAULT_PHYS_BASE: u32 = 0xfec00000;
    const APIC_VERSION: u8 = 0x14;
    const CPU_STEPPING: u32 = 0x600;
    const CPU_FEATURE_APIC: u32 = 0x200;
    const CPU_FEATURE_FPU: u32 = 0x001;
    let mut mpc = [0u8; 512];
    let mut p = 0usize;
    let mut entry_count: u16 = 0;
    mpc[p..p + 4].copy_from_slice(b"PCMP");
    p += 4;
    wr16(&mut mpc, p, 0);
    p += 2;
    mpc[p] = 4;
    p += 1;
    mpc[p] = 0;
    p += 1;
    mpc[p..p + 8].copy_from_slice(b"HYDO    ");
    p += 8;
    mpc[p..p + 12].copy_from_slice(b"MPTB        ");
    p += 12;
    wr32(&mut mpc, p, 0);
    p += 4;
    wr16(&mut mpc, p, 0);
    p += 2;
    wr16(&mut mpc, p, 0);
    p += 2;
    wr32(&mut mpc, p, APIC_DEFAULT_PHYS_BASE);
    p += 4;
    wr16(&mut mpc, p, 0);
    p += 2;
    mpc[p] = 0;
    p += 1;
    mpc[p] = 0;
    p += 1;
    for cpu in 0..ncpus.max(1) {
        mpc[p] = 0;
        mpc[p + 1] = cpu as u8;
        mpc[p + 2] = APIC_VERSION;
        mpc[p + 3] = if cpu != 0 { 0x01 } else { 0x03 };
        p += 4;
        wr32(&mut mpc, p, CPU_STEPPING);
        p += 4;
        wr32(&mut mpc, p, CPU_FEATURE_APIC | CPU_FEATURE_FPU);
        p += 4;
        p += 8;
        entry_count += 1;
    }
    mpc[p] = 1;
    mpc[p + 1] = 0;
    p += 2;
    mpc[p..p + 6].copy_from_slice(b"ISA   ");
    p += 6;
    entry_count += 1;
    mpc[p] = 2;
    mpc[p + 1] = 0;
    mpc[p + 2] = APIC_VERSION;
    mpc[p + 3] = 1;
    p += 4;
    wr32(&mut mpc, p, IOAPIC_DEFAULT_PHYS_BASE);
    p += 4;
    entry_count += 1;
    for irq in 0..16u8 {
        mpc[p] = 3;
        mpc[p + 1] = 0;
        p += 2;
        wr16(&mut mpc, p, 0);
        p += 2;
        mpc[p] = 0;
        mpc[p + 1] = irq;
        mpc[p + 2] = 0;
        mpc[p + 3] = irq;
        p += 4;
        entry_count += 1;
    }
    wr16(&mut mpc, 0x04, p as u16);
    wr16(&mut mpc, 0x22, entry_count);
    mpc[0x07] = checksum(&mpc[..p]);
    guest_write(mem, MPC_ADDR, &mpc[..p])?;
    w.push((MPC_ADDR, p as u64));
    let mut mpf = [0u8; 16];
    mpf[..4].copy_from_slice(b"_MP_");
    wr32(&mut mpf, 4, MPC_ADDR as u32);
    mpf[8] = 1;
    mpf[9] = 4;
    mpf[10] = 0;
    mpf[10] = checksum(&mpf);
    guest_write(mem, MPF_ADDR, &mpf)?;
    w.push((MPF_ADDR, 16));
    Ok(())
}

/// `load_bzimage_n`. Returns the line the C prints on stderr.
pub fn load_bzimage_n(
    mem: &mut [u8],
    image: &[u8],
    initrd: &[u8],
    cmdline: &str,
    ncpus: u32,
    w: &mut Written,
) -> Result<String, String> {
    let mem_size = mem.len() as u64;
    if image.len() < 0x1000 {
        return Err("hvx_boot: kernel image too small".into());
    }
    if rd16(image, 0x1fe) != 0xAA55 || &image[0x202..0x206] != b"HdrS" {
        return Err("hvx_boot: not a bzImage (bad boot_flag/HdrS magic)".into());
    }
    let version = rd16(image, 0x206);
    let xloadflags = rd16(image, 0x236);
    if xloadflags & 1 == 0 {
        return Err("hvx_boot: kernel lacks a 64-bit entry point (XLF_KERNEL_64 clear)".into());
    }
    let setup_sects = if image[OFF_SETUP_SECTS] == 0 {
        4usize
    } else {
        image[OFF_SETUP_SECTS] as usize
    };
    let setup_size = (setup_sects + 1) * 512;
    if setup_size >= image.len() {
        return Err("hvx_boot: bzImage smaller than its declared setup area".into());
    }
    let kernel_len = image.len() - setup_size;
    let kend = KERNEL_LOAD_ADDR + kernel_len as u64;
    if kend >= mem_size {
        return Err(format!(
            "hvx_boot: guest RAM too small for kernel image (need > {kend:#x}, have {mem_size:#x})"
        ));
    }
    guest_write(mem, KERNEL_LOAD_ADDR, &image[setup_size..])?;
    w.push((KERNEL_LOAD_ADDR, kernel_len as u64));
    let mut bp = [0u8; 4096];
    bp[HDR_COPY_START..HDR_COPY_END].copy_from_slice(&image[HDR_COPY_START..HDR_COPY_END]);
    bp[OFF_TYPE_OF_LOADER] = 0xff;
    let mut lf = bp[OFF_LOADFLAGS];
    lf |= LOADED_HIGH | CAN_USE_HEAP;
    lf &= !(QUIET_FLAG | KEEP_SEGMENTS);
    bp[OFF_LOADFLAGS] = lf;
    wr16(&mut bp, OFF_HEAP_END_PTR, 0xfe00);
    let mut cmd = cmdline.as_bytes().to_vec();
    cmd.push(0);
    guest_write(mem, CMDLINE_ADDR, &cmd)?;
    w.push((CMDLINE_ADDR, cmd.len() as u64));
    wr32(&mut bp, OFF_CMD_LINE_PTR, CMDLINE_ADDR as u32);
    wr32(&mut bp, OFF_CMDLINE_SIZE, cmdline.len() as u32);
    if !initrd.is_empty() {
        let cap = mem_size.min(IDENTITY_MAP_CAP);
        if initrd.len() as u64 + 0x100000 >= cap {
            return Err("hvx_boot: guest RAM too small for initramfs".into());
        }
        let addr = (cap - initrd.len() as u64) & !0xfff;
        guest_write(mem, addr, initrd)?;
        w.push((addr, initrd.len() as u64));
        wr32(&mut bp, OFF_RAMDISK_IMAGE, addr as u32);
        wr32(&mut bp, OFF_RAMDISK_SIZE, initrd.len() as u32);
    }
    let mut e820: Vec<(u64, u64, u32)> = vec![(0, EBDA_START, E820_RAM)];
    if mem_size > HIMEM_START {
        e820.push((HIMEM_START, mem_size - HIMEM_START, E820_RAM));
    }
    bp[OFF_E820_ENTRIES] = e820.len() as u8;
    for (i, (a, s, t)) in e820.iter().enumerate() {
        let base = OFF_E820_TABLE + i * 20;
        wr64(&mut bp, base, *a);
        wr64(&mut bp, base + 8, *s);
        wr32(&mut bp, base + 16, *t);
    }
    guest_write(mem, BOOT_PARAMS_ADDR, &bp)?;
    w.push((BOOT_PARAMS_ADDR, 4096));
    build_mptable_n(mem, ncpus, w)?;
    Ok(format!(
        "hvx_boot: bzImage v{}.{}  setup={}B  kernel={}B @ {:#x}  initramfs={}B  cmdline=\"{}\"",
        version >> 8,
        version & 0xff,
        setup_size,
        kernel_len,
        KERNEL_LOAD_ADDR,
        initrd.len(),
        cmdline
    ))
}

/// `fill_sregs`.
pub fn fill_sregs() -> hvx_sregs {
    let ds = hvx_segment {
        selector: 0x10,
        base: 0,
        limit: 0xffffffff,
        ar: 0xc093,
    };
    hvx_sregs {
        cr3: PML4_ADDR,
        cr4: CR4_PAE,
        cr0: CR0_PE | CR0_NE | CR0_PG,
        efer: EFER_LME | EFER_LMA,
        dr7: 0x400,
        cs: hvx_segment {
            selector: 0x08,
            base: 0,
            limit: 0xffffffff,
            ar: 0xa09b,
        },
        ds,
        es: ds,
        fs: ds,
        gs: ds,
        ss: ds,
        tr: hvx_segment {
            selector: 0x18,
            base: 0,
            limit: 0x67,
            ar: 0x8b,
        },
        ldtr: hvx_segment::default(),
        gdt_base: GDT_ADDR,
        gdt_limit: 3 * 8 - 1,
        idt_base: 0,
        idt_limit: 0,
    }
}

/// The partitions of the image's MBR (`mbr_partitions`): (p1_off, p1_len, p2_off, p2_len).
pub fn mbr_partitions(img: &[u8]) -> Result<(u64, u64, u64, u64), String> {
    if img.len() < 512 || img[0x1fe] != 0x55 || img[0x1ff] != 0xaa {
        return Err("hvx_stack_verify: image has no MBR boot signature".into());
    }
    let le = |o: usize| u32::from_le_bytes([img[o], img[o + 1], img[o + 2], img[o + 3]]);
    let (p1_lba, p1_n) = (le(0x1be + 8), le(0x1be + 12));
    let (p2_lba, p2_n) = (le(0x1ce + 8), le(0x1ce + 12));
    if p1_n == 0 || p2_n == 0 {
        return Err("hvx_stack_verify: MBR partition table empty".into());
    }
    let p = (
        p1_lba as u64 * 512,
        p1_n as u64 * 512,
        p2_lba as u64 * 512,
        p2_n as u64 * 512,
    );
    let len = img.len() as u64;
    if p.2 + p.3 > len || p.0 + p.1 > len {
        return Err("hvx_stack_verify: partition ranges exceed disk size".into());
    }
    Ok(p)
}

/// `exit_reason_name`.
pub fn exit_reason_name(r: u32) -> &'static str {
    match r {
        0 => "exception/NMI",
        1 => "external interrupt",
        2 => "TRIPLE FAULT",
        7 => "interrupt window",
        10 => "CPUID",
        12 => "HLT",
        16 => "RDTSC",
        28 => "control-register access",
        30 => "I/O instruction",
        31 => "RDMSR",
        32 => "WRMSR",
        33 => "VM-entry failed: invalid guest state",
        37 => "monitor trap flag (single-step)",
        48 => "EPT violation",
        49 => "EPT misconfiguration",
        51 => "RDTSCP",
        0xffff_ffff => "VMFAIL (VMLAUNCH/VMRESUME failed)",
        0xffff_fffe => "THROTTLE (run-loop bound; re-run)",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mptable_checksums() {
        let mut mem = vec![0u8; 1 << 20];
        let mut w = Vec::new();
        build_mptable_n(&mut mem, 1, &mut w).unwrap();
        let mpc = &mem[MPC_ADDR as usize..];
        let len = rd16(mpc, 4) as usize;
        assert_eq!(len, 44 + 20 + 8 + 8 + 16 * 8);
        assert_eq!(mpc[..len].iter().fold(0u8, |s, &x| s.wrapping_add(x)), 0);
        let mpf = &mem[MPF_ADDR as usize..MPF_ADDR as usize + 16];
        assert_eq!(mpf.iter().fold(0u8, |s, &x| s.wrapping_add(x)), 0);
    }
}
