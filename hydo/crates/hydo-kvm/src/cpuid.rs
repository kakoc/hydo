//! The guest's CPUID: one fixed table per CPU level, the same on every host (Intel or AMD), so
//! the guest takes the same code paths everywhere and nothing about the host reaches it through
//! CPUID. The default level is x86-64-v2 (the hvx engine's default level); `guest.cpu: v3` adds
//! hvx's x86-64-v3 set ([`Level::V3`]: AVX, AVX2, FMA, F16C, BMI1, BMI2, LZCNT, MOVBE and XSAVE for
//! x87, SSE and AVX only, in the standard 832-byte format: no XSAVEOPT, XSAVEC, XGETBV1 or XSAVES,
//! no AVX-512, MPX or PKRU). At both levels everything that leaks the host, or that the engine
//! does not emulate deterministically, is hidden:
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
//!   MONITOR/MWAIT, XSAVE and AVX at v2 (as hvx's v2), PCID/INVPCID, FSGSBASE, MCE/MCA, MTRR, the
//!   speculation-control and ARCH_CAPABILITIES bits, WAITPKG, the thermal and power leaf, the
//!   cache and topology leaves (zero: the guest uses defaults; with `guest.cpu_cache` the cache
//!   leaves describe a fixed hierarchy instead, below).
//!
//! `guest.cpu_cache: true` (Milestone 203, [`table_with`]) fills the cache leaves with the hvx
//! engine's values (`hydo_hvx::abi::HVX_CPUID4_SUB` and its neighbours, the module's
//! `HVX_VM_CPU_CACHE`): leaf 2 says "see leaf 4", leaf 4 lists a 32 KiB L1 data and instruction
//! cache, a 256 KiB L2 and an 8 MiB L3, and leaf 0x8000_0006 gives the L2 again. Without it the
//! glibc 2.28 of Red Hat Enterprise Linux 8 and its rebuilds finds no cache sizes and its AVX
//! `memcpy` (at v3) fails; with it the table describes a CPU the host does not have, the same on
//! every host. Off by default: the table without it is the one every recorded run used.
//!
//! The vendor is `GenuineIntel` on every host: one table means one set of guest code paths; on
//! an AMD host KVM's cross-vendor emulation covers what a 64-bit Linux guest could hit
//! (docs/design/kvm-engine.md, "AMD hosts"). [`check`] refuses a host whose KVM cannot give a
//! feature of the table (rather than a guest that silently differs).
//!
//! At v3, KVM itself keeps two values of the table in step with the guest, as hardware does: the
//! OSXSAVE bit (CPUID.1:ECX 27) mirrors the guest's CR4.OSXSAVE, and CPUID.0xD.0:EBX is the XSAVE
//! area's size for the guest's current XCR0 (576 bytes with x87 and SSE, 832 with AVX; the AVX
//! component is at offset 576 in the standard format on Intel and AMD alike, which [`check`]
//! verifies on the host). Both are functions of the guest's own state.

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
/// CPUID.1:ECX at x86-64-v3, on top of v2's: FMA, MOVBE, XSAVE, AVX, F16C (OSXSAVE is KVM's: it
/// mirrors the guest's CR4.OSXSAVE).
pub const L1_ECX_V3: u32 = (1 << 12) | (1 << 22) | (1 << 26) | (1 << 28) | (1 << 29);
/// CPUID.7.0:EBX at x86-64-v3: BMI1, AVX2, BMI2.
pub const L7_EBX_V3: u32 = (1 << 3) | (1 << 5) | (1 << 8);
/// CPUID.0x80000001:ECX at x86-64-v3, on top of v2's: LZCNT (ABM).
pub const E1_ECX_V3: u32 = 1 << 5;
/// The XSAVE components the guest may enable at x86-64-v3: x87, SSE, AVX (CPUID.0xD.0:EAX).
pub const XCR0_V3: u32 = 0x7;
/// The standard XSAVE area with x87, SSE and AVX: 512 legacy bytes, the 64-byte header, then
/// the AVX component (the upper halves of YMM0-15) at offset 576, 256 bytes.
pub const XSAVE_AVX_OFFSET: u32 = 576;
pub const XSAVE_AVX_SIZE: u32 = 256;
pub const XSAVE_SIZE_V3: u32 = XSAVE_AVX_OFFSET + XSAVE_AVX_SIZE;
/// CPUID.0x80000001:EDX: SYSCALL, NX, LM.
pub const E1_EDX: u32 = (1 << 11) | (1 << 20) | (1 << 29);
/// Family 6, model 0xF, stepping 1 (the hvx engine's synthetic CPU, M12).
pub const L1_EAX: u32 = 0x0000_06f1;
/// CLFLUSH line 8 quadwords, one logical processor, APIC ID 0.
pub const L1_EBX: u32 = 0x0001_0800;
/// 36 physical address bits (every x86-64 host has at least that), 48 linear.
pub const PHYS_BITS: u32 = 36;

