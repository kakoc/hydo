#!/usr/bin/env bash
#
# M27 FLAGSHIP-ON-UNMODIFIED-REDIS runner (run ON THE HOST, after ./dev.sh sync +
# build-linux.sh + deploy-hvx.sh build). The SUT is an UNMODIFIED, off-the-shelf static
# `redis-server` + `redis-cli` CO-LOCATED in ONE L2 guest, talking over the in-kernel
# 127.0.0.1 LOOPBACK (loopback-first: no virtio-net). Persistence is the AOF on a REAL
# ext2 fs on /dev/vda.
#
#   1. SOLO boot: real Linux brings up lo, mounts ext2, starts redis-server, drives 32
#      committed SETs over 127.0.0.1 via redis-cli, then an oracle re-reads them. NO crash
#      -> HYDO-ORACLE: OK (redis + loopback client<->server + AOF all work, deterministic).
#   2. hvx_redis_verify: the hydo-verify checks on Redis's DOCUMENTED AOF durability window:
#      SANITY everysec no-crash OK; F1 always survives a faithful volatile-cache power loss
#      (OK, deterministic replay); F2 everysec LOSES a committed key under the SAME power
#      loss (VIOLATION, replay + branch bit-for-bit) -- F1-vs-F2 IS the always/everysec
#      contrast; F3 a seeded lying-disk DROP loses different keys per seed.
#   3. regression: the FAULT-FREE with-disk baseline (36583d533d7c989b) is preserved.
# Scoped kills only; never touches the host or unrelated qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

DISK_FP="36583d533d7c989b"      # fault-free with-disk baseline (must be preserved)
CMDBASE="console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"

echo ">> staging bzImage + redis initramfs + reproducible ext2 disk into L1:/tmp"
L1SCP "$HOME/hydo/build/initramfs-redis.cpio.gz" "$HOME/hydo/build/redis-disk.img" \
      dev@localhost:/tmp/ >/dev/null
L1SCP "$HOME/hydo/build/bzImage" dev@localhost:/tmp/bzImage-redis >/dev/null 2>&1 || true

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

BZ="/tmp/bzImage"; L1SSH 'test -f /tmp/bzImage' || BZ="/tmp/bzImage-redis"

echo ">> 1. SOLO boot: unmodified redis, client<->server over 127.0.0.1 -- expect HYDO-ORACLE: OK"
L1SSH "sudo timeout 900 env HVX_IDLE_HEARTBEAT=1 HVX_DISK=/tmp/redis-disk.img \
       /tmp/hvx-src/hvx_linux $BZ /tmp/initramfs-redis.cpio.gz \
       \"$CMDBASE hydo_phase=solo hydo_fsync=everysec\" \
       >/tmp/hvx_redis_solo.out 2>&1; echo \"hvx_linux rc=\$?\""
L1SSH "grep -aE 'hydo-redis:|HYDO-ORACLE:|redis ready|redis NOT' /tmp/hvx_redis_solo.out | tail -12"

echo ">> 2. hvx_redis_verify: the four checks (SANITY OK; F1 always survives + deterministic; F2 everysec VIOLATION + replay + branch; F3 seed-sensitive)"
L1SSH "sudo timeout 7200 /tmp/hvx-src/hvx_redis_verify $BZ \
       /tmp/initramfs-redis.cpio.gz /tmp/redis-disk.img 2>&1 | \
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
