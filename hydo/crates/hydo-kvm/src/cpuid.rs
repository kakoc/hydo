//! The guest's CPUID: one fixed table, the same on every host (Intel or AMD), so the guest takes
//! the same code paths everywhere and nothing about the host reaches it through CPUID. It is
//! x86-64-v2 (the hvx engine's default level) with everything that leaks the host, or that the
//! engine does not emulate deterministically, hidden:
//!
//! - the TSC (leaf 1 EDX 4), RDTSCP (0x80000001 EDX 27), the invariant TSC (0x80000007), the TSC
//!   deadline timer and TSC_ADJUST: the guest kernel (hydo-kvm-guest.patch) takes its time from
//!   the engine's exit clock and emulates user-space RDTSC under CR4.TSD;
//! - the local APIC and x2APIC (leaf 1 EDX 9, ECX 21): the engine emulates an 8259 PIC instead,
//!   so every interrupt decision is the engine's (KVM keeps the APIC bit in step with the
//!   APIC_BASE MSR, so the engine also sets that MSR to 0, [`crate::vmm`]);
//! - KVM's paravirtual interface: the hypervisor leaves carry the signature
//!   [`SIGNATURE`] instead of KVM's, so no kvmclock, steal time, async page faults, PV EOI,
//!   PV TLB flush, PV spinlocks or PV send-IPI is ever enabled;
//! - the PMU (leaf 0xA is zero; the engine also disables KVM's PMU), RDRAND and RDSEED, RDPID,
//!   MONITOR/MWAIT, XSAVE and AVX (as hvx's v2), PCID/INVPCID, FSGSBASE, MCE/MCA, MTRR, the
//!   speculation-control and ARCH_CAPABILITIES bits, WAITPKG, the thermal and power leaf, the
//!   cache and topology leaves (zero: the guest uses defaults).
//!
//! The vendor is `GenuineIntel` on every host: one table means one set of guest code paths; on
//! an AMD host KVM's cross-vendor emulation covers what a 64-bit Linux guest could hit
//! (docs/design/kvm-engine.md, "AMD hosts"). [`check`] refuses a host whose KVM cannot give a
//! feature of the table (rather than a guest that silently differs).

use crate::abi::{kvm_cpuid_entry2, KVM_CPUID_FLAG_SIGNIFCANT_INDEX};

/// The hypervisor signature at leaf 0x4000_0000 (hydo-kvm-guest.patch detects it).
pub const SIGNATURE: &[u8; 12] = b"hydo-kvm-x86";

/// CPUID.1:EDX: FPU, DE, PSE, MSR, PAE, CX8, SEP, PGE, CMOV, PAT, PSE36, CLFSH, MMX, FXSR, SSE,
/// SSE2.
pub const L1_EDX: u32 = (1 << 0)
    | (1 << 2)
    | (1 << 3)
    | (1 << 5)
    | (1 << 6)
    | (1 << 8)
    | (1 << 11)
    | (1 << 13)
    | (1 << 15)
    | (1 << 16)
    | (1 << 17)
    | (1 << 19)
    | (1 << 23)
    | (1 << 24)
    | (1 << 25)
    | (1 << 26);
/// CPUID.1:ECX: SSE3, SSSE3, CX16, SSE4.1, SSE4.2, POPCNT (x86-64-v2), and the hypervisor bit.
pub const L1_ECX_FEATURES: u32 =
    (1 << 0) | (1 << 9) | (1 << 13) | (1 << 19) | (1 << 20) | (1 << 23);
pub const HYPERVISOR_BIT: u32 = 1 << 31;
/// CPUID.0x80000001:ECX: LAHF/SAHF in 64-bit mode (x86-64-v2).
pub const E1_ECX: u32 = 1 << 0;
/// CPUID.0x80000001:EDX: SYSCALL, NX, LM.
pub const E1_EDX: u32 = (1 << 11) | (1 << 20) | (1 << 29);
/// Family 6, model 0xF, stepping 1 (the hvx engine's synthetic CPU, M12).
pub const L1_EAX: u32 = 0x0000_06f1;
/// CLFLUSH line 8 quadwords, one logical processor, APIC ID 0.
pub const L1_EBX: u32 = 0x0001_0800;
/// 36 physical address bits (every x86-64 host has at least that), 48 linear.
pub const PHYS_BITS: u32 = 36;

