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

/// Load `image` (a bzImage), `initrd` and `cmdline` into `mem` (guest RAM from GPA 0): Ok with a
/// line describing what was loaded.
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
