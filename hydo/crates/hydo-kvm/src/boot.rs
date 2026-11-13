//! The Linux boot recipe: hydo-hvx's (`hv/hvx/hvx_boot.h` ported byte for byte: long-mode page
//! tables and GDT, the bzImage and initramfs in guest RAM, the zero page with its e820 map and
//! the command line) without its MP table, and the vCPU's first state as KVM's types.
//!
//! The MP table is erased because it names a local APIC and an I/O APIC this machine does not
//! have (the guest has the 8259 PIC only); everything else is the hvx engine's recipe, called as
//! it is, so the two engines load a kernel identically (`tests` below compare the bytes).

use crate::abi::{kvm_dtable, kvm_regs, kvm_segment, kvm_sregs};
use hydo_hvx::abi::hvx_segment;
use hydo_hvx::boot;

/// Load `image` (a bzImage), `initrd` and `cmdline` into `mem` (the guest RAM buffer: its low
/// block from GPA 0, the rest from 4 GiB, [`crate::layout::ram_blocks`]; everything the recipe
/// writes is below 1 GiB, and its e820 map describes the same two blocks, hydo-hvx's
/// `boot::e820_map`): Ok with a line describing what was loaded.
pub fn load(mem: &mut [u8], image: &[u8], initrd: &[u8], cmdline: &str) -> Result<String, String> {
    let mut w = Vec::new();
    boot::build_page_tables_and_gdt(mem, &mut w);
    let line = boot::load_bzimage_n(mem, image, initrd, cmdline, 1, &mut w)
        .map_err(|e| e.replace("hvx_boot", "hydo-kvm boot"))?;
    // no MP table: erase the floating pointer and the table the recipe wrote
    for (at, len) in mp_ranges(&w) {
        mem[at as usize..(at + len) as usize].fill(0);
    }
    Ok(line.replace("hvx_boot", "hydo-kvm boot"))
}

/// [`load`] for a machine with `n` vCPUs: with more than one, the MP table lists `n`
/// processors (APIC IDs 0 to n-1, the first the boot CPU) and the ISA bus, and no I/O APIC
/// (docs/design/kvm-vcpus.md, "What the guest sees"): the 8259 PIC reaches vCPU 0's LINT0. One
/// vCPU is [`load`] itself.
pub fn load_n(
    mem: &mut [u8],
    image: &[u8],
    initrd: &[u8],
    cmdline: &str,
    n: usize,
) -> Result<String, String> {
    let line = load(mem, image, initrd, cmdline)?;
    if n > 1 {
        mptable(mem, n);
    }
    Ok(line)
}

/// Where the MP floating pointer and the table go (the EBDA, as hydo-hvx's recipe).
const MPF_ADDR: usize = 0x9fc00;
const MPC_ADDR: usize = 0x9fc10;

/// The byte that makes `b` sum to 0 (the MP specification's checksums).
fn checksum(b: &[u8]) -> u8 {
    0u8.wrapping_sub(b.iter().fold(0u8, |s, x| s.wrapping_add(*x)))
}

/// The MP table of `n` processors without an I/O APIC, and its floating pointer.
fn mptable(mem: &mut [u8], n: usize) {
    let mut t = Vec::with_capacity(64 + 20 * n);
    t.extend_from_slice(b"PCMP");
    t.extend_from_slice(&[0, 0, 4, 0]); // length (below), revision 1.4, checksum (below)
    t.extend_from_slice(b"HYDO    ");
    t.extend_from_slice(b"KVM SMP     ");
    t.extend_from_slice(&[0; 4]); // no OEM table
    t.extend_from_slice(&[0; 2]);
    t.extend_from_slice(&[0; 2]); // entry count (below)
    t.extend_from_slice(&0xfee0_0000u32.to_le_bytes());
    t.extend_from_slice(&[0; 4]); // no extended table
    for cpu in 0..n {
        // a processor: APIC ID, APIC version 0x14, enabled (and the boot CPU for 0), family 6
        t.extend_from_slice(&[0, cpu as u8, 0x14, if cpu == 0 { 3 } else { 1 }]);
        t.extend_from_slice(&0x600u32.to_le_bytes());
        t.extend_from_slice(&0x201u32.to_le_bytes()); // APIC and FPU
        t.extend_from_slice(&[0; 8]);
    }
    t.extend_from_slice(&[1, 0]);
    t.extend_from_slice(b"ISA   ");
    let len = t.len() as u16;
    t[4..6].copy_from_slice(&len.to_le_bytes());
    t[34..36].copy_from_slice(&((n + 1) as u16).to_le_bytes());
    t[7] = checksum(&t);
    mem[MPC_ADDR..MPC_ADDR + t.len()].copy_from_slice(&t);
    let mut f = [0u8; 16];
    f[..4].copy_from_slice(b"_MP_");
    f[4..8].copy_from_slice(&(MPC_ADDR as u32).to_le_bytes());
    f[8] = 1; // 16 bytes
    f[9] = 4;
    f[10] = checksum(&f);
    mem[MPF_ADDR..MPF_ADDR + 16].copy_from_slice(&f);
}

