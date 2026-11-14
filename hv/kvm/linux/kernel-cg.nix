# The KVM engine's guest kernels from the fingerprint epoch 3 on (docs/design/guest-cgroups.md):
# kernel.nix (one vCPU) or kernel-smp.nix (several vCPUs) with hydo-kvm-cgroups.config appended to
# their list. Neither recipe changes, so bzImage-kvm and bzImage-kvm-smp keep their pins for the
# images built at epoch 2 and before.
#
#   nix-build hv/kvm/linux/kernel-cg.nix --arg nixpkgs <pinned nixpkgs> [--arg smp true]
#   (CGROUPS=1 [SMP=1] sh hv/kvm/linux/build-kernel.sh)
#
# The result: bzImage-kvm-cg (or bzImage-kvm-smp-cg), its .config, and vmlinux for
# audit-rdtsc.sh. Pins: docs/BUILD-PINS.md "The KVM engine's guest kernel".
{ nixpkgs, smp ? false }:
let
  base = import (if smp then ./kernel-smp.nix else ./kernel.nix) { inherit nixpkgs; };
  pkgs = import nixpkgs {
    system = "x86_64-linux";
    config = { };
    overlays = [ ];
  };
  name = if smp then "bzImage-kvm-smp-cg" else "bzImage-kvm-cg";
  config = pkgs.runCommand "${name}.allconfig" { } (
    (if smp then ''
      grep -v -E '^CONFIG_(SMP|X86_X2APIC)=' ${./hydo-kvm.config} > $out
      cat ${./hydo-kvm-smp.config} >> $out
    '' else ''
      cat ${./hydo-kvm.config} > $out
    '') + ''
      cat ${./hydo-kvm-cgroups.config} >> $out
    ''
  );
in
base.overrideAttrs (old: {
  pname = if smp then "hydo-kvm-guest-kernel-smp-cg" else "hydo-kvm-guest-kernel-cg";
  configurePhase = ''
    runHook preConfigure
    make ARCH=x86_64 allnoconfig KCONFIG_ALLCONFIG=${config}
    for c in $(sed -n 's/^\(CONFIG_[A-Z0-9_]*\)=y$/\1/p' ${config}); do
      grep -qx "$c=y" .config || { echo "missing $c=y" >&2; exit 1; }
    done
    for c in $(sed -n 's/^\(CONFIG_[A-Z0-9_]*\)=n$/\1/p' ${config}); do
      ! grep -qx "$c=y" .config || { echo "unexpected $c=y" >&2; exit 1; }
    done
    ${if smp then ''grep -qx 'CONFIG_NR_CPUS=8' .config || { echo "missing CONFIG_NR_CPUS=8" >&2; exit 1; }'' else ""}
    runHook postConfigure
  '';
  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp arch/x86/boot/bzImage $out/${name}
    cp .config $out/${name}.config
    cp vmlinux $out/vmlinux
    runHook postInstall
  '';
})
