#!/usr/bin/env bash
#
# M33 Kafka flagship demo (Rung 5/6): a REAL, unmodified single-broker Apache Kafka 3.5.0
# (KRaft) + a producer/consumer client, run DETERMINISTICALLY under hvx over the in-kernel
# 127.0.0.1 loopback, on the M29 ext4 READ-ONLY root (the big JDK + Kafka-libs closure lives
# on the ro-root, EXCLUDED from the snapshot) with the small ext2 rw disk (/dev/vda2) as
# Kafka's log.dirs (the only snapshotted disk + the seeded-fault target). Reproduces Kafka's
# DOCUMENTED single-broker durability window: log.flush.interval.messages=1 SURVIVES a power
# loss, the default (no per-message fsync) LOSES an acked record -- bit-for-bit.
#
# Guest RAM is 1024 MiB (single EPT PD, NO M28 PD fan-out -- the >1 GiB fan-out corrupts a
# real-Linux guest, a separate hvx-core bug). The guest console is streamed to a HOST file so
# progress is visible without a nested ssh into the CPU-saturated L1.
#
# Run ON THE HOST (via ./dev.sh sync + build-kafka.sh + deploy-hvx.sh build). Scoped kills only.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes -o ConnectTimeout=15"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

DISK_FP="36583d533d7c989b"           # fault-free with-disk regression baseline (must be preserved)
MEM="${MEM:-1024}"
MODE="${1:-sanity}"                   # sanity | replay | f1 | f2 | f3 | all
HVX_JIT="${HVX_JIT:-c1sync}"          # deterministic JIT (M32)
SNAP_DIR="${HVX_SNAP_DIR:-/var/tmp/hvx-snap}"
BOOT_TIMEOUT="${BOOT_TIMEOUT:-2400}"
VERIFY_TIMEOUT="${VERIFY_TIMEOUT:-86400}"
CMDLINE="root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys tsc=reliable hydo_jit=$HVX_JIT hydo_phase=solo hydo_flush=off"

echo ">> staging bzImage + kafka initramfs + partitioned kafka disk into L1:/tmp"
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs-kafka.cpio.gz" \
      "$HOME/hydo/build/kafka-disk.img" dev@localhost:/tmp/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

echo ">> 1. SANITY solo boot: real Kafka broker + produce/consume over loopback (JIT=$HVX_JIT, ${MEM} MiB)."
echo "      console streams to L1:/tmp/kboot.out; timed."
L1SSH "cd /tmp; /usr/bin/time -v sudo timeout $BOOT_TIMEOUT env HVX_DISK=/tmp/kafka-disk.img \
       HVX_CPU_V2=1 HVX_IDLE_HEARTBEAT=1 HVX_EXPECT='hydo-kafka-marker' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-kafka.cpio.gz \"$CMDLINE\" $MEM \
       >/tmp/kboot.out 2>/tmp/kboot.time; echo \"hvx_linux rc=\$?\"; \
       grep -E 'Elapsed|Maximum resident' /tmp/kboot.time | tail -3"
L1SSH "grep -aE 'switched into|broker STARTED|broker DIED|HYDO-KAFKA|HYDO-ORACLE|hydo-kafka-marker|Oops|panic' /tmp/kboot.out | tail -18"

echo ">> 2. hvx_kafka_verify (mode=$MODE, JIT=$HVX_JIT, file-backed snapshot in $SNAP_DIR)"
L1SSH "sudo env HVX_JIT=$HVX_JIT HVX_SNAP_DIR=$SNAP_DIR timeout $VERIFY_TIMEOUT /tmp/hvx-src/hvx_kafka_verify \
       /tmp/bzImage /tmp/initramfs-kafka.cpio.gz /tmp/kafka-disk.img \"$CMDLINE\" $MEM $MODE 2>&1 | \
       grep -aiE 'SANITY|REPLAY|F1|F2|F3|BASELINE|ro-root|verdict|PASS|FAIL|MISMATCH|IDENTICAL|VIOLATION|durable|client=|boot[12]|MILESTONE 33' | tail -40"

echo ">> 3. regression: FAULT-FREE with-disk baseline preserved (expect $DISK_FP)"
L1SCP "$HOME/hydo/build/vblk-disk.img" "$HOME/hydo/build/initramfs-disk.cpio.gz" dev@localhost:/tmp/ >/dev/null 2>&1 || true
L1SSH "sudo timeout 600 /tmp/hvx-src/hvx_disk_boot_verify /tmp/bzImage \
       /tmp/initramfs-disk.cpio.gz /tmp/vblk-disk.img 2>&1 | \
       grep -aiE 'whole-boot|branch |PASS|FAIL|MISMATCH|$DISK_FP' | tail -6"

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops|bad page" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