/// The byte ranges of the MP floating pointer and table among a recipe's writes.
fn mp_ranges(w: &boot::Written) -> Vec<(u64, u64)> {
    // build_mptable_n writes the table at 0x9fc10 and the pointer at 0x9fc00 (the EBDA)
    w.iter()
        .copied()
        .filter(|(at, _)| *at == 0x9fc00 || *at == 0x9fc10)
        .collect()
}

/// A segment of hvx's form (VMX access rights) as KVM's.
pub fn segment(s: &hvx_segment) -> kvm_segment {
    let ar = s.ar as u32;
    kvm_segment {
        base: s.base,
        limit: s.limit,
        selector: s.selector,
        type_: (ar & 0xf) as u8,
        s: ((ar >> 4) & 1) as u8,
        dpl: ((ar >> 5) & 3) as u8,
        present: ((ar >> 7) & 1) as u8,
        avl: ((ar >> 12) & 1) as u8,
        l: ((ar >> 13) & 1) as u8,
        db: ((ar >> 14) & 1) as u8,
        g: ((ar >> 15) & 1) as u8,
        unusable: (ar == 0) as u8,
        padding: 0,
    }
}

/// The first system registers: hvx's long mode (`fill_sregs`), and the APIC base 0 (the APIC
/// disabled: KVM then clears the APIC bit of the guest's CPUID, see [`crate::cpuid`]).
pub fn sregs() -> kvm_sregs {
    let h = boot::fill_sregs();
    kvm_sregs {
        cs: segment(&h.cs),
        ds: segment(&h.ds),
        es: segment(&h.es),
        fs: segment(&h.fs),
        gs: segment(&h.gs),
        ss: segment(&h.ss),
        tr: segment(&h.tr),
        ldt: segment(&h.ldtr),
        gdt: kvm_dtable {
            base: h.gdt_base,
            limit: h.gdt_limit as u16,
            padding: [0; 3],
        },
        idt: kvm_dtable {
            base: h.idt_base,
            limit: h.idt_limit as u16,
            padding: [0; 3],
        },
        cr0: h.cr0,
        cr2: 0,
        cr3: h.cr3,
        cr4: h.cr4,
        cr8: 0,
        efer: h.efer,
        apic_base: 0,
        interrupt_bitmap: [0; 4],
    }
}

