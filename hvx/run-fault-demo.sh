#!/usr/bin/env bash
#
# M22 STRETCH runner (run ON THE HOST, after ./dev.sh sync + deploy-hvx.sh build).
# Proves DETERMINISTIC disk fault injection reaches a REAL Linux kernel:
#   1. boot real Linux with the virtio-blk disk attached AND a seeded I/O-error
#      fault policy; show the kernel logs the I/O error (blk_update_request /
#      Buffer I/O error) on /dev/vda;
#   2. run the with-disk determinism harness UNDER the same fault policy twice +
#      branch -- all bit-identical (a NEW reproducible with-fault fingerprint),
#      proving the injected faults are deterministic end-to-end;
#   3. re-check the FAULT-FREE with-disk baseline (36583d533d7c989b) is preserved
#      (fault policy inert when HVX_FAULT is unset).
# Scoped kills only; never touches the host or unrelated qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

DISK_FP="36583d533d7c989b"      # fault-free with-disk baseline (must be preserved)
FAULT_SEED="0x2222"             # any seed; the schedule is a pure function of it

echo ">> staging bzImage + disk-enabled initramfs + disk image into L1:/tmp"
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs-disk.cpio.gz" \
      "$HOME/hydo/build/vblk-disk.img" dev@localhost:/tmp/ >/dev/null || true

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

echo ">> M22 stretch: boot real Linux with an I/O-ERROR fault policy (seed $FAULT_SEED) on /dev/vda"
L1SSH "sudo timeout 200 env HVX_DISK=/tmp/vblk-disk.img HVX_FAULT=$FAULT_SEED \
       HVX_INPUT='echo hydo-vblk-shell\\n' HVX_EXPECT='hydo-vblk-shell' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-disk.cpio.gz \
       >/tmp/hvx_fault.out 2>/tmp/hvx_fault.err; echo \"hvx_linux rc=\$?\""
echo ">> proof: the guest kernel logged a DETERMINISTIC I/O error on /dev/vda"
L1SSH "grep -aiE 'I/O error|Buffer I/O error|blk_update_request|end_request|critical (target|medium)' /tmp/hvx_fault.out | tail -8; \
       echo '---- fault policy line ----'; grep -a 'fault policy' /tmp/hvx_fault.err | tail -2"

echo ">> M22 stretch determinism: two fault-injected boots + branch identical (NEW with-fault fp)"
L1SSH "sudo timeout 480 env HVX_FAULT=$FAULT_SEED /tmp/hvx-src/hvx_disk_boot_verify \
       /tmp/bzImage /tmp/initramfs-disk.cpio.gz /tmp/vblk-disk.img 2>&1 | \
       grep -aiE 'fault policy|whole-boot|branch |PASS|FAIL|MISMATCH' | tail -6"

echo ">> regression: FAULT-FREE with-disk baseline preserved (expect $DISK_FP)"
L1SSH "sudo timeout 480 /tmp/hvx-src/hvx_disk_boot_verify /tmp/bzImage \
       /tmp/initramfs-disk.cpio.gz /tmp/vblk-disk.img 2>&1 | \
       grep -aiE 'whole-boot|branch |PASS|FAIL|MISMATCH|$DISK_FP' | tail -6"

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
