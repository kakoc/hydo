#!/usr/bin/env bash
#
# M31 JVM demo (Rung 5): run a REAL, unmodified OpenJDK JRE + a real multi-threaded
# Java workload DETERMINISTICALLY under hvx, on the M29 ext4 READ-ONLY root (the big
# JRE closure lives on the ro-root, EXCLUDED from the snapshot) with a small ext2 rw
# data disk as the durability-fault target. Run ON THE HOST after ./dev.sh sync +
# build-linux.sh + deploy-hvx.sh build.
#
#   1. boot real Linux; the tiny initramfs mounts /dev/vda1 ext4 READ-ONLY and
#      switch_roots into it; the real root's /sbin/init seeds the CRNG (M27), mounts
#      /dev/vda2 rw, runs a REAL JVM (-Xint SerialGC fixed-heap), writes committed
#      records, drops the page cache, and an oracle re-reads them. NO fault ->
#      HYDO-ORACLE: OK.
#   2. run hvx_jvm_verify: the four hydo-verify checks -- REPLAY (2 fresh JVM boots
#      bit-identical), ROUND-TRIP, BRANCH-from-snapshot, SEED-SENSITIVITY (a seeded
#      DROP on the rw disk loses a committed record differently per seed) -- with the
#      snapshot capturing only RAM + the rw disk and the ro-root hash-stable.
#   3. regression: the FAULT-FREE with-disk baseline (36583d533d7c989b) is preserved.
# Scoped kills only; never touches the host or unrelated qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

DISK_FP="36583d533d7c989b"      # fault-free with-disk baseline (must be preserved)
MEM=640
MODE="${1:-all}"                # all | replay | roundtrip | branch | seed
# M32 Part 1: select the HotSpot compiler via the kernel cmdline token hydo_jit=<config>.
# The JIT is now proven bit-for-bit DETERMINISTIC under hvx (replay + branch), so we
# default to c1sync (C1-only, ONE synchronous compiler) -- deterministic AND ~fastest.
# Per-config baseline fingerprints on build/jvm-disk.img (the JIT-selectable disk):
#   int=8744ce0aa8977316  c1=36d2be4980583ffc  c1sync=393fc95c9dc02118
#   tiered=0019feb57d72c15d  tieredsync=bb852900661c1b32   (each replay+branch IDENTICAL)
# (The M31 -Xint baseline 4b8ffacde50f1169 lives on the pristine build/jvm-disk-m31.img.)
HVX_JIT="${HVX_JIT:-c1sync}"
# M32 Part 2: the verify snapshot double-buffer is FILE-backed on a real disk path in L1
# (/var/tmp is ext4 on /dev/vda1 here, NOT tmpfs), so a bigger guest snapshot fits without
# pressing L1 RAM. HVX_SNAP_MALLOC=1 restores the old in-RAM buffer for an A/B RAM check.
SNAP_DIR="${HVX_SNAP_DIR:-/var/tmp/hvx-snap}"
CMDLINE="root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys hydo_jit=$HVX_JIT"

echo ">> staging bzImage + jvm initramfs + partitioned jvm disk into L1:/tmp"
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs-jvm.cpio.gz" \
      "$HOME/hydo/build/jvm-disk.img" dev@localhost:/tmp/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

echo ">> 1. NO FAULT: boot real Linux -> switch_root -> run the JVM with JIT=$HVX_JIT (expect HYDO-ORACLE: OK). Timing it."
L1SSH "cd /tmp; /usr/bin/time -v sudo timeout 2400 env HVX_DISK=/tmp/jvm-disk.img \
       HVX_CPU_V2=1 HVX_IDLE_HEARTBEAT=1 \
       HVX_INPUT='echo hydo-jvm-shell\n' HVX_EXPECT='hydo-jvm-marker' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-jvm.cpio.gz \"$CMDLINE\" $MEM \
       >/tmp/hvx_jvm_ok.out 2>/tmp/hvx_jvm_ok.err; echo \"hvx_linux rc=\$?\"; \
       grep -E 'Elapsed|Maximum resident' /tmp/hvx_jvm_ok.err | tail -3"
L1SSH "grep -aE 'hydo-jvm|HYDO-JVM|HYDO-ORACLE:|hydo-jvm-marker|switched into' /tmp/hvx_jvm_ok.out | tail -18"

echo ">> 2. hvx_jvm_verify (mode=$MODE, JIT=$HVX_JIT, file-backed snapshot in $SNAP_DIR): the four checks"
L1SSH "sudo env HVX_SNAP_DIR=$SNAP_DIR timeout 21600 /tmp/hvx-src/hvx_jvm_verify /tmp/bzImage \
       /tmp/initramfs-jvm.cpio.gz /tmp/jvm-disk.img \"$CMDLINE\" $MEM $MODE 2>&1 | \
       grep -aiE 'REPLAY|ROUND-TRIP|BRANCH|SEED-SENS|BASELINE|ro-root|verdict|PASS|FAIL|MISMATCH|partitions|mode=|snapshot save' | tail -26"

echo ">> 3. regression: FAULT-FREE with-disk baseline preserved (expect $DISK_FP)"
L1SCP "$HOME/hydo/build/vblk-disk.img" "$HOME/hydo/build/initramfs-disk.cpio.gz" dev@localhost:/tmp/ >/dev/null 2>&1 || true
L1SSH "sudo timeout 600 /tmp/hvx-src/hvx_disk_boot_verify /tmp/bzImage \
       /tmp/initramfs-disk.cpio.gz /tmp/vblk-disk.img 2>&1 | \
       grep -aiE 'whole-boot|branch |PASS|FAIL|MISMATCH|$DISK_FP' | tail -6"

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops|bad page" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