fn e(function: u32, index: u32, eax: u32, ebx: u32, ecx: u32, edx: u32) -> kvm_cpuid_entry2 {
    kvm_cpuid_entry2 {
        function,
        index,
        flags: 0,
        eax,
        ebx,
        ecx,
        edx,
        padding: [0; 3],
    }
}

fn regs4(s: &[u8; 16]) -> [u32; 4] {
    let w = |i: usize| u32::from_le_bytes(s[i * 4..i * 4 + 4].try_into().unwrap());
    [w(0), w(1), w(2), w(3)]
}

/// The guest's CPUID table, in leaf order.
pub fn table() -> Vec<kvm_cpuid_entry2> {
    let vendor = b"GenuineIntel";
    let v = |i: usize| u32::from_le_bytes(vendor[i * 4..i * 4 + 4].try_into().unwrap());
    let sig = |i: usize| u32::from_le_bytes(SIGNATURE[i * 4..i * 4 + 4].try_into().unwrap());
    let brand: &[u8; 48] = b"hydo deterministic x86-64-v2 CPU (KVM engine)\0\0\0";
    let b0 = regs4(brand[0..16].try_into().unwrap());
    let b1 = regs4(brand[16..32].try_into().unwrap());
    let b2 = regs4(brand[32..48].try_into().unwrap());
    let mut t = vec![
        // max basic leaf 7; "GenuineIntel" in EBX, EDX, ECX
        e(0, 0, 7, v(0), v(2), v(1)),
        e(
            1,
            0,
            L1_EAX,
            L1_EBX,
            L1_ECX_FEATURES | HYPERVISOR_BIT,
            L1_EDX,
        ),
        // no cache or TLB descriptors
        e(2, 0, 0x0000_0001, 0, 0, 0),
        e(3, 0, 0, 0, 0, 0),
        // no deterministic cache parameters (type 0 ends the list)
        e(4, 0, 0, 0, 0, 0),
        // no MONITOR/MWAIT, no thermal and power management
        e(5, 0, 0, 0, 0, 0),
        e(6, 0, 0, 0, 0, 0),
        // structured extended features: none (no FSGSBASE, ERMS, INVPCID, RDSEED, RDPID,
        // WAITPKG, speculation control or ARCH_CAPABILITIES)
        e(7, 0, 0, 0, 0, 0),
        // the hypervisor leaves: hydo's signature, interface 1, no paravirtual features
        e(0x4000_0000, 0, 0x4000_0001, sig(0), sig(1), sig(2)),
        e(0x4000_0001, 0, 1, 0, 0, 0),
        e(0x8000_0000, 0, 0x8000_0008, 0, 0, 0),
        e(0x8000_0001, 0, 0, 0, E1_ECX, E1_EDX),
        e(0x8000_0002, 0, b0[0], b0[1], b0[2], b0[3]),
        e(0x8000_0003, 0, b1[0], b1[1], b1[2], b1[3]),
        e(0x8000_0004, 0, b2[0], b2[1], b2[2], b2[3]),
        e(0x8000_0005, 0, 0, 0, 0, 0),
        e(0x8000_0006, 0, 0, 0, 0, 0),
        // no invariant TSC, no power management
        e(0x8000_0007, 0, 0, 0, 0, 0),
        e(0x8000_0008, 0, (48 << 8) | PHYS_BITS, 0, 0, 0),
    ];
    for x in t.iter_mut().filter(|x| matches!(x.function, 4 | 7)) {
        x.flags = KVM_CPUID_FLAG_SIGNIFCANT_INDEX;
    }
    t
}

