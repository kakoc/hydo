#!/usr/bin/env bash
# Build hvx.ko against the debug kernel, the hand-written tests (static musl), and the
# initramfs of the debug test VM (Milestone 97). Runs ON THE BUILD MACHINE BUILD BOX after
# build-kernel.sh. Output: $SAFETY/debug/{hvx.ko,initramfs.cpio.gz}.
set -euo pipefail
SAFETY=${SAFETY:-$HOME/hydo-safety}
HERE="$(cd "$(dirname "$0")" && pwd)"
HVX="$(cd "$HERE/.." && pwd)"
K="$SAFETY/kbuild/linux"
NIXPKGS=/nix/store/f0ddmw6s86y567yg06h5019z72szbzch-source
GCC_MUSL=/nix/store/aa1jsl19ywwfpqp0438vqbncc7sndnyw-gcc-static-x86_64-unknown-linux-musl-13.2.0/bin/gcc
MUSL=/nix/store/0nlqfwsn4qpx1453gb4zvx6dvn3121l4-musl-static-x86_64-unknown-linux-musl-1.2.6
BB=/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox
GUEST=${HVX_GUEST_DIR:-$HOME/hydo/build}	# the L2 guest images (bzImage, initramfs*.cpio.gz, vblk-disk.img)
[ -f "$K/.config" ] || { echo "build-debug: run build-kernel.sh first" >&2; exit 1; }

# The suite of hvx/deploy-metal.sh (hand-written tests, the several-vCPU tests) and short
# real-Linux boots under hvx. Tests that need the PMU branch clock fail with ENODEV in a
# nested VM; they still run, which exercises the module's refusal paths.
TESTS="hvx_test hvx_rdtsc_test hvx_step_test hvx_tscclock_test hvx_snapshot_test hvx_intr_test
hvx_verify hvx_console_test hvx_bigmem_test hvx_scale_test hvx_cpuid_test hvx_sregs_test
hvx_input_test hvx_smp_test hvx_virtio_iso_test hvx_virtio_pci_test hvx_fault_test hvx_net_test
hvx_net_fault_test hvx_bug_f0_test hvx_fuzzresume_test hvx_pmu_test hvx_bigram_test hvx_avx_test
hvx_sched_pmu_test hvx_sched_race_test hvx_sipi_test hvx_sched_sweep_test hvx_sched_burst_test
hvx_linux hvx_boot_verify hvx_disk_boot_verify"
REPROS=$(cd "$HERE/repro" 2>/dev/null && ls *.c 2>/dev/null | sed 's/\.c$//' || true)

D="$SAFETY/debug"
rm -rf "$D/root"; mkdir -p "$D/root"/{bin,hvx,guest,proc,sys,dev,tmp}

# 1. the module, built against the debug kernel (KASAN/KCOV/UBSAN flags come from its kbuild)
rm -rf "$D/mod"; mkdir -p "$D/mod"
cp "$HVX/hvx.c" "$HVX/hvx_abi.h" "$HVX/Makefile" "$D/mod/"
nix-shell -I nixpkgs="$NIXPKGS" -p gnumake flex bison bc elfutils openssl perl --run "
  export NIX_HARDENING_ENABLE=''
  make -s -C '$K' M='$D/mod' modules 2>&1 | grep -v '^$' || true"
[ -f "$D/mod/hvx.ko" ] || { echo "build-debug: hvx.ko did not build" >&2; exit 1; }
cp "$D/mod/hvx.ko" "$D/hvx.ko"; cp "$D/mod/hvx.ko" "$D/root/hvx/"

# 2. the tests and the reproducers, static musl
for t in $TESTS; do
  "$GCC_MUSL" -static -O2 -B "$MUSL/lib" -I"$HVX" -o "$D/root/hvx/$t" "$HVX/$t.c" 2> "$D/cc-$t.log" ||
    { echo "build-debug: compile failed: $t"; cat "$D/cc-$t.log"; exit 1; }
done
for r in $REPROS; do
  "$GCC_MUSL" -static -O2 -B "$MUSL/lib" -I"$HVX" -lpthread -o "$D/root/hvx/$r" "$HERE/repro/$r.c" -lpthread
done
echo $TESTS > "$D/root/hvx/tests"
echo $REPROS > "$D/root/hvx/repros"

# 3. busybox, the init, the L2 guest images
cp "$BB" "$D/root/bin/busybox"
for a in $("$BB" --list); do [ "$a" = busybox ] || ln -sf busybox "$D/root/bin/$a"; done
cp "$HERE/debug-init.sh" "$D/root/init"; chmod 755 "$D/root/init"
cp "$GUEST/bzImage" "$GUEST/initramfs.cpio.gz" "$GUEST/initramfs-disk.cpio.gz" "$GUEST/vblk-disk.img" "$D/root/guest/"
(cd "$D/root" && find . | LC_ALL=C sort | "$BB" cpio -o -H newc 2>/dev/null | gzip -1 > "$D/initramfs.cpio.gz")
echo "build-debug: OK ($(du -h "$D/initramfs.cpio.gz" | cut -f1) initramfs, hvx.ko $(sha256sum "$D/hvx.ko" | cut -c1-16))"
