#!/usr/bin/env bash
#
# SPREAD_ON_HVX Phase 3 — the autonomous serial-fed snapshot-restore SEARCH LOOP. Boots the
# Phase-2 two-broker guest ONCE to the READY state (both brokers up + queue q + the driver
# blocked on the serial read = SPREAD-READY-FOR-INPUT), snapshots it, then per generated
# op-program RESTORES that snapshot (no reboot), feeds the program over the serial RX
# (HVX_PUT_INPUT, staged before resume), runs to SPREAD-RUN-DONE, and reads the mini-Jepsen
# verdict (spread: consistent / BUG-ASSERT) from the console. Deterministic: same program ->
# identical verdict + fingerprint (the harness proves it on prog#0 before the campaign).
# Run ON THE HOST after ./dev.sh sync + ./build-spread2.sh + ./hvx/deploy-hvx.sh.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -n -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

MEM="${SPREAD_MEM:-768}"
NPROG="${1:-24}"                 # number of generated programs
SEED="${2:-1}"                   # campaign seed (same seed -> same program stream)
FAULT_SEED="${3:-}"             # optional: seeded DROP disk-fault on the shared data disk
FAULT_PROB="${4:-30}"
SNAP_DIR="${HVX_SNAP_DIR:-/var/tmp/hvx-snap}"
SPREAD_MODE="${SPREAD_MODE:-full}"   # set SPREAD_MODE=fuzzonly for hydo-16's brokers-disabled split test
SEARCH_MODE="${HVX_SEARCH_MODE:-guided}"   # guided (corpus+reachability novelty) | random (baseline)
CMDLINE="root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys spread_mode=$SPREAD_MODE"

echo ">> staging bzImage + spread2 initramfs + disk into L1:/tmp"
L1SSH 'sudo rm -f /tmp/bzImage /tmp/initramfs-spread3.cpio.gz /tmp/spread3-disk.img'
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs-spread3.cpio.gz" \
      "$HOME/hydo/build/spread3-disk.img" dev@localhost:/tmp/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

echo ">> search loop: mode=$SEARCH_MODE nprog=$NPROG seed=$SEED fault_seed='${FAULT_SEED:-none}' (boot once to READY, restore per program)"
L1SSH "cd /tmp; sudo env HVX_SNAP_DIR=$SNAP_DIR HVX_SEARCH_MODE=$SEARCH_MODE timeout 3600 /tmp/hvx-src/hvx_spread_search \
       /tmp/bzImage /tmp/initramfs-spread3.cpio.gz /tmp/spread3-disk.img \"$CMDLINE\" $MEM $NPROG $SEED ${FAULT_SEED:-0} $FAULT_PROB 2>&1 | \
       grep -aiE 'search:|restore latency|warm run|DETERMINISM|COMPONENTS|prog#|SUMMARY|PASS|MISMATCH|NOMARKER|READY|MODE|REACHABILITY|markers_reached|SOMETIMES|distinct_combos|deepest_combo|DISTANCE|corpus_size|GUIDED-SUMMARY|BUG-REPRO|BUG-ASSERT|^S |^R |^N |^P |^Z |^U |PROGRAM-(BEGIN|END)' | tail -200"

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops|bad page" | tail -12; \
       echo "---- dmesg last 4 ----"; sudo dmesg | tail -4; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