/// The guest CPU level (`guest.cpu`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    /// x86-64-v2 (the default): no XSAVE, no AVX; the guest's XCR0 stays at its reset value.
    #[default]
    V2,
    /// x86-64-v3: AVX, AVX2, FMA, F16C, BMI1/2, LZCNT, MOVBE and XSAVE for x87, SSE and AVX.
    V3,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::V2 => "x86-64-v2",
            Level::V3 => "x86-64-v3",
        }
    }
    /// `v2` or `v3` (the manifest's and the engine option's spelling).
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "v2" => Some(Level::V2),
            "v3" => Some(Level::V3),
            _ => None,
        }
    }
}

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

/// The guest's CPUID table at x86-64-v2, in leaf order ([`table_for`]).
pub fn table() -> Vec<kvm_cpuid_entry2> {
    table_for(Level::V2)
}

/// The guest's CPUID table at `level`, in leaf order. The v2 table is the engine's since
/// Milestone 159, unchanged; v3 adds its feature bits, raises the basic leaves to 0xD and
/// describes the XSAVE area there.
pub fn table_for(level: Level) -> Vec<kvm_cpuid_entry2> {
    table_with(level, false)
}

/// The guest's CPUID table at `level`, with the cache leaves (`guest.cpu_cache`, Milestone 203)
/// or without them ([`table_for`]). With them, leaf 2 is `HVX_CPUID2_EAX`, leaf 4 has the
/// sub-leaves `HVX_CPUID4_SUB` and a fifth that ends the list, and leaf 0x8000_0006's ECX is
/// `HVX_CPUID_E6_ECX`: the hvx engine's values for one logical processor, so both engines give a
/// guest the same caches. Nothing else changes.
pub fn table_with(level: Level, cache: bool) -> Vec<kvm_cpuid_entry2> {
    use hydo_hvx::abi::{HVX_CPUID2_EAX, HVX_CPUID4_SUB, HVX_CPUID_E6_ECX};
    let v3 = level == Level::V3;
    let vendor = b"GenuineIntel";
    let v = |i: usize| u32::from_le_bytes(vendor[i * 4..i * 4 + 4].try_into().unwrap());
    let sig = |i: usize| u32::from_le_bytes(SIGNATURE[i * 4..i * 4 + 4].try_into().unwrap());
    let brand: &[u8; 48] = if v3 {
        b"hydo deterministic x86-64-v3 CPU (KVM engine)\0\0\0"
    } else {
        b"hydo deterministic x86-64-v2 CPU (KVM engine)\0\0\0"
    };
    let b0 = regs4(brand[0..16].try_into().unwrap());
    let b1 = regs4(brand[16..32].try_into().unwrap());
    let b2 = regs4(brand[32..48].try_into().unwrap());
    let (l1_ecx, l7_ebx, e1_ecx) = if v3 {
        (L1_ECX_FEATURES | L1_ECX_V3, L7_EBX_V3, E1_ECX | E1_ECX_V3)
    } else {
        (L1_ECX_FEATURES, 0, E1_ECX)
    };
    let mut t = vec![
        // max basic leaf 7 (v3: 0xD, the XSAVE leaf); "GenuineIntel" in EBX, EDX, ECX
        e(0, 0, if v3 { 0xd } else { 7 }, v(0), v(2), v(1)),
        e(1, 0, L1_EAX, L1_EBX, l1_ecx | HYPERVISOR_BIT, L1_EDX),
        // no cache or TLB descriptors (with the cache leaves: "see leaf 4")
        e(
            2,
            0,
            if cache { HVX_CPUID2_EAX } else { 0x0000_0001 },
            0,
            0,
            0,
        ),
        e(3, 0, 0, 0, 0, 0),
    ];
    if cache {
        // the deterministic cache parameters, then type 0 (the end of the list)
        for (i, s) in HVX_CPUID4_SUB.iter().enumerate() {
            t.push(e(4, i as u32, s[0], s[1], s[2], s[3]));
        }
        t.push(e(4, HVX_CPUID4_SUB.len() as u32, 0, 0, 0, 0));
    } else {
        // no deterministic cache parameters (type 0 ends the list)
        t.push(e(4, 0, 0, 0, 0, 0));
    }
    t.extend([
        // no MONITOR/MWAIT, no thermal and power management
        e(5, 0, 0, 0, 0, 0),
        e(6, 0, 0, 0, 0, 0),
        // structured extended features: none at v2, BMI1/AVX2/BMI2 at v3 (never FSGSBASE, ERMS,
        // INVPCID, RDSEED, RDPID, WAITPKG, AVX-512, speculation control or ARCH_CAPABILITIES)
        e(7, 0, 0, l7_ebx, 0, 0),
    ]);
    if v3 {
        // leaves 8 to 0xC are absent, so zero (no PMU, no topology). The XSAVE leaf: x87, SSE
        // and AVX; EBX the size for the current XCR0 (KVM keeps it in step), ECX the size for
        // all of them; sub-leaf 1 zero (no XSAVEOPT, XSAVEC, XGETBV1, XSAVES); sub-leaf 2 the
        // AVX component
        t.extend([
            e(0xd, 0, XCR0_V3, XSAVE_AVX_OFFSET, XSAVE_SIZE_V3, 0),
            e(0xd, 1, 0, 0, 0, 0),
            e(0xd, 2, XSAVE_AVX_SIZE, XSAVE_AVX_OFFSET, 0, 0),
        ]);
    }
    t.extend([
        // the hypervisor leaves: hydo's signature, interface 1, no paravirtual features
        e(0x4000_0000, 0, 0x4000_0001, sig(0), sig(1), sig(2)),
        e(0x4000_0001, 0, 1, 0, 0, 0),
        e(0x8000_0000, 0, 0x8000_0008, 0, 0, 0),
        e(0x8000_0001, 0, 0, 0, e1_ecx, E1_EDX),
        e(0x8000_0002, 0, b0[0], b0[1], b0[2], b0[3]),
        e(0x8000_0003, 0, b1[0], b1[1], b1[2], b1[3]),
        e(0x8000_0004, 0, b2[0], b2[1], b2[2], b2[3]),
        e(0x8000_0005, 0, 0, 0, 0, 0),
        e(
            0x8000_0006,
            0,
            0,
            0,
            if cache { HVX_CPUID_E6_ECX } else { 0 },
            0,
        ),
        // no invariant TSC, no power management
        e(0x8000_0007, 0, 0, 0, 0, 0),
        e(0x8000_0008, 0, (48 << 8) | PHYS_BITS, 0, 0, 0),
    ]);
    for x in t.iter_mut().filter(|x| matches!(x.function, 4 | 7 | 0xd)) {
        x.flags = KVM_CPUID_FLAG_SIGNIFCANT_INDEX;
    }
    t
}