/// The first general registers: the 64-bit entry with the zero page in RSI (as hydo-hvx).
pub fn regs() -> kvm_regs {
    kvm_regs {
        rip: boot::KERNEL_LOAD_ADDR + boot::KERNEL_ENTRY_OFF,
        rsi: boot::BOOT_PARAMS_ADDR,
        rsp: boot::STACK_TOP,
        rflags: 0x2,
        ..kvm_regs::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic bzImage: the header fields the recipe reads, and a payload.
    fn bzimage() -> Vec<u8> {
        let mut b = vec![0u8; 0x4000];
        b[0x1f1] = 4; // setup_sects
        b[0x1fe] = 0x55;
        b[0x1ff] = 0xaa;
        b[0x202..0x206].copy_from_slice(b"HdrS");
        b[0x206..0x208].copy_from_slice(&0x020fu16.to_le_bytes());
        b[0x236..0x238].copy_from_slice(&1u16.to_le_bytes()); // XLF_KERNEL_64
        for (i, x) in b.iter_mut().enumerate().skip(5 * 512) {
            *x = (i * 7) as u8;
        }
        b
    }

    #[test]
    fn several_vcpus_get_an_mp_table_without_an_io_apic() {
        let mut mem = vec![0u8; 64 << 20];
        load_n(&mut mem, &bzimage(), b"initrd", "console=ttyS0", 3).unwrap();
        let f = &mem[MPF_ADDR..MPF_ADDR + 16];
        assert_eq!(&f[..4], b"_MP_");
        assert_eq!(
            f.iter().fold(0u8, |s, x| s.wrapping_add(*x)),
            0,
            "pointer checksum"
        );
        let len = u16::from_le_bytes([mem[MPC_ADDR + 4], mem[MPC_ADDR + 5]]) as usize;
        let t = &mem[MPC_ADDR..MPC_ADDR + len];
        assert_eq!(&t[..4], b"PCMP");
        assert_eq!(
            t.iter().fold(0u8, |s, x| s.wrapping_add(*x)),
            0,
            "table checksum"
        );
        assert_eq!(
            u16::from_le_bytes([t[34], t[35]]),
            4,
            "3 processors and a bus"
        );
        assert_eq!(len, 44 + 3 * 20 + 8);
        // processor entries: APIC IDs 0..3, the boot CPU first; no I/O APIC (type 2) entry
        for cpu in 0..3 {
            let e = &t[44 + 20 * cpu..44 + 20 * (cpu + 1)];
            assert_eq!(
                (e[0], e[1], e[3]),
                (0, cpu as u8, if cpu == 0 { 3 } else { 1 })
            );
        }
        assert_eq!(t[44 + 60], 1, "the bus entry");
        // one vCPU: the recipe of before, no table
        let mut one = vec![0u8; 64 << 20];
        load_n(&mut one, &bzimage(), b"initrd", "console=ttyS0", 1).unwrap();
        let mut base = vec![0u8; 64 << 20];
        load(&mut base, &bzimage(), b"initrd", "console=ttyS0").unwrap();
        assert!(one == base);
    }

    #[test]
    fn the_hvx_recipe_without_the_mp_table() {
        let img = bzimage();
        let initrd = vec![0xa5u8; 5000];
        let size = 64 << 20;
        let mut ours = vec![0u8; size];
        load(&mut ours, &img, &initrd, "console=ttyS0").unwrap();
        let mut hvx = vec![0u8; size];
        let mut w = Vec::new();
        boot::build_page_tables_and_gdt(&mut hvx, &mut w);
        boot::load_bzimage_n(&mut hvx, &img, &initrd, "console=ttyS0", 1, &mut w).unwrap();
        let mp = mp_ranges(&w);
        assert_eq!(mp.len(), 2, "the recipe writes the MP pointer and table");
        for (i, (a, b)) in ours.iter().zip(hvx.iter()).enumerate() {
            let in_mp = mp
                .iter()
                .any(|(at, len)| (*at..at + len).contains(&(i as u64)));
            if in_mp {
                assert_eq!(*a, 0, "MP byte {i:#x} left");
            } else {
                assert_eq!(a, b, "byte {i:#x} differs from hvx's recipe");
            }
        }
        assert_ne!(&hvx[0x9fc00..0x9fc04], b"\0\0\0\0");
        assert_eq!(&ours[0x9fc00..0x9fc04], b"\0\0\0\0");
    }

    /// The zero page's e820 entries: (address, size, type).
    fn e820_of(mem: &[u8]) -> Vec<(u64, u64, u32)> {
        let bp = &mem[0x10000..0x11000];
        let rd = |o: usize, n: usize| {
            let mut b = [0u8; 8];
            b[..n].copy_from_slice(&bp[o..o + n]);
            u64::from_le_bytes(b)
        };
        (0..bp[0x1e8] as usize)
            .map(|i| {
                let o = 0x2d0 + i * 20;
                (rd(o, 8), rd(o + 8, 8), rd(o + 16, 4) as u32)
            })
            .collect()
    }

    #[test]
    fn e820_one_block_up_to_3_gib_and_the_rest_at_4_gib() {
        let img = bzimage();
        // 3 GiB and less: the recipe's two entries (the test above compares the bytes)
        let mut mem = vec![0u8; 64 << 20];
        load(&mut mem, &img, &[], "x").unwrap();
        assert_eq!(
            e820_of(&mem),
            vec![(0, 0x9fc00, 1), (0x100000, (64 << 20) - 0x100000, 1)]
        );
        // above 3 GiB: 1 MiB to 3 GiB, then the rest from 4 GiB, as layout::ram_blocks maps it
        // (the buffer is zero pages the kernel never backs, apart from what the recipe writes)
        let len = (3u64 << 30) + (64 << 20);
        let mut big = vec![0u8; len as usize];
        load(&mut big, &img, &[], "x").unwrap();
        let blocks = crate::layout::ram_blocks(len);
        assert_eq!(
            e820_of(&big),
            vec![
                (0, 0x9fc00, 1),
                (0x100000, blocks[0].2 - 0x100000, 1),
                (blocks[1].0, blocks[1].2, 1)
            ]
        );
        assert_eq!(blocks[1], (0x1_0000_0000, 0xC000_0000, 64 << 20));
    }

    #[test]
    fn segments_and_registers() {
        let s = sregs();
        assert_eq!(
            (s.cs.type_, s.cs.s, s.cs.present, s.cs.l, s.cs.db, s.cs.g),
            (0xb, 1, 1, 1, 0, 1)
        );
        assert_eq!(
            (s.ss.type_, s.ss.s, s.ss.present, s.ss.l, s.ss.db, s.ss.g),
            (3, 1, 1, 0, 1, 1)
        );
        assert_eq!((s.tr.type_, s.tr.s, s.tr.present), (0xb, 0, 1));
        assert_eq!(s.ldt.unusable, 1);
        assert_eq!(s.apic_base, 0);
        assert_eq!(s.efer & (1 << 10), 1 << 10, "long mode active");
        assert_eq!(s.gdt.limit, 23);
        let r = regs();
        assert_eq!((r.rip, r.rsi, r.rflags), (0x100200, 0x10000, 2));
    }
}
