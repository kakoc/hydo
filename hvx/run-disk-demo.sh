#!/usr/bin/env bash
#
# M21 end-to-end runner (run ON THE HOST, after ./dev.sh sync + build-linux.sh +
# deploy-hvx.sh build). Proves REAL Linux uses the virtio-blk disk over MSI-X:
#   1. boots the NixOS 6.6.35 bzImage + a disk-enabled initramfs with a virtio-blk
#      disk attached (a marker at sector 0), shows /dev/vda was enumerated + READ;
#   2. runs the with-disk determinism harness (two boots + branch bit-identical);
#   3. re-checks the no-disk determinism baseline is preserved (device inert).
# Scoped kills only; never touches the host or unrelated qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

NODISK_FP="640ece2f900fe5b3"    # reproducible no-disk baseline (device inert)
DISK_FP="36583d533d7c989b"      # reproducible with-disk baseline

echo ">> staging bzImage + both initramfs images + disk image into L1:/tmp"
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs.cpio.gz" \
      "$HOME/hydo/build/initramfs-disk.cpio.gz" "$HOME/hydo/build/vblk-disk.img" \
      dev@localhost:/tmp/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

echo ">> M21 real-Linux disk boot: attach /dev/vda (marker at sector 0), read it"
# scripted input so hvx_linux exits cleanly once the shell echoes back after the read
L1SSH "sudo timeout 200 env HVX_DISK=/tmp/vblk-disk.img HVX_INPUT='echo hydo-vblk-shell\\n' \
       HVX_EXPECT='hydo-vblk-shell' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-disk.cpio.gz \
       >/tmp/hvx_disk.out 2>/tmp/hvx_disk.err; echo \"hvx_linux rc=\$?\""
echo ">> proof: guest kernel discovered the PCI disk + read the marker over MSI-X"
L1SSH "grep -aE '1af4:1042|virtio_blk virtio0: \[vda\]|hydo-vblk: /dev/vda present|hydo-vblk-read: HYDO-VBLK-DISK|hydo-disk-marker' /tmp/hvx_disk.out | tail -8"

echo ">> M21 with-disk determinism (expect $DISK_FP; two boots + branch identical)"
L1SSH "sudo timeout 480 /tmp/hvx-src/hvx_disk_boot_verify /tmp/bzImage \
       /tmp/initramfs-disk.cpio.gz /tmp/vblk-disk.img 2>&1 | \
       grep -aiE 'whole-boot|branch |PASS|FAIL|MISMATCH|$DISK_FP' | tail -6"

echo ">> no-disk regression: device inert (expect $NODISK_FP; two boots + branch identical)"
L1SSH "sudo timeout 450 /tmp/hvx-src/hvx_boot_verify /tmp/bzImage /tmp/initramfs.cpio.gz 2>&1 | \
       grep -aiE 'whole-boot|branch |PASS|FAIL|MISMATCH|$NODISK_FP' | tail -6"

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