/// The bytes of the table at `level` (their hash is recorded: `tests::table_is_fixed`).
pub fn table_bytes(level: Level) -> Vec<u8> {
    let mut b = Vec::new();
    for x in table_for(level) {
        for v in [x.function, x.index, x.flags, x.eax, x.ebx, x.ecx, x.edx] {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    b
}

/// Which feature bits of the table the host's KVM must support (`KVM_GET_SUPPORTED_CPUID`):
/// (leaf, register name, the table's bits). The hypervisor bit is KVM's own and not checked.
pub fn required(level: Level) -> Vec<(u32, &'static str, u32)> {
    let mut r = vec![
        (1, "ecx", L1_ECX_FEATURES),
        (1, "edx", L1_EDX),
        (0x8000_0001, "ecx", E1_ECX),
        (0x8000_0001, "edx", E1_EDX),
    ];
    if level == Level::V3 {
        r.extend([
            (1, "ecx", L1_ECX_V3),
            (7, "ebx", L7_EBX_V3),
            (0x8000_0001, "ecx", E1_ECX_V3),
            (0xd, "eax", XCR0_V3),
        ]);
    }
    r
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
        (1, "ecx", 12) => "fma",
        (1, "ecx", 22) => "movbe",
        (1, "ecx", 26) => "xsave",
        (1, "ecx", 28) => "avx",
        (1, "ecx", 29) => "f16c",
        (7, "ebx", 3) => "bmi1",
        (7, "ebx", 5) => "avx2",
        (7, "ebx", 8) => "bmi2",
        (0x8000_0001, "ecx", 5) => "lzcnt",
        (0xd, "eax", 0) => "XSAVE of the x87 state",
        (0xd, "eax", 1) => "XSAVE of the SSE state",
        (0xd, "eax", 2) => "XSAVE of the AVX state",
        _ => "",
    };
    if n.is_empty() {
        format!("CPUID.{leaf:#x}:{reg} bit {bit}")
    } else {
        n.to_string()
    }
}

/// Check the table at `level` against the host's supported CPUID: Err names every missing
/// feature, a physical address width below [`PHYS_BITS`], or (v3) an XSAVE layout that does not
/// put the AVX state at [`XSAVE_AVX_OFFSET`].
pub fn check(supported: &[kvm_cpuid_entry2], level: Level) -> Result<(), String> {
    let find_at = |leaf: u32, index: u32| {
        supported
            .iter()
            .find(|x| x.function == leaf && x.index == index)
    };
    let find = |leaf: u32| find_at(leaf, 0);
    let mut missing = Vec::new();
    for (leaf, reg, bits) in required(level) {
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
    if level == Level::V3 && missing.is_empty() {
        match find_at(0xd, 2) {
            Some(x) if x.eax == XSAVE_AVX_SIZE && x.ebx == XSAVE_AVX_OFFSET => {}
            Some(x) => missing.push(format!(
                "the AVX state at offset {XSAVE_AVX_OFFSET} ({XSAVE_AVX_SIZE} bytes) of the \
                 standard XSAVE area (the host says offset {}, {} bytes)",
                x.ebx, x.eax
            )),
            None => missing.push("CPUID.0xD.2 (where the AVX state is in the XSAVE area)".into()),
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "this host's KVM lacks what the guest CPU ({}) needs: {}",
            level.name(),
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
        assert!(check(&host, Level::V2).is_ok());
        for x in host.iter_mut() {
            if x.function == 1 {
                x.ecx &= !(1 << 23);
            }
            if x.function == 0x8000_0008 {
                x.eax = 32;
            }
        }
        let e = check(&host, Level::V2).unwrap_err();
        assert!(e.contains("popcnt"), "{e}");
        assert!(e.contains("x86-64-v2"), "{e}");
        assert!(
            e.contains("36 physical address bits (the host has 32)"),
            "{e}"
        );
    }

    #[test]
    fn v3_adds_hvx_s_set_and_nothing_else() {
        let v2 = table();
        let v3 = table_for(Level::V3);
        let l1 = leaf(&v3, 1);
        assert_eq!(l1.ecx & !leaf(&v2, 1).ecx, L1_ECX_V3);
        // FMA, MOVBE, XSAVE, AVX, F16C; never OSXSAVE (KVM's), RDRAND, x2APIC, TSC deadline
        assert_eq!(L1_ECX_V3, 0x3440_1000);
        assert_eq!(l1.ecx & (1 << 27 | 1 << 30 | 1 << 21 | 1 << 24), 0);
        assert_eq!(l1.edx, leaf(&v2, 1).edx);
        let l7 = leaf(&v3, 7);
        assert_eq!((l7.eax, l7.ebx, l7.ecx, l7.edx), (0, L7_EBX_V3, 0, 0));
        assert_eq!(
            L7_EBX_V3, 0x128,
            "BMI1, AVX2, BMI2: no AVX-512, RDSEED, ERMS"
        );
        let e1 = leaf(&v3, 0x8000_0001);
        assert_eq!(e1.ecx, E1_ECX | (1 << 5), "LAHF and LZCNT");
        assert_eq!(e1.edx, leaf(&v2, 0x8000_0001).edx);
        assert_eq!(leaf(&v3, 0).eax, 0xd);
        // the XSAVE leaf: x87|SSE|AVX, the standard 832-byte area, no XSAVEOPT/XSAVEC/XSAVES
        let d: Vec<_> = v3.iter().filter(|x| x.function == 0xd).collect();
        assert_eq!(d.len(), 3);
        assert!(d.iter().all(|x| x.flags == KVM_CPUID_FLAG_SIGNIFCANT_INDEX));
        assert_eq!((d[0].index, d[0].eax, d[0].ecx, d[0].edx), (0, 7, 832, 0));
        assert_eq!((d[1].index, d[1].eax, d[1].ebx, d[1].ecx), (1, 0, 0, 0));
        assert_eq!((d[2].index, d[2].eax, d[2].ebx), (2, 256, 576));
        // no leaf of the host between 7 and 0xD (PMU, topology), and the rest as at v2
        assert!(v3.iter().all(|x| !(8..=0xc).contains(&x.function)));
        for x in &v2 {
            if !matches!(x.function, 0 | 1 | 7 | 0x8000_0001..=0x8000_0004) {
                assert_eq!(
                    Some(x),
                    v3.iter()
                        .find(|y| y.function == x.function && y.index == x.index)
                );
            }
        }
        let mut b = Vec::new();
        for f in 0x8000_0002..=0x8000_0004 {
            let x = leaf(&v3, f);
            for r in [x.eax, x.ebx, x.ecx, x.edx] {
                b.extend_from_slice(&r.to_le_bytes());
            }
        }
        assert!(b.starts_with(b"hydo deterministic x86-64-v3 CPU"));
        assert_eq!(Level::parse("v3"), Some(Level::V3));
        assert_eq!(Level::parse("v4"), None);
    }

    #[test]
    fn v3_check_names_what_is_missing() {
        // a host with the v3 table's features (and the AVX state where the standard format has it)
        let mut host = table_for(Level::V3);
        assert!(check(&host, Level::V3).is_ok());
        assert!(check(&host, Level::V2).is_ok());
        // a v2 host: what v3 needs is named, v2 still runs
        let v2 = table();
        let e = check(&v2, Level::V3).unwrap_err();
        for n in [
            "avx", "avx2", "fma", "f16c", "movbe", "xsave", "bmi1", "bmi2", "lzcnt",
        ] {
            assert!(e.split([':', ',']).any(|w| w.trim() == n), "{n}: {e}");
        }
        assert!(
            e.contains("XSAVE of the AVX state") && e.contains("x86-64-v3"),
            "{e}"
        );
        assert!(check(&v2, Level::V2).is_ok());
        // an XSAVE layout without the AVX state at 576
        for x in host.iter_mut() {
            if x.function == 0xd && x.index == 2 {
                x.ebx = 640;
            }
        }
        let e = check(&host, Level::V3).unwrap_err();
        assert!(e.contains("the host says offset 640"), "{e}");
    }

    /// The size glibc computes from a leaf-4 entry: ways * partitions * line size * sets.
    fn size4(x: &kvm_cpuid_entry2) -> u32 {
        ((x.ebx >> 22) + 1) * (((x.ebx >> 12) & 0x3ff) + 1) * ((x.ebx & 0xfff) + 1) * (x.ecx + 1)
    }

    #[test]
    fn cache_leaves_are_hvx_s_and_change_nothing_else() {
        for level in [Level::V2, Level::V3] {
            let off = table_for(level);
            assert_eq!(table_with(level, false), off);
            let on = table_with(level, true);
            assert_eq!(
                leaf(&on, 2).eax,
                0x0000_ff01,
                "one round, descriptor 0xff: see leaf 4"
            );
            assert_eq!(leaf(&off, 2).eax, 1, "without: no descriptors");
            let l4: Vec<_> = on.iter().filter(|x| x.function == 4).collect();
            assert_eq!(l4.len(), 5);
            let mut sizes = Vec::new();
            for (i, x) in l4.iter().enumerate() {
                assert_eq!(x.index, i as u32);
                assert_eq!(x.flags, KVM_CPUID_FLAG_SIGNIFCANT_INDEX);
                if i < 4 {
                    let h = hydo_hvx::abi::HVX_CPUID4_SUB[i];
                    assert_eq!([x.eax, x.ebx, x.ecx, x.edx], h);
                    // (type, level), one logical processor: no sharing, one core
                    assert_eq!(x.eax >> 14, 0);
                    sizes.push((x.eax & 0x1f, (x.eax >> 5) & 7, size4(x)));
                } else {
                    assert_eq!(
                        (x.eax, x.ebx, x.ecx, x.edx),
                        (0, 0, 0, 0),
                        "the end of the list"
                    );
                }
            }
            assert_eq!(
                sizes,
                [
                    (1, 1, 32 << 10),
                    (2, 1, 32 << 10),
                    (3, 2, 256 << 10),
                    (3, 3, 8 << 20)
                ]
            );
            assert_eq!(l4[3].edx & 2, 2, "the L3 is inclusive");
            let e6 = leaf(&on, 0x8000_0006);
            assert_eq!((e6.eax, e6.ebx, e6.edx), (0, 0, 0));
            assert_eq!(e6.ecx >> 16, 256, "the L2 in KiB");
            assert_eq!((e6.ecx >> 12) & 0xf, 4, "4-way");
            assert_eq!(e6.ecx & 0xff, 64, "64-byte lines");
            assert_eq!(leaf(&off, 0x8000_0006).ecx, 0);
            // every other entry is the table without the cache leaves
            let rest = |t: &[kvm_cpuid_entry2]| -> Vec<kvm_cpuid_entry2> {
                t.iter()
                    .filter(|x| !matches!(x.function, 2 | 4 | 0x8000_0006))
                    .copied()
                    .collect()
            };
            assert_eq!(rest(&on), rest(&off));
        }
    }

    #[test]
    fn table_is_fixed() {
        // the table shapes every guest run: a change is a new baseline of this engine
        assert_eq!(table_bytes(Level::V2).len(), 19 * 28);
        assert_eq!(
            format!(
                "{:016x}",
                hydo_hvm::fnv::fnv(&table_bytes(Level::V2), hydo_hvm::fnv::OFFSET)
            ),
            TABLE_FNV
        );
        assert_eq!(table_bytes(Level::V3).len(), 22 * 28);
        assert_eq!(
            format!(
                "{:016x}",
                hydo_hvm::fnv::fnv(&table_bytes(Level::V3), hydo_hvm::fnv::OFFSET)
            ),
            TABLE_V3_FNV
        );
        // M203: with the cache leaves (four more leaf-4 entries)
        let with = |l| {
            let mut b = Vec::new();
            for x in table_with(l, true) {
                for v in [x.function, x.index, x.flags, x.eax, x.ebx, x.ecx, x.edx] {
                    b.extend_from_slice(&v.to_le_bytes());
                }
            }
            b
        };
        assert_eq!(with(Level::V2).len(), 23 * 28);
        assert_eq!(with(Level::V3).len(), 26 * 28);
        assert_eq!(
            [
                format!(
                    "{:016x}",
                    hydo_hvm::fnv::fnv(&with(Level::V2), hydo_hvm::fnv::OFFSET)
                ),
                format!(
                    "{:016x}",
                    hydo_hvm::fnv::fnv(&with(Level::V3), hydo_hvm::fnv::OFFSET)
                )
            ],
            [TABLE_CACHE_FNV, TABLE_V3_CACHE_FNV]
        );
    }

    /// The v2 table's FNV-1a (recorded since Milestone 159; [`table_is_fixed`]).
    const TABLE_FNV: &str = "d7160ae07579d4da";
    /// The v3 table's FNV-1a (recorded; [`table_is_fixed`]).
    const TABLE_V3_FNV: &str = "4ee3ccb3c689b48a";
    /// The tables with the cache leaves (M203; [`table_is_fixed`]).
    const TABLE_CACHE_FNV: &str = "dbc8a19ef8edfb20";
    const TABLE_V3_CACHE_FNV: &str = "470155319f8ae4e4";
}
