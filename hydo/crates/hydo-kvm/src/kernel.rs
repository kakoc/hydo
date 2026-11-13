//! The engine's guest kernel and how an image finds it.
//!
//! An x86_64 image records the kernel its root and initramfs were made for (`/etc/hydo/image.json`,
//! M155: the hvx engine's `bzImage`, by sha256). This engine boots the same images with its own
//! build of that kernel: the same Linux release with hv/kvm/linux/hydo-kvm-guest.patch and the
//! configuration hv/kvm/linux/hydo-kvm.config (built by hv/kvm/linux/build-kernel.sh, pinned in
//! docs/BUILD-PINS.md). [`TWINS`] pairs each recorded kernel with its KVM build, so an image needs
//! no new record and keeps its bytes (and every fingerprint the other engines recorded for it);
//! the image's own initramfs is used as it is (its module loads fail on this kernel, which has
//! every driver built in, and its init goes on, as it was written to).

/// A KVM build of a recorded kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Twin {
    /// the sha256 of the kernel an image records (hydo-image's x86_64 kernel)
    pub of: &'static str,
    /// the file name of the KVM build
    pub file: &'static str,
    /// its sha256 (docs/BUILD-PINS.md)
    pub sha256: &'static str,
    pub version: &'static str,
}

/// The KVM build of the x86_64 guest kernel (Linux 6.6.35).
pub const KERNEL: Twin = Twin {
    of: "0a9ede352483e1718a8583e35afa3e82791c8223e40c0a888d39b0530158e9d3",
    file: "bzImage-kvm",
    sha256: "d50fbc67261a2544404d7a5f3e7a79cabe3b0a2e1f455158ccc5369d878d6207",
    version: "6.6.35",
};

/// Every pair this engine knows.
pub const TWINS: [Twin; 1] = [KERNEL];

/// The SMP build of the same kernel, for several vCPUs (docs/design/kvm-vcpus.md):
/// hv/kvm/linux/kernel-smp.nix, `SMP=1 sh hv/kvm/linux/build-kernel.sh`.
pub const KERNEL_SMP: Twin = Twin {
    of: KERNEL.of,
    file: "bzImage-kvm-smp",
    sha256: "17415b1f447957c150c3036f85f952c5b475c7f3a0327262a3d06a7cc9aaf2bf",
    version: "6.6.35",
};

/// The SMP pairs, as [`TWINS`].
pub const SMP_TWINS: [Twin; 1] = [KERNEL_SMP];

/// The KVM build of the kernel with sha256 `of`.
pub fn twin_of(of: &str) -> Option<Twin> {
    TWINS.iter().copied().find(|t| t.of == of)
}

/// The KVM build of the kernel with sha256 `of` for a machine of `vcpus` vCPUs: the UP build for
/// one, the SMP build for more.
pub fn twin_for(of: &str, vcpus: u32) -> Option<Twin> {
    if vcpus > 1 {
        SMP_TWINS.iter().copied().find(|t| t.of == of)
    } else {
        twin_of(of)
    }
}
