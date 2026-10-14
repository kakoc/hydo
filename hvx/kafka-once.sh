#!/usr/bin/env bash
# Run ON THE HOST (via ./dev.sh). One timed Kafka L2 boot via hvx_linux (console streamed
# to L1:/tmp/kboot.out, self-terminates at the marker via HVX_EXPECT), then report the
# broker/oracle lines + wall-time + max RSS. Diagnostic bring-up, not the determinism proof.
#   ./dev.sh "bash ~/hydo/hvx/kafka-once.sh [solo|workload|oracle] [jit]"
set -uo pipefail
K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1() { ssh -p 2222 $K dev@localhost "$@"; }
PHASE="${1:-solo}"
JIT="${2:-${HVX_JIT:-c1sync}}"
FLUSH="${HVX_FLUSH:-off}"
FAULTENV="${KAFKA_FAULTENV:-}"     # e.g. "HVX_FAULT=0x1 HVX_FAULT_CLASSES=32 HVX_FAULT_PROB=100"

echo ">> stage fresh disk+initramfs, kill stale, reload module"
L1 "sudo pkill -f hvx_linux 2>/dev/null; sudo pkill -f hvx_kafka_verify 2>/dev/null; sleep 2; sudo rm -f /tmp/kafka-disk.img /tmp/initramfs-kafka.cpio.gz"
scp -P 2222 $K "$HOME/hydo/build/kafka-disk.img" "$HOME/hydo/build/initramfs-kafka.cpio.gz" \
   "$HOME/hydo/build/bzImage" dev@localhost:/tmp/ >/dev/null 2>&1 || \
scp -P 2222 $K "$HOME/hydo/build/kafka-disk.img" "$HOME/hydo/build/initramfs-kafka.cpio.gz" dev@localhost:/tmp/ >/dev/null
L1 "lsmod | grep -q hvx && sudo rmmod hvx; sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; sudo dmesg -C; sudo insmod /tmp/hvx-src/hvx.ko; echo insmod-rc=\$?"

CMD="root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys hydo_jit=$JIT hydo_phase=$PHASE hydo_flush=$FLUSH"
echo ">> boot phase=$PHASE jit=$JIT flush=$FLUSH fault='$FAULTENV' (timeout 2400s, stops at marker)"
L1 "/usr/bin/time -v sudo timeout 2400 env HVX_DISK=/tmp/kafka-disk.img HVX_CPU_V2=1 HVX_IDLE_HEARTBEAT=1 $FAULTENV HVX_INPUT='echo hydo-kafka-shell\n' HVX_EXPECT='hydo-kafka-marker' /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-kafka.cpio.gz '$CMD' 1280 >/tmp/kboot.out 2>/tmp/kboot.time; echo hvx_linux-rc=\$?; grep -E 'Elapsed|Maximum res' /tmp/kboot.time"
echo "===RESULT==="
L1 "grep -aE 'switched into|loopback up|format rc|broker STARTED|broker DIED|HYDO-KAFKA|HYDO-ORACLE|hydo-kafka-marker|Kafka Server started' /tmp/kboot.out | tail -25"
