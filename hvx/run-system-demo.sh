#!/usr/bin/env bash
#
# M30 MULTI-SERVICE SYSTEM runner (Rung 4) -- run ON THE HOST after ./dev.sh sync +
# build-linux.sh + deploy-hvx.sh build. TWO real services are co-located in ONE L2 guest,
# driven by a client, all over the in-kernel 127.0.0.1 LOOPBACK:
#
#     client (hydoapp load/verify)  --loopback:8080-->  hydoapp server (app tier)
#                                                       --loopback:6379-->  redis-server
#
#   1. SOLO boot: real Linux brings up lo, mounts ext2, starts redis + the app tier, the
#      client commits 48 values THROUGH the app, then re-verifies the aggregate through the
#      app. NO crash -> HYDO-ORACLE: OK (the whole system is correct + deterministic).
#   2. hvx_system_verify: the hydo-verify checks on the SYSTEM-LEVEL invariant (the app's
#      recomputed aggregate == the client's committed aggregate): SANITY OK; F1 always
#      survives a faithful volatile-cache power loss (OK, deterministic replay); F2 everysec
#      VIOLATES the invariant under the SAME power loss -- the app disagrees with the client
#      (replay + branch bit-for-bit); F3 a seeded lying-disk DROP breaks it differently per
#      seed. F1-vs-F2 IS the always/everysec contrast at the system level.
#   3. regression: the FAULT-FREE with-disk baseline (36583d533d7c989b) is preserved.
# Scoped kills only; never touches the host or unrelated qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

DISK_FP="36583d533d7c989b"      # fault-free with-disk baseline (must be preserved)
CMDBASE="console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"

echo ">> staging bzImage + system initramfs + reproducible ext2 disk into L1:/tmp"
L1SCP "$HOME/hydo/build/initramfs-system.cpio.gz" "$HOME/hydo/build/system-disk.img" \
      dev@localhost:/tmp/ >/dev/null
L1SCP "$HOME/hydo/build/bzImage" dev@localhost:/tmp/bzImage-system >/dev/null 2>&1 || true

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

BZ="/tmp/bzImage"; L1SSH 'test -f /tmp/bzImage' || BZ="/tmp/bzImage-system"

echo ">> 1. SOLO boot: client -> app -> redis over 127.0.0.1 -- expect HYDO-ORACLE: OK"
L1SSH "sudo timeout 1200 env HVX_IDLE_HEARTBEAT=1 HVX_DISK=/tmp/system-disk.img \
       /tmp/hvx-src/hvx_linux $BZ /tmp/initramfs-system.cpio.gz \
       \"$CMDBASE hydo_phase=solo hydo_fsync=everysec\" \
       >/tmp/hvx_system_solo.out 2>&1; echo \"hvx_linux rc=\$?\""
L1SSH "grep -aE 'hydo-sys:|HYDO-ORACLE:|client (LOAD|VERIFY)|ready' /tmp/hvx_system_solo.out | tail -16"

echo ">> 2. hvx_system_verify: the four checks (SANITY OK; F1 always survives + deterministic; F2 everysec VIOLATION + replay + branch; F3 seed-sensitive)"
L1SSH "sudo timeout 10800 /tmp/hvx-src/hvx_system_verify $BZ \
       /tmp/initramfs-system.cpio.gz /tmp/system-disk.img 2>&1 | \
       grep -aiE 'SANITY|F1 |F2 |F3 |verdict=|PASS|FAIL|MISMATCH' | tail -24"

echo ">> 3. regression: FAULT-FREE with-disk baseline preserved (expect $DISK_FP)"
L1SSH "scp -P 2222 $K $HOME/hydo/build/vblk-disk.img dev@localhost:/tmp/ >/dev/null 2>&1 || true; \
       scp -P 2222 $K $HOME/hydo/build/initramfs-disk.cpio.gz dev@localhost:/tmp/ >/dev/null 2>&1 || true"
L1SSH "sudo timeout 480 /tmp/hvx-src/hvx_disk_boot_verify $BZ \
       /tmp/initramfs-disk.cpio.gz /tmp/vblk-disk.img 2>&1 | \
       grep -aiE 'whole-boot|branch |PASS|FAIL|MISMATCH|$DISK_FP' | tail -6"

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
