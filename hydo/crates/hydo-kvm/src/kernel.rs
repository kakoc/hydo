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
//!
//! hvx's kernel did not change at the fingerprint epoch 3, so the recorded sha256 does not tell
//! an image built at epoch 3 from one built before. Each build therefore names the epochs of the
//! images it boots: an image of epoch 2 or before boots `bzImage-kvm` (or `bzImage-kvm-smp`) and
//! keeps its fingerprints; an image of epoch 3 or later boots the build with cgroups,
//! `bzImage-kvm-cg` (or `bzImage-kvm-smp-cg`), on which a pause of threads runs (Milestone 234,
//! docs/design/guest-cgroups.md).
//!
//! An image whose manifest asks for a kernel variant (`guest.kernel: containers`, Milestone 256,
//! docs/design/k3s-in-guest.md) records it in `kernel_variant`, and boots the build of that
//! variant: `bzImage-kvm-containers` (or `bzImage-kvm-smp-containers`). The recorded kernel is
//! still hvx's, which has those options already.

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
    /// the fingerprint epochs of the images it boots, from and to (both included)
    pub epochs: (u64, u64),
    /// the kernel variant an image's record names (`kernel_variant`), None for the images
    /// without one
    pub variant: Option<&'static str>,
}

/// The KVM build of the x86_64 guest kernel (Linux 6.6.35) for images built at the fingerprint
/// epoch 2 and before.
pub const KERNEL: Twin = Twin {
    of: "0a9ede352483e1718a8583e35afa3e82791c8223e40c0a888d39b0530158e9d3",
    file: "bzImage-kvm",
    sha256: "d50fbc67261a2544404d7a5f3e7a79cabe3b0a2e1f455158ccc5369d878d6207",
    version: "6.6.35",
    epochs: (0, 2),
    variant: None,
};

/// The SMP build of the same kernel, for several vCPUs (docs/design/kvm-vcpus.md):
/// hv/kvm/linux/kernel-smp.nix, `SMP=1 sh hv/kvm/linux/build-kernel.sh`.
pub const KERNEL_SMP: Twin = Twin {
    of: KERNEL.of,
    file: "bzImage-kvm-smp",
    sha256: "b40b2711a845dcf8622bf5230e0cb4d775b27ad2c297b16b0e5a8d631e23aac3",
    version: "6.6.35",
    epochs: (0, 2),
    variant: None,
};

/// The KVM build with cgroups, for images built at the fingerprint epoch 3 and later
/// (hv/kvm/linux/kernel-cg.nix, `CGROUPS=1 sh hv/kvm/linux/build-kernel.sh`).
pub const KERNEL_CG: Twin = Twin {
    of: KERNEL.of,
    file: "bzImage-kvm-cg",
    sha256: "f0e2d441f334437fa4a5053645dd21057cba4843d890b7d9fda90f7be50da9bc",
    version: "6.6.35",
    epochs: (3, u64::MAX),
    variant: None,
};

/// The SMP build with cgroups (`CGROUPS=1 SMP=1 sh hv/kvm/linux/build-kernel.sh`).
pub const KERNEL_SMP_CG: Twin = Twin {
    of: KERNEL.of,
    file: "bzImage-kvm-smp-cg",
    sha256: "f24a52e3f76c782b4bb559bb5d47d3e1acce8c8e862aa8201268abeccb8c1f01",
    version: "6.6.35",
    epochs: (3, u64::MAX),
    variant: None,
};

/// The KVM build of the variant `containers` (Milestone 256): `bzImage-kvm-cg`'s list with the
/// options of hv/kvm/linux/hydo-kvm-containers.config (hv/kvm/linux/kernel-containers.nix,
/// `CONTAINERS=1 sh hv/kvm/linux/build-kernel.sh`).
pub const KERNEL_CONTAINERS: Twin = Twin {
    of: KERNEL.of,
    file: "bzImage-kvm-containers",
    sha256: "0c9c1c9531eda025e096c60530055a556a7f25754376c260b17482066808f4fe",
    version: "6.6.35",
    epochs: (3, u64::MAX),
    variant: Some("containers"),
};

/// The SMP build of the variant `containers`, with the cpuset controller too
/// (`CONTAINERS=1 SMP=1 sh hv/kvm/linux/build-kernel.sh`).
pub const KERNEL_SMP_CONTAINERS: Twin = Twin {
    of: KERNEL.of,
    file: "bzImage-kvm-smp-containers",
    sha256: "6f85c1c5efc381afbbdf8845d44603258223e7dfdec9260ce5bf4956f271250c",
    version: "6.6.35",
    epochs: (3, u64::MAX),
    variant: Some("containers"),
};

/// The one-vCPU builds this engine knows.
pub const TWINS: [Twin; 3] = [KERNEL, KERNEL_CG, KERNEL_CONTAINERS];

/// The SMP builds, as [`TWINS`].
pub const SMP_TWINS: [Twin; 3] = [KERNEL_SMP, KERNEL_SMP_CG, KERNEL_SMP_CONTAINERS];

fn pick(list: &[Twin], of: &str, epoch: u64, variant: Option<&str>) -> Option<Twin> {
    list.iter()
        .copied()
        .find(|t| t.of == of && (t.epochs.0..=t.epochs.1).contains(&epoch) && t.variant == variant)
}

/// The one-vCPU KVM build of the kernel with sha256 `of` for an image built at `epoch` without a
/// kernel variant.
pub fn twin_of(of: &str, epoch: u64) -> Option<Twin> {
    pick(&TWINS, of, epoch, None)
}

/// The KVM build of the kernel with sha256 `of` for an image built at `epoch` with the kernel
/// variant `variant` (the record's `kernel_variant`) on a machine of `vcpus` vCPUs: the UP build
/// for one, the SMP build for more.
pub fn twin_for(of: &str, vcpus: u32, epoch: u64, variant: Option<&str>) -> Option<Twin> {
    if vcpus > 1 {
        pick(&SMP_TWINS, of, epoch, variant)
    } else {
        pick(&TWINS, of, epoch, variant)
    }
}

/// Every KVM build this engine knows: the one-vCPU builds, then the SMP builds.
pub fn all() -> impl Iterator<Item = Twin> {
    TWINS.into_iter().chain(SMP_TWINS)
}
