#!/bin/sh
# Run a service guest under hvm with its data on /dev/vda (virtio-blk).
#   run.sh solo  <redis|miniflux> <setting>          one boot: workload + invariants
#   run.sh cut   <redis|miniflux> <setting> <dir>    two-boot power cut (the hvx M27 model):
#        boot 1: workload with every write held in the disk's volatile cache until a FLUSH
#                (HVM_FAULT_CLASSES=unflushed, prob 100), stop right after the last acknowledgement;
#                HVM_DISK_OUT keeps only the flushed bytes = what survives a power loss
#        boot 2: a fresh VM on that image, no faults: the service recovers, the acked list from
#                boot 1 is checked
#   setting: redis appendfsync (always|everysec|no); miniflux synchronous_commit (on|off)
# Prints the fingerprints and the verdicts. Run from hvx-mac/ after `sh build.sh`; guests in services/guest/.
set -e
cd "$(dirname "$0")/.."
MODE=$1 SVC=$2 SET=$3 DIR=${4:-/tmp/hvm-svc}
G=services/guest
BA="console=ttyS0 earlycon=uart8250,mmio,0x09000000 clocksource.arm_arch_timer.evtstrm=0 nokaslr rdinit=/init loglevel=4 random.trust_bootloader=on"
case $SVC in
  redis)    INITRD=$G/redis.cpio.gz;    DISK=$G/redis-disk.img; KNOB=hydo_fsync; MEM=1024 ;;
  miniflux) INITRD=$G/miniflux.cpio.gz; DISK=$G/pg-disk.img;    KNOB=hydo_sync;  MEM=2048 ;;
  *) echo "unknown service $SVC" >&2; exit 2 ;;
esac
MARK="hydo-svc-marker: done"
mkdir -p "$DIR"
fp() { grep -o 'stop=[0-9]* exits=[0-9]*\|fp=[0-9a-f]*\|wall=[0-9.]* s' "$1" | tr '\n' ' '; }
verdict() { grep -a 'HYDO-ORACLE' "$1" | tr -d '\r' | head -1; }
if [ "$MODE" = solo ]; then
  HVM_MEM_MB=$MEM HVM_DISK=$DISK HVM_DISK_OUT="$DIR/solo.img" HVM_BOOTARGS="$BA hydo_phase=solo $KNOB=$SET" \
    ./hvm run $G/Image-svc $INITRD "$MARK" > "$DIR/solo.txt" 2>&1 || true
  echo "solo $SVC $SET: $(fp "$DIR/solo.txt")| $(verdict "$DIR/solo.txt")"
  exit 0
fi
HVM_MEM_MB=$MEM HVM_DISK=$DISK HVM_DISK_OUT="$DIR/durable.img" HVM_FAULT=1 HVM_FAULT_CLASSES=unflushed HVM_FAULT_PROB=100 \
  HVM_BOOTARGS="$BA hydo_phase=cut $KNOB=$SET" ./hvm run $G/Image-svc $INITRD "$MARK" > "$DIR/cut.txt" 2>&1 || true
ACKED=$(grep -a '^hydo-acked:' "$DIR/cut.txt" | tr -d '\r' | sed 's/^hydo-acked: //')
HVM_MEM_MB=$MEM HVM_DISK="$DIR/durable.img" HVM_BOOTARGS="$BA hydo_phase=recover $KNOB=$SET hydo_acked=$ACKED" \
  ./hvm run $G/Image-svc $INITRD "$MARK" > "$DIR/recover.txt" 2>&1 || true
echo "cut $SVC $SET: boot1 $(fp "$DIR/cut.txt")| $(grep -o 'writes held.*held [0-9]*)' "$DIR/cut.txt") | durable $(shasum -a 256 "$DIR/durable.img" | cut -c1-16)"
echo "cut $SVC $SET: boot2 $(fp "$DIR/recover.txt")| $(verdict "$DIR/recover.txt")"
