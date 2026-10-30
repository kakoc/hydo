#!/usr/bin/env bash
# Build hvx.ko against the debug kernel, the hand-written tests and the reproducers (static),
# and the initramfs of the debug test VM (Milestone 97). Runs where build-kernel.sh ran (the
# bare-metal test machine or the build machine); it only builds. Output: $SAFETY/debug/{hvx.ko,
# initramfs.cpio.gz}, which run-debug.sh boots on the build machine.
set -euo pipefail
SAFETY=${SAFETY:-$HOME/hydo-safety}
HERE="$(cd "$(dirname "$0")" && pwd)"
HVX="$(cd "$HERE/.." && pwd)"
K="$SAFETY/kbuild/linux"
. "$HERE/lib.sh"
# static busybox: the pinned nix one (docs/BUILD-PINS.md); on a machine without nix, a copy of
# it in $SAFETY/bin/busybox (safety.sh fetch-busybox puts it there)
BB=/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox
[ -x "$BB" ] || BB="$SAFETY/bin/busybox"
GCC_MUSL=/nix/store/aa1jsl19ywwfpqp0438vqbncc7sndnyw-gcc-static-x86_64-unknown-linux-musl-13.2.0/bin/gcc
MUSL=/nix/store/0nlqfwsn4qpx1453gb4zvx6dvn3121l4-musl-static-x86_64-unknown-linux-musl-1.2.6
if [ -x "$GCC_MUSL" ]; then CCS="$GCC_MUSL -static -O2 -B $MUSL/lib"; else CCS="gcc -static -O2"; fi
GUEST=${HVX_GUEST_DIR:-$HOME/hydo/build}	# the L2 guest images (bzImage, initramfs*.cpio.gz, vblk-disk.img)
[ -f "$K/.config" ] || { echo "build-debug: run build-kernel.sh first" >&2; exit 1; }
[ -x "$BB" ] || { echo "build-debug: no static busybox ($BB)" >&2; exit 1; }

# The suite of hv/hvx/deploy-metal.sh (hand-written tests, the several-vCPU tests), short
# real-Linux boots under hvx, and the reproducers of the bugs M97 fixed. Tests that need the
# PMU branch clock fail with ENODEV in a nested VM; they still run, which exercises the
# module's refusal paths.
TESTS="hvx_test hvx_rdtsc_test hvx_step_test hvx_tscclock_test hvx_snapshot_test hvx_intr_test
hvx_verify hvx_console_test hvx_bigmem_test hvx_scale_test hvx_cpuid_test hvx_sregs_test
hvx_input_test hvx_smp_test hvx_virtio_iso_test hvx_virtio_pci_test hvx_fault_test hvx_net_test
hvx_net_fault_test hvx_bug_f0_test hvx_fuzzresume_test hvx_pmu_test hvx_bigram_test hvx_avx_test
hvx_sched_pmu_test hvx_sched_race_test hvx_sipi_test hvx_sched_sweep_test hvx_sched_burst_test
hvx_linux hvx_boot_verify hvx_disk_boot_verify"
REPROS="hvx_repro_pageleak hvx_repro_pmu hvx_repro_txlen hvx_repro_unsync hvx_repro_pcishift hvx_repro_diskrace hvx_repro_repins hvx_repro_vqloop"

D="$SAFETY/${DEBUG_OUT:-debug}"	# DEBUG_OUT/HVX_C: build another hvx.c (e.g. the unfixed one) into its own bundle
rm -rf "$D/root"; mkdir -p "$D/root"/{bin,hvx,guest,proc,sys,dev,tmp}

# 1. the module, built against the debug kernel (KASAN/KCOV/UBSAN flags come from its kbuild)
rm -rf "$D/mod"; mkdir -p "$D/mod"
cp "${HVX_C:-$HVX/hvx.c}" "$D/mod/hvx.c"; cp "$HVX/hvx_abi.h" "$HVX/Makefile" "$D/mod/"
echo "make -C '$K' M='$D/mod' modules" > "$D/mod/build.sh"
safety_build "$D/mod/build.sh" "$D/mod/make.log" gnumake flex bison bc elfutils openssl perl
grep -E 'warning:|error:' "$D/mod/make.log" | grep -v 'compiler differs' || true
cp "$D/mod/hvx.ko" "$D/hvx.ko"; cp "$D/mod/hvx.ko" "$D/root/hvx/"

# 2. the tests and the reproducers, static
for t in $TESTS; do
  $CCS -I"$HVX" -o "$D/root/hvx/$t" "$HVX/$t.c" -lm 2> "$D/cc-$t.log" ||
    { echo "build-debug: compile failed: $t"; cat "$D/cc-$t.log"; exit 1; }
done
$CCS -I"$HVX" -o "$D/root/hvx/hvx_repro" "$HERE/repro/hvx_repro.c" -lpthread
echo $TESTS $REPROS > "$D/root/hvx/tests"

# 3. busybox, the init, the L2 guest images
cp "$BB" "$D/root/bin/busybox"
for a in $("$BB" --list); do [ "$a" = busybox ] || ln -sf busybox "$D/root/bin/$a"; done
cp "$HERE/debug-init.sh" "$D/root/init"; chmod 755 "$D/root/init"
cp "$GUEST/bzImage" "$GUEST/initramfs.cpio.gz" "$GUEST/initramfs-disk.cpio.gz" "$GUEST/vblk-disk.img" "$D/root/guest/"
(cd "$D/root" && find . | LC_ALL=C sort | "$BB" cpio -o -H newc 2>/dev/null | gzip -1 > "$D/initramfs.cpio.gz")
cp "$SAFETY/kbuild/bzImage" "$D/bzImage"
echo "build-debug: OK ($(du -h "$D/initramfs.cpio.gz" | cut -f1) initramfs, hvx.ko $(sha256sum "$D/hvx.ko" | cut -c1-16))"
