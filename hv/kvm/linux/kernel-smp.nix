# The SMP build of the KVM engine's guest kernel, for several vCPUs (docs/design/kvm-vcpus.md):
# kernel.nix with hydo-kvm-smp.patch after hydo-kvm-guest.patch, and hydo-kvm.config without
# its CONFIG_SMP and CONFIG_X86_X2APIC lines plus hydo-kvm-smp.config. kernel.nix is not
# changed, so the UP kernel bzImage-kvm keeps its pin.
#
#   nix-build hv/kvm/linux/kernel-smp.nix --arg nixpkgs <pinned nixpkgs>   (SMP=1 build-kernel.sh)
#
# The result: bzImage-kvm-smp, its .config, and vmlinux for audit-rdtsc.sh. Not pinned yet.
{ nixpkgs }:
let
  up = import ./kernel.nix { inherit nixpkgs; };
  pkgs = import nixpkgs {
    system = "x86_64-linux";
    config = { };
    overlays = [ ];
  };
  smpConfig = pkgs.runCommand "hydo-kvm-smp.config" { } ''
    grep -v -E '^CONFIG_(SMP|X86_X2APIC)=' ${./hydo-kvm.config} > $out
    cat ${./hydo-kvm-smp.config} >> $out
  '';
in
up.overrideAttrs (old: {
  pname = "hydo-kvm-guest-kernel-smp";
  patches = old.patches ++ [ ./hydo-kvm-smp.patch ];
  configurePhase = ''
    runHook preConfigure
    make ARCH=x86_64 allnoconfig KCONFIG_ALLCONFIG=${smpConfig}
    for c in $(sed -n 's/^\(CONFIG_[A-Z0-9_]*\)=y$/\1/p' ${smpConfig}); do
      grep -qx "$c=y" .config || { echo "missing $c=y" >&2; exit 1; }
    done
    for c in $(sed -n 's/^\(CONFIG_[A-Z0-9_]*\)=n$/\1/p' ${smpConfig}); do
      ! grep -qx "$c=y" .config || { echo "unexpected $c=y" >&2; exit 1; }
    done
    grep -qx 'CONFIG_NR_CPUS=8' .config || { echo "missing CONFIG_NR_CPUS=8" >&2; exit 1; }
    runHook postConfigure
  '';
  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp arch/x86/boot/bzImage $out/bzImage-kvm-smp
    cp .config $out/bzImage-kvm-smp.config
    cp vmlinux $out/vmlinux
    runHook postInstall
  '';
})
