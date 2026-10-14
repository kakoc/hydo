#!/usr/bin/env bash
#
# M29 ROOTFS demo (Rung 3, R3.4/R3.5): boot real Linux from a REAL ext4 READ-ONLY
# root via switch_root (instead of the in-RAM initramfs), plus a small ext2 rw data
# disk, and prove determinism AT SCALE with a snapshot that captures ONLY guest RAM +
# the small rw disk (the big static ro-root is excluded + hash-checked). Run ON THE
# HOST after ./dev.sh sync + build-linux.sh + deploy-hvx.sh build.
#
#   1. boot real Linux; the tiny initramfs mounts /dev/vda1 ext4 READ-ONLY and
#      switch_roots into it; the real root's /sbin/init mounts /dev/vda2 rw, writes
#      committed records, drops the page cache, and an oracle re-reads them. NO fault
#      -> HYDO-ORACLE: OK.
#   2. boot again under a SEEDED virtio-blk DROP fault on the rw-disk writes ->
#      HYDO-ORACLE: VIOLATION (a committed record is LOST; the root is ro so DROP
#      only ever hits the rw-data writes).
#   3. run hvx_rootfs_verify: the four hydo-verify checks at scale -- REPLAY (2 fresh
#      boots identical), ROUND-TRIP, BRANCH-from-snapshot, and SEED-SENSITIVITY (a
#      different seed loses a different record) -- with the snapshot capturing only
#      RAM + the rw disk and the ro-root asserted static.
#   4. regression: the FAULT-FREE with-disk baseline (36583d533d7c989b) is preserved.
# Scoped kills only; never touches the host or unrelated qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

DISK_FP="36583d533d7c989b"      # fault-free with-disk baseline (must be preserved)
DROP=4                          # HVX_FAULT_DROP bit (1<<2)
SEED="0x2905A0"                 # matches hvx_rootfs_verify SEED_A
PROB=12                         # matches hvx_rootfs_verify FAULT_PROB
CMDLINE="root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"

echo ">> staging bzImage + rootfs initramfs + partitioned rootfs disk into L1:/tmp"
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs-rootfs.cpio.gz" \
      "$HOME/hydo/build/rootfs-disk.img" dev@localhost:/tmp/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

echo ">> 1. NO FAULT: boot real Linux from the ext4 ro-root via switch_root -- expect HYDO-ORACLE: OK"
L1SSH "sudo timeout 700 env HVX_DISK=/tmp/rootfs-disk.img \
       HVX_INPUT='echo hydo-rootfs-shell\n' HVX_EXPECT='hydo-rootfs-marker' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-rootfs.cpio.gz \"$CMDLINE\" 512 \
       >/tmp/hvx_rootfs_ok.out 2>/tmp/hvx_rootfs_ok.err; echo \"hvx_linux rc=\$?\""
L1SSH "grep -aE 'hydo-rootfs:|switch|HYDO-ORACLE:|hydo-rootfs-marker|switched into' /tmp/hvx_rootfs_ok.out | tail -14"

echo ">> 2. SEEDED DROP on the rw disk (seed $SEED prob $PROB%): expect HYDO-ORACLE: VIOLATION"
L1SSH "sudo timeout 700 env HVX_DISK=/tmp/rootfs-disk.img HVX_FAULT=$SEED \
       HVX_FAULT_CLASSES=$DROP HVX_FAULT_PROB=$PROB \
       HVX_INPUT='echo hydo-rootfs-shell\n' HVX_EXPECT='hydo-rootfs-marker' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-rootfs.cpio.gz \"$CMDLINE\" 512 \
       >/tmp/hvx_rootfs_bug.out 2>/tmp/hvx_rootfs_bug.err; echo \"hvx_linux rc=\$?\""
L1SSH "grep -aE 'hydo-rootfs:|HYDO-ORACLE:|hydo-rootfs-marker|fault policy' /tmp/hvx_rootfs_bug.out /tmp/hvx_rootfs_bug.err | tail -12"

echo ">> 3. hvx_rootfs_verify: the four checks at scale (RAM + rw-disk snapshot, ro-root excluded/hash-stable)"
L1SSH "sudo timeout 5400 /tmp/hvx-src/hvx_rootfs_verify /tmp/bzImage \
       /tmp/initramfs-rootfs.cpio.gz /tmp/rootfs-disk.img 2>&1 | \
       grep -aiE 'REPLAY|ROUND-TRIP|BRANCH|SEED-SENS|ro-root|verdict|PASS|FAIL|MISMATCH|partitions' | tail -20"

echo ">> 4. regression: FAULT-FREE with-disk baseline preserved (expect $DISK_FP)"
L1SSH "scp -P 2222 $K \$HOME/hydo/build/vblk-disk.img dev@localhost:/tmp/ >/dev/null 2>&1 || true" 2>/dev/null || true
L1SCP "$HOME/hydo/build/vblk-disk.img" "$HOME/hydo/build/initramfs-disk.cpio.gz" dev@localhost:/tmp/ >/dev/null 2>&1 || true
L1SSH "sudo timeout 600 /tmp/hvx-src/hvx_disk_boot_verify /tmp/bzImage \
       /tmp/initramfs-disk.cpio.gz /tmp/vblk-disk.img 2>&1 | \
       grep -aiE 'whole-boot|branch |PASS|FAIL|MISMATCH|$DISK_FP' | tail -6"

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops|bad page" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
