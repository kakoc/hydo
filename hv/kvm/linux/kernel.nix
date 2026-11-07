# The x86_64 guest kernel of hydo's KVM engine (docs/design/kvm-engine.md): Linux 6.6.35 from
# kernel.org (the tarball nixpkgs' linux_6_6 uses, pinned by sha256), hydo-kvm-guest.patch, and
# allnoconfig plus hydo-kvm.config, built with the pinned nixpkgs' gcc and binutils.
#
#   nix-build hv/kvm/linux/kernel.nix --arg nixpkgs <pinned nixpkgs>      (build-kernel.sh does it)
#
# The result: bzImage-kvm, its .config, and vmlinux for the RDTSC audit (audit-rdtsc.sh).
# Reproducible: fixed KBUILD_BUILD_* values, the kernel's own path remapping, no host path or
# time inside; `nix-build --check` rebuilds it and compares the bytes. The pinned values are in
# docs/BUILD-PINS.md "The KVM engine's guest kernel".
{ nixpkgs }:
let
  pkgs = import nixpkgs {
    system = "x86_64-linux";
    config = { };
    overlays = [ ];
  };
in
pkgs.stdenv.mkDerivation {
  pname = "hydo-kvm-guest-kernel";
  version = "6.6.35";
  src = pkgs.fetchurl {
    url = "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.6.35.tar.xz";
    sha256 = "fce3ee728712ed063aa8c14a8756c8ff8c7a46ba3827f61d2b04a73c7cf5dd9e";
  };
  patches = [ ./hydo-kvm-guest.patch ];
  nativeBuildInputs = with pkgs; [ bc bison flex perl python3 elfutils ];
  # the compiler wrapper's hardening flags are for programs, not for a kernel
  hardeningDisable = [ "all" ];
  dontStrip = true;
  # No build-id notes. The kernel's: the linker hashes the image before scripts/sorttable sorts
  # the exception and ORC tables in place, and two builds gave two build-ids for otherwise equal
  # bytes. The vDSO's (inside the kernel's .rodata): a build outside Nix's sandbox gave the vDSO
  # another build-id, the only 20 bytes that differed.
  postPatch = ''
    patchShebangs scripts tools
    sed -i 's/^LDFLAGS_vmlinux += --build-id=sha1$/LDFLAGS_vmlinux += --build-id=none/' Makefile
    grep -qx 'LDFLAGS_vmlinux += --build-id=none' Makefile
    sed -i 's/--hash-style=both --build-id=sha1/--hash-style=both --build-id=none/' arch/x86/entry/vdso/Makefile
    grep -q 'build-id=none' arch/x86/entry/vdso/Makefile
  '';
  configurePhase = ''
    runHook preConfigure
    make ARCH=x86_64 allnoconfig KCONFIG_ALLCONFIG=${./hydo-kvm.config}
    for c in $(sed -n 's/^\(CONFIG_[A-Z0-9_]*\)=y$/\1/p' ${./hydo-kvm.config}); do
      grep -qx "$c=y" .config || { echo "missing $c=y" >&2; exit 1; }
    done
    for c in $(sed -n 's/^\(CONFIG_[A-Z0-9_]*\)=n$/\1/p' ${./hydo-kvm.config}); do
      ! grep -qx "$c=y" .config || { echo "unexpected $c=y" >&2; exit 1; }
    done
    runHook postConfigure
  '';
  buildPhase = ''
    runHook preBuild
    make ARCH=x86_64 KBUILD_BUILD_TIMESTAMP=2026-01-01 KBUILD_BUILD_USER=hydo \
      KBUILD_BUILD_HOST=hydo KBUILD_BUILD_VERSION=1 -j$NIX_BUILD_CORES bzImage
    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp arch/x86/boot/bzImage $out/bzImage-kvm
    cp .config $out/bzImage-kvm.config
    cp vmlinux $out/vmlinux
    runHook postInstall
  '';
}
