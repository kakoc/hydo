# The KVM engine's guest kernels from the fingerprint epoch 7 on (Milestone 376): the list of
# kernel-cg.nix (or, with containers = true, of kernel-containers.nix) with hydo-kvm-procio.config
# appended, so the guest has /proc/<pid>/io. kernel.nix (one vCPU) or kernel-smp.nix (several
# vCPUs) is the base. No other recipe changes, so every other KVM kernel keeps its pin for the
# images built at epoch 6 and before (and for an image whose manifest says guest.proc_io: false).
#
#   nix-build hv/kvm/linux/kernel-procio.nix --arg nixpkgs <pinned nixpkgs> [--arg smp true]
#     [--arg containers true]
#   (PROCIO=1 [CONTAINERS=1] [SMP=1] sh hv/kvm/linux/build-kernel.sh)
#
# The result: bzImage-kvm-io, bzImage-kvm-smp-io, bzImage-kvm-containers-io or
# bzImage-kvm-smp-containers-io, its .config, and vmlinux for audit-rdtsc.sh. Pins:
# docs/BUILD-PINS.md "The guest kernels with /proc/<pid>/io".
{ nixpkgs, smp ? false, containers ? false }:
let
  base = import (if smp then ./kernel-smp.nix else ./kernel.nix) { inherit nixpkgs; };
  pkgs = import nixpkgs {
    system = "x86_64-linux";
    config = { };
    overlays = [ ];
  };
  stem = (if smp then "bzImage-kvm-smp" else "bzImage-kvm")
    + (if containers then "-containers" else "");
  name = "${stem}-io";
  # the list of kernel-cg.nix or kernel-containers.nix, as those recipes make it
  config = pkgs.runCommand "${name}.allconfig" { } (
    (if smp then ''
      grep -v -E '^CONFIG_(SMP|X86_X2APIC)=' ${./hydo-kvm.config} > base
      cat ${./hydo-kvm-smp.config} >> base
    '' else ''
      cat ${./hydo-kvm.config} > base
    '') + ''
      cat ${./hydo-kvm-cgroups.config} >> base
    '' + (if containers then ''
      cat ${./hydo-kvm-containers.config} > list
    '' + (if smp then ''
      echo CONFIG_CPUSETS=y >> list
    '' else "") + ''
      sed -n 's/^\(CONFIG_[A-Z0-9_]*\)=.*$/\1/p' list > names
      grep -v -F -w -f names base > $out
      cat list >> $out
    '' else ''
      cat base > $out
    '') + ''
      cat ${./hydo-kvm-procio.config} >> $out
    ''
  );
in
base.overrideAttrs (old: {
  pname = "hydo-kvm-guest-kernel"
    + (if smp then "-smp" else "")
    + (if containers then "-containers" else "")
    + "-io";
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
