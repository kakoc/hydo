#!/usr/bin/env bash
#
# M25 FLAGSHIP runner (run ON THE HOST, after ./dev.sh sync + build-linux.sh +
# deploy-hvx.sh build). The payoff of the whole "be your own hypervisor" path:
# find a REAL durability bug in a REAL app (hydokv, a raw-block O_DIRECT KV store
# with an ack-before-fsync bug) via a SEEDED disk fault, and prove the failure
# reproduces bit-for-bit.
#
#   1. boot real Linux running hydokv with NO fault -> the oracle prints
#      HYDO-ORACLE: OK (every committed key is durable; the app is correct);
#   2. boot again with a SEEDED virtio-blk DROP fault (models power loss discarding
#      an un-barriered write) -> the oracle prints HYDO-ORACLE: VIOLATION
#      (a genuinely-committed key was silently LOST);
#   3. run hvx_bug_verify: the four hydo-verify checks on this real bug --
#      F1 no-fault OK + deterministic (2 boots + branch identical),
#      F2 seeded DROP VIOLATION + replay + branch bit-for-bit,
#      F3 a different seed => a different reproducible failure;
#   4. regression: the FAULT-FREE with-disk baseline (36583d533d7c989b) is
#      preserved (the KV VM only attaches a disk + a fault policy; both inert-by-
#      default paths preserve every prior fingerprint).
# Scoped kills only; never touches the host or unrelated qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

DISK_FP="36583d533d7c989b"      # fault-free with-disk baseline (must be preserved)
DROP=4                          # HVX_FAULT_DROP bit
SEED="0xB5EED"                  # matches hvx_bug_verify SEED_A
PROB=25                         # matches hvx_bug_verify FAULT_PROB

echo ">> staging bzImage + KV initramfs + zeroed KV disk into L1:/tmp"
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs-kv.cpio.gz" \
      "$HOME/hydo/build/kv-disk.img" dev@localhost:/tmp/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

echo ">> 1. NO FAULT: boot real Linux running hydokv -- expect HYDO-ORACLE: OK"
L1SSH "sudo timeout 200 env HVX_DISK=/tmp/kv-disk.img \
       HVX_INPUT='echo hydo-kv-shell\\n' HVX_EXPECT='hydo-flagship-marker' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-kv.cpio.gz \
       >/tmp/hvx_kv_ok.out 2>/tmp/hvx_kv_ok.err; echo \"hvx_linux rc=\$?\""
L1SSH "grep -aE 'hydokv:|HYDO-ORACLE:|hydo-flagship-marker' /tmp/hvx_kv_ok.out | tail -8"

echo ">> 2. SEEDED DROP (seed $SEED prob $PROB%): expect HYDO-ORACLE: VIOLATION (a committed key lost)"
L1SSH "sudo timeout 200 env HVX_DISK=/tmp/kv-disk.img HVX_FAULT=$SEED \
       HVX_FAULT_CLASSES=$DROP HVX_FAULT_PROB=$PROB \
       HVX_INPUT='echo hydo-kv-shell\\n' HVX_EXPECT='hydo-flagship-marker' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-kv.cpio.gz \
       >/tmp/hvx_kv_bug.out 2>/tmp/hvx_kv_bug.err; echo \"hvx_linux rc=\$?\""
L1SSH "grep -aE 'hydokv:|HYDO-ORACLE:|hydo-flagship-marker|fault policy' /tmp/hvx_kv_bug.out /tmp/hvx_kv_bug.err | tail -10"

echo ">> 3. hvx_bug_verify: the four checks (F1 OK+deterministic, F2 VIOLATION+replay+branch, F3 seed-sensitive)"
L1SSH "sudo timeout 1500 /tmp/hvx-src/hvx_bug_verify /tmp/bzImage \
       /tmp/initramfs-kv.cpio.gz /tmp/kv-disk.img 2>&1 | \
       grep -aiE 'F1 |F2 |F3 |verdict=|PASS|FAIL|MISMATCH' | tail -20"

echo ">> 4. regression: FAULT-FREE with-disk baseline preserved (expect $DISK_FP)"
L1SSH "test -f /tmp/vblk-disk.img || (scp -P 2222 $K $HOME/hydo/build/vblk-disk.img dev@localhost:/tmp/ >/dev/null 2>&1); \
       scp -P 2222 $K $HOME/hydo/build/initramfs-disk.cpio.gz dev@localhost:/tmp/ >/dev/null 2>&1 || true"
L1SSH "sudo timeout 480 /tmp/hvx-src/hvx_disk_boot_verify /tmp/bzImage \
       /tmp/initramfs-disk.cpio.gz /tmp/vblk-disk.img 2>&1 | \
       grep -aiE 'whole-boot|branch |PASS|FAIL|MISMATCH|$DISK_FP' | tail -6"

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
