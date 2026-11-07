//! The guest's MSRs. KVM is told to let the guest touch only the MSRs that are pure guest state
//! (the system-call entry points, segment bases, EFER and PAT: their values are whatever the
//! guest wrote, saved and restored with the vCPU), and to send every other RDMSR and WRMSR to the
//! engine (`KVM_X86_SET_MSR_FILTER` with default deny, `KVM_CAP_X86_USER_SPACE_MSR`). The engine
//! answers those from this fixed model, so no MSR value comes from the host or from KVM's own
//! emulation (KVM answers IA32_TSC, PLATFORM_INFO, ARCH_CAPABILITIES, UCODE_REV and others from
//! the host; none of them reaches the guest).
//!
//! The model: a few MSRs Linux reads without a fault handler get fixed values (MISC_ENABLE is
//! also writable, as DEBUGCTL), IA32_TSC reads the exit clock, and every other MSR raises #GP, as
//! on a CPU that does not have it (Linux probes those with the `_safe` accessors).

/// The MSRs KVM handles itself (allowed by the filter): pure guest state.
pub const KVM_OWNED: [u32; 12] = [
    0x174,       // IA32_SYSENTER_CS
    0x175,       // IA32_SYSENTER_ESP
    0x176,       // IA32_SYSENTER_EIP
    0x277,       // IA32_PAT
    0xc000_0080, // EFER (also in sregs)
    0xc000_0081, // STAR
    0xc000_0082, // LSTAR
    0xc000_0083, // CSTAR
    0xc000_0084, // SFMASK
    0xc000_0100, // FS_BASE (also in sregs)
    0xc000_0101, // GS_BASE (also in sregs)
    0xc000_0102, // KERNEL_GS_BASE
];

/// The KVM-owned MSRs a vCPU snapshot saves with `KVM_GET_MSRS` (EFER and the FS/GS bases are
/// in the segment registers already).
pub const SNAPSHOT: [u32; 9] = [
    0x174,
    0x175,
    0x176,
    0x277,
    0xc000_0081,
    0xc000_0082,
    0xc000_0083,
    0xc000_0084,
    0xc000_0102,
];

pub const IA32_TSC: u32 = 0x10;
pub const IA32_PLATFORM_ID: u32 = 0x17;
pub const IA32_APIC_BASE: u32 = 0x1b;
pub const IA32_FEAT_CTL: u32 = 0x3a;
pub const IA32_UCODE_REV: u32 = 0x8b;
pub const IA32_MISC_ENABLE: u32 = 0x1a0;
pub const IA32_DEBUGCTL: u32 = 0x1d9;

/// MISC_ENABLE at reset: fast strings, branch trace and PEBS unavailable.
pub const MISC_ENABLE_RESET: u64 = (1 << 0) | (1 << 11) | (1 << 12);

/// The filter's allow ranges: (first MSR, count, bitmap), one bit per MSR, 1 = KVM handles it.
pub fn filter_ranges() -> Vec<(u32, u32, Vec<u8>)> {
    let mut v: Vec<(u32, u32, Vec<u8>)> = Vec::new();
    for &m in KVM_OWNED.iter() {
        if let Some(r) = v.last_mut() {
            if r.0 + r.1 == m {
                r.1 += 1;
                continue;
            }
        }
        v.push((m, 1, Vec::new()));
    }
    for r in v.iter_mut() {
        r.2 = vec![0u8; (r.1 as usize).div_ceil(8)];
        for i in 0..r.1 as usize {
            r.2[i / 8] |= 1 << (i % 8);
        }
    }
    v
}

/// The engine's MSR state (in the snapshot and the fingerprint).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Msrs {
    pub misc_enable: u64,
    pub debugctl: u64,
}

impl Default for Msrs {
    fn default() -> Msrs {
        Msrs {
            misc_enable: MISC_ENABLE_RESET,
            debugctl: 0,
        }
    }
}

/// What the engine answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Value(u64),
    /// #GP: the CPU has no such MSR (or the write is not allowed).
    Fault,
}

impl Msrs {
    /// RDMSR `index` at virtual time `now`.
    pub fn read(&self, index: u32, now: u64) -> Answer {
        match index {
            IA32_TSC => Answer::Value(now),
            IA32_PLATFORM_ID | IA32_UCODE_REV | IA32_APIC_BASE => Answer::Value(0),
            IA32_FEAT_CTL => Answer::Value(1), // locked, VMX off
            IA32_MISC_ENABLE => Answer::Value(self.misc_enable),
            IA32_DEBUGCTL => Answer::Value(self.debugctl),
            _ => Answer::Fault,
        }
    }

    /// WRMSR `index` = `v`: Ok, or #GP.
    pub fn write(&mut self, index: u32, v: u64) -> Answer {
        match index {
            // the microcode revision probe writes 0 first; a TSC write changes nothing (the
            // guest has no TSC); the APIC is disabled and stays so
            IA32_UCODE_REV | IA32_TSC | IA32_APIC_BASE => Answer::Value(0),
            IA32_MISC_ENABLE => {
                self.misc_enable = v;
                Answer::Value(0)
            }
            IA32_DEBUGCTL => {
                // only the BTF bit (single-step on branches) is ever written by Linux; keep what
                // the guest wrote, KVM does not see it
                self.debugctl = v;
                Answer::Value(0)
            }
            _ => Answer::Fault,
        }
    }

    pub fn known(index: u32) -> bool {
        matches!(
            index,
            IA32_TSC
                | IA32_PLATFORM_ID
                | IA32_UCODE_REV
                | IA32_APIC_BASE
                | IA32_FEAT_CTL
                | IA32_MISC_ENABLE
                | IA32_DEBUGCTL
        )
    }

    /// The canonical bytes (for the machine state's hash).
    pub fn bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&self.misc_enable.to_le_bytes());
        b[8..].copy_from_slice(&self.debugctl.to_le_bytes());
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_allows_exactly_the_kvm_owned_msrs() {
        let r = filter_ranges();
        assert_eq!(
            r.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>(),
            vec![(0x174, 3), (0x277, 1), (0xc000_0080, 5), (0xc000_0100, 3)]
        );
        let allowed = |m: u32| {
            r.iter().any(|(b, n, bm)| {
                m >= *b && m < b + n && bm[((m - b) / 8) as usize] & (1 << ((m - b) % 8)) != 0
            })
        };
        for m in KVM_OWNED {
            assert!(allowed(m), "{m:#x}");
        }
        // the MSRs KVM would answer from the host are not allowed: they reach the engine
        for m in [
            0x10,
            0x3b,
            0xce,
            0x10a,
            0x8b,
            0x1a0,
            0x6e0,
            0xc000_0103,
            0x4b56_4d00,
        ] {
            assert!(!allowed(m), "{m:#x}");
        }
        for m in SNAPSHOT {
            assert!(KVM_OWNED.contains(&m));
        }
    }

    #[test]
    fn model() {
        let mut m = Msrs::default();
        assert_eq!(m.read(IA32_TSC, 1234), Answer::Value(1234));
        assert_eq!(m.read(IA32_MISC_ENABLE, 0), Answer::Value(0x1801));
        assert_eq!(m.write(IA32_MISC_ENABLE, 0x1), Answer::Value(0));
        assert_eq!(m.read(IA32_MISC_ENABLE, 0), Answer::Value(1));
        assert_eq!(m.read(0xce, 0), Answer::Fault);
        assert_eq!(m.write(0x48, 1), Answer::Fault);
        assert_eq!(m.read(IA32_FEAT_CTL, 0), Answer::Value(1));
        assert!(!Msrs::known(0x10a));
    }
}
