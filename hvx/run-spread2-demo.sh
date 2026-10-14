#!/usr/bin/env bash
#
# SPREAD_ON_HVX Phase 2 demo: TWO co-located `fs`-backed spread brokers sharing ONE rw data disk,
# driven by the FULL mini-Jepsen consistency oracle (spread-drv op-program runner) with a broker
# crash, run DETERMINISTICALLY under hvx — plus seeded hvx DISK faults on the shared data disk.
# Run ON THE HOST after ./dev.sh sync + ./build-spread2.sh + ./hvx/deploy-hvx.sh.
#
#   serve      boot -> two brokers on shared /data -> create q -> op-program (with X 2 crash) ->
#              the oracle maps `spread: consistent`/`BUG-ASSERT` to HYDO-ORACLE: OK|VIOLATION.
#   replay     two fresh boots -> identical whole-boot fingerprint (RAM + rw data disk + console).
#   roundtrip  snapshot->restore in place -> identical fingerprint.
#   branch     snapshot at a pre-initramfs marker, continue twice -> identical fingerprint.
#   fault      seeded hvx DISK DROP fault on the shared data disk: the SAME seed reproduces
#              bit-for-bit (fingerprint + verdict); a different seed can give a different
#              fingerprint (seed-sensitivity, informational). Verdict OK or VIOLATION both fine.
#   all        serve + replay + roundtrip + branch + fault.
#
# hvx-isms: HVX_CPU_V2=1 (glibc userland), HVX_IDLE_HEARTBEAT=1 (brokers park in accept/epoll),
# TOKIO_WORKER_THREADS=1 per broker, hvx_seedrng, randomize_va_space=0. Scoped kills only.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

MEM="${SPREAD_MEM:-768}"
MODE="${1:-all}"                # all | serve | replay | roundtrip | branch | fault
SNAP_DIR="${HVX_SNAP_DIR:-/var/tmp/hvx-snap}"
FAULT_PROB="${SPREAD_FAULT_PROB:-30}"    # seeded DROP probability (%) on the shared data disk
SEED_A="${SPREAD_SEED_A:-0x51a0}"
SEED_B="${SPREAD_SEED_B:-0x51a1}"
CRASH="${SPREAD_CRASH:-0}"        # op-program includes the broker-2 crash (X 2) when 1 (default 0:
                                  # crash=1 stalls the fs-backend survivor on the dead broker's lease)
CMDLINE="root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys spread_crash=$CRASH"

echo ">> staging bzImage + spread2 initramfs + partitioned spread2 disk into L1:/tmp"
L1SSH 'sudo rm -f /tmp/bzImage /tmp/initramfs-spread2.cpio.gz /tmp/spread2-disk.img'
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs-spread2.cpio.gz" \
      "$HOME/hydo/build/spread2-disk.img" dev@localhost:/tmp/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

if [ "$MODE" = "all" ] || [ "$MODE" = "serve" ]; then
  echo ">> 1. BOOT + SERVE: two brokers on shared /data + op-program (expect a HYDO-ORACLE verdict). Timing it."
  L1SSH "cd /tmp; /usr/bin/time -v sudo timeout 1800 env HVX_DISK=/tmp/spread2-disk.img \
         HVX_CPU_V2=1 HVX_IDLE_HEARTBEAT=1 \
         HVX_INPUT='echo hydo-spread-shell\n' HVX_EXPECT='hydo-spread-marker' \
         /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-spread2.cpio.gz \"$CMDLINE\" $MEM \
         >/tmp/hvx_spread2_ok.out 2>/tmp/hvx_spread2_ok.err; echo \"hvx_linux rc=\$?\"; \
         grep -E 'Elapsed|Maximum resident' /tmp/hvx_spread2_ok.err | tail -3"
  L1SSH "grep -aE 'hydo-spread|HYDO-ORACLE:|hydo-spread-marker|switched into|both brokers|BUG-ASSERT|spread: consistent|METRICS|RPT sat' /tmp/hvx_spread2_ok.out | tail -34"
fi

run_verify() {  # $1 = replay | roundtrip | branch | fault
  local vm="$1"
  echo ">> hvx_spread_verify (mode=$vm, file-backed snapshot in $SNAP_DIR)"
  L1SSH "sudo env HVX_SNAP_DIR=$SNAP_DIR HVX_JVM_SEED_A=$SEED_A HVX_JVM_SEED_B=$SEED_B HVX_JVM_PROB=$FAULT_PROB \
         timeout 5400 /tmp/hvx-src/hvx_spread_verify /tmp/bzImage \
         /tmp/initramfs-spread2.cpio.gz /tmp/spread2-disk.img \"$CMDLINE\" $MEM $vm 2>&1 | \
         grep -aiE 'REPLAY|ROUND-TRIP|BRANCH|FAULT|seed-sensitivity|BASELINE|ro-root|verdict|PASS|FAIL|MISMATCH|IDENTICAL|partitions|mode=|oracle' | tail -30"
}
if [ "$MODE" = "all" ] || [ "$MODE" = "replay" ];    then run_verify replay;    fi
if [ "$MODE" = "all" ] || [ "$MODE" = "roundtrip" ]; then run_verify roundtrip; fi
if [ "$MODE" = "all" ] || [ "$MODE" = "branch" ];    then run_verify branch;    fi
if [ "$MODE" = "all" ] || [ "$MODE" = "fault" ];     then run_verify fault;     fi

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops|bad page" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
