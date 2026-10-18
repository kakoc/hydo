#!/bin/sh
# Two-boot power cut on the virtio-blk disk (the hvx M27 model, on the Mac):
#   boot 1 (cut):     faults armed, write records (the first NSYNC fsync'd), stop = power cut;
#                     HVM_DISK_OUT keeps only the bytes a FLUSH made durable (the volatile cache is lost)
#   boot 2 (recover): a fresh VM on that durable image, no faults: ext4 replays its journal, the oracle
#                     checks that every fsync'd record survived.
# usage: cut-test.sh <out-dir> <n> <nsync> [HVM_FAULT env...]   e.g. HVM_FAULT=1 HVM_FAULT_CLASSES=unflushed
set -e
cd "$(dirname "$0")"
OUT=$1; N=$2; NS=$3; mkdir -p "$OUT"
BA="console=ttyS0 earlycon=uart8250,mmio,0x09000000 clocksource.arm_arch_timer.evtstrm=0 nokaslr rdinit=/init loglevel=7 random.trust_bootloader=on"
HVM_DISK=guest/disk-ext4.img HVM_DISK_OUT="$OUT/durable.img" HVM_BOOTARGS="$BA hydo_phase=cut hydo_n=$N hydo_sync=$NS" \
	./hvm run guest/Image-virtio guest/initramfs-disk.cpio.gz "hydo-disk: power cut here" > "$OUT/cut.txt" 2>&1
env -u HVM_FAULT HVM_DISK="$OUT/durable.img" HVM_BOOTARGS="$BA hydo_phase=recover hydo_n=$N hydo_sync=$NS" \
	./hvm run guest/Image-virtio guest/initramfs-disk.cpio.gz "hydo-disk: oracle complete" > "$OUT/recover.txt" 2>&1 || true
echo "cut:     $(grep -o 'fp=[0-9a-f]*' "$OUT/cut.txt") $(grep -o 'writes held.*held [0-9]*)' "$OUT/cut.txt")"
echo "durable: $(shasum -a 256 "$OUT/durable.img" | cut -c1-16)"
echo "recover: $(grep -o 'fp=[0-9a-f]*' "$OUT/recover.txt") | $(grep -a 'hydo-rec: present' "$OUT/recover.txt" | tr -d '\r') | $(grep -a 'HYDO-ORACLE' "$OUT/recover.txt" | tr -d '\r')"
