# The KVM engine's guest kernel variant `containers` (docs/design/k3s-in-guest.md, Milestone 256):
# kernel.nix (one vCPU) or kernel-smp.nix (several vCPUs) with the list of kernel-cg.nix, without
# the options that hydo-kvm-containers.config names, then that file (and CONFIG_CPUSETS=y for
# several vCPUs). An image asks for it with `guest.kernel: containers`. No other recipe changes,
# so every other KVM kernel keeps its pin.
#
#   nix-build hv/kvm/linux/kernel-containers.nix --arg nixpkgs <pinned nixpkgs> [--arg smp true]
#   (CONTAINERS=1 [SMP=1] sh hv/kvm/linux/build-kernel.sh)
#
# The result: bzImage-kvm-containers (or bzImage-kvm-smp-containers), its .config, and vmlinux
# for audit-rdtsc.sh. Pins: docs/BUILD-PINS.md "The guest kernels for containers".
{ nixpkgs, smp ? false }:
let
  base = import (if smp then ./kernel-smp.nix else ./kernel.nix) { inherit nixpkgs; };
  pkgs = import nixpkgs {
    system = "x86_64-linux";
    config = { };
    overlays = [ ];
  };
  name = if smp then "bzImage-kvm-smp-containers" else "bzImage-kvm-containers";
  config = pkgs.runCommand "${name}.allconfig" { } (
    (if smp then ''
      grep -v -E '^CONFIG_(SMP|X86_X2APIC)=' ${./hydo-kvm.config} > base
      cat ${./hydo-kvm-smp.config} >> base
    '' else ''
      cat ${./hydo-kvm.config} > base
    '') + ''
      cat ${./hydo-kvm-cgroups.config} >> base
      cat ${./hydo-kvm-containers.config} > list
    '' + (if smp then ''
      echo CONFIG_CPUSETS=y >> list
    '' else "") + ''
      sed -n 's/^\(CONFIG_[A-Z0-9_]*\)=.*$/\1/p' list > names
      grep -v -F -w -f names base > $out
      cat list >> $out
    ''
  );
in
base.overrideAttrs (old: {
  pname = if smp then "hydo-kvm-guest-kernel-smp-containers" else "hydo-kvm-guest-kernel-containers";
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