/// The bytes of the table, for the fingerprint seed and the engine's identity line.
pub fn table_bytes() -> Vec<u8> {
    let mut b = Vec::new();
    for x in table() {
        for v in [x.function, x.index, x.flags, x.eax, x.ebx, x.ecx, x.edx] {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    b
}

/// Which feature bits of the table the host's KVM must support (`KVM_GET_SUPPORTED_CPUID`):
/// (leaf, register name, the table's bits). The hypervisor bit is KVM's own and not checked.
pub fn required() -> Vec<(u32, &'static str, u32)> {
    vec![
        (1, "ecx", L1_ECX_FEATURES),
        (1, "edx", L1_EDX),
        (0x8000_0001, "ecx", E1_ECX),
        (0x8000_0001, "edx", E1_EDX),
    ]
}

/// The names of the table's feature bits (for a refusal that says what the host lacks).
pub fn bit_name(leaf: u32, reg: &str, bit: u32) -> String {
    let n = match (leaf, reg, bit) {
        (1, "edx", 0) => "fpu",
        (1, "edx", 2) => "de",
        (1, "edx", 3) => "pse",
        (1, "edx", 5) => "msr",
        (1, "edx", 6) => "pae",
        (1, "edx", 8) => "cx8",
        (1, "edx", 11) => "sep",
        (1, "edx", 13) => "pge",
        (1, "edx", 15) => "cmov",
        (1, "edx", 16) => "pat",
        (1, "edx", 17) => "pse36",
        (1, "edx", 19) => "clflush",
        (1, "edx", 23) => "mmx",
        (1, "edx", 24) => "fxsr",
        (1, "edx", 25) => "sse",
        (1, "edx", 26) => "sse2",
        (1, "ecx", 0) => "sse3",
        (1, "ecx", 9) => "ssse3",
        (1, "ecx", 13) => "cx16",
        (1, "ecx", 19) => "sse4.1",
        (1, "ecx", 20) => "sse4.2",
        (1, "ecx", 23) => "popcnt",
        (0x8000_0001, "ecx", 0) => "lahf_lm",
        (0x8000_0001, "edx", 11) => "syscall",
        (0x8000_0001, "edx", 20) => "nx",
        (0x8000_0001, "edx", 29) => "lm",
        _ => "",
    };
    if n.is_empty() {
        format!("CPUID.{leaf:#x}:{reg} bit {bit}")
    } else {
        n.to_string()
    }
}

/// Check the table against the host's supported CPUID: Err names every missing feature, or a
/// physical address width below [`PHYS_BITS`].
pub fn check(supported: &[kvm_cpuid_entry2]) -> Result<(), String> {
    let find = |leaf: u32| {
        supported
            .iter()
            .find(|x| x.function == leaf && x.index == 0)
    };
    let mut missing = Vec::new();
    for (leaf, reg, bits) in required() {
        let have = find(leaf).map_or(0, |x| match reg {
            "eax" => x.eax,
            "ebx" => x.ebx,
            "ecx" => x.ecx,
            _ => x.edx,
        });
        for bit in 0..32 {
            if bits & (1 << bit) != 0 && have & (1 << bit) == 0 {
                missing.push(bit_name(leaf, reg, bit));
            }
        }
    }
    if let Some(x) = find(0x8000_0008) {
        if x.eax & 0xff < PHYS_BITS {
            missing.push(format!(
                "{PHYS_BITS} physical address bits (the host has {})",
                x.eax & 0xff
            ));
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "this host's KVM lacks what the guest CPU (x86-64-v2) needs: {}",
            missing.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(t: &[kvm_cpuid_entry2], f: u32) -> kvm_cpuid_entry2 {
        *t.iter().find(|x| x.function == f).unwrap()
    }

    #[test]
    fn hides_every_host_leak() {
        let t = table();
        let l1 = leaf(&t, 1);
        assert_eq!(l1.edx & (1 << 4), 0, "TSC");
        assert_eq!(l1.edx & (1 << 9), 0, "APIC");
        assert_eq!(l1.edx & (1 << 7) | l1.edx & (1 << 14), 0, "MCE/MCA");
        assert_eq!(l1.edx & (1 << 12), 0, "MTRR");
        assert_eq!(l1.ecx & (1 << 21), 0, "x2APIC");
        assert_eq!(l1.ecx & (1 << 24), 0, "TSC deadline");
        assert_eq!(l1.ecx & (1 << 30), 0, "RDRAND");
        assert_eq!(l1.ecx & (1 << 3), 0, "MONITOR");
        assert_eq!(
            l1.ecx & (1 << 26) | l1.ecx & (1 << 27) | l1.ecx & (1 << 28),
            0,
            "XSAVE/AVX"
        );
        assert_eq!(l1.ecx & (1 << 17), 0, "PCID");
        assert_ne!(l1.ecx & HYPERVISOR_BIT, 0);
        let l7 = leaf(&t, 7);
        assert_eq!(
            (l7.eax, l7.ebx, l7.ecx, l7.edx),
            (0, 0, 0, 0),
            "RDSEED, RDPID, ..."
        );
        assert!(
            t.iter().all(|x| x.function != 0xa),
            "no PMU leaf (max leaf 7)"
        );
        let e1 = leaf(&t, 0x8000_0001);
        assert_eq!(e1.edx & (1 << 27), 0, "RDTSCP");
        assert_eq!(e1.edx & (1 << 26), 0, "1 GiB pages");
        assert_eq!(leaf(&t, 0x8000_0007).edx, 0, "invariant TSC");
        let h = leaf(&t, 0x4000_0000);
        let mut s = Vec::new();
        for r in [h.ebx, h.ecx, h.edx] {
            s.extend_from_slice(&r.to_le_bytes());
        }
        assert_eq!(&s, SIGNATURE);
        assert_ne!(&s[..9], b"KVMKVMKVM");
        assert_eq!(leaf(&t, 0x4000_0001).eax, 1);
        assert_eq!(leaf(&t, 0).eax, 7);
        assert!(t.iter().all(|x| x.function <= 7
            || (0x4000_0000..=0x4000_0001).contains(&x.function)
            || (0x8000_0000..=0x8000_0008).contains(&x.function)));
    }

    #[test]
    fn vendor_and_brand() {
        let t = table();
        let l0 = leaf(&t, 0);
        let mut v = Vec::new();
        for r in [l0.ebx, l0.edx, l0.ecx] {
            v.extend_from_slice(&r.to_le_bytes());
        }
        assert_eq!(&v, b"GenuineIntel");
        let mut b = Vec::new();
        for f in 0x8000_0002..=0x8000_0004 {
            let x = leaf(&t, f);
            for r in [x.eax, x.ebx, x.ecx, x.edx] {
                b.extend_from_slice(&r.to_le_bytes());
            }
        }
        assert!(b.starts_with(b"hydo deterministic x86-64-v2 CPU"));
    }

    #[test]
    fn check_names_what_is_missing() {
        let mut host = table();
        assert!(check(&host).is_ok());
        for x in host.iter_mut() {
            if x.function == 1 {
                x.ecx &= !(1 << 23);
            }
            if x.function == 0x8000_0008 {
                x.eax = 32;
            }
        }
        let e = check(&host).unwrap_err();
        assert!(e.contains("popcnt"), "{e}");
        assert!(
            e.contains("36 physical address bits (the host has 32)"),
            "{e}"
        );
    }

    #[test]
    fn table_is_fixed() {
        // the table shapes every guest run: a change is a new baseline of this engine
        assert_eq!(table_bytes().len(), 19 * 28);
        assert_eq!(
            format!(
                "{:016x}",
                hydo_hvm::fnv::fnv(&table_bytes(), hydo_hvm::fnv::OFFSET)
            ),
            TABLE_FNV
        );
    }

    /// The table's FNV-1a (recorded; [`table_is_fixed`]).
    const TABLE_FNV: &str = "d7160ae07579d4da";
}
