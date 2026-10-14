#!/usr/bin/env bash
#
# SPREAD_ON_HVX Phase 1 demo: run ONE unmodified `spread` queue broker (a dynamically-
# linked Rust binary, glibc nix closure) on the `fs` backend DETERMINISTICALLY under hvx,
# on the M31-style ext4 READ-ONLY root (the spread closure lives on the ro-root, EXCLUDED
# from the snapshot) with a small ext2 rw data disk as the fs backend --data-dir. The
# broker is driven by an in-guest static-Go HTTP driver+oracle (spread-drv). Run ON THE
# HOST after ./dev.sh sync + ./build-linux.sh + ./hvx/deploy-hvx.sh build.
#
#   1. BOOT + SERVE: boot real Linux; the tiny initramfs mounts /dev/vda1 ext4 READ-ONLY
#      and switch_roots into it; the real root's /sbin/init seeds the CRNG (hvx_seedrng),
#      brings up loopback, mounts /dev/vda2 rw, starts ONE spread broker on 127.0.0.1:8080,
#      polls /healthz, and drives a single-broker smoke workload (create queue, send 2
#      ordered messages, receive+delete, check FIFO) -> HYDO-ORACLE: OK.
#   2. REPLAY DETERMINISM: hvx_spread_verify mode=replay boots the SAME disk twice and
#      fingerprints the whole end state (guest RAM + rw data disk + console); the two
#      fingerprints must be IDENTICAL (the big ro-root closure is excluded + hash-stable).
#
# Mandatory hvx-isms: HVX_CPU_V2=1 (glibc userland needs x86-64-v2), HVX_IDLE_HEARTBEAT=1
# (broker parks in accept/epoll on loopback). Scoped kills only; never touches the host or
# the sibling `repeat` qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

MEM="${SPREAD_MEM:-512}"
MODE="${1:-all}"                # all | serve | replay | roundtrip | branch
# NOTE: the harness's `seed` (seeded-DROP) mode is JVM-key-oracle-specific (it asserts a lost
# committed KEY FILE), which does not map to spread's queue semantics — so `all` runs
# serve+replay+roundtrip+branch and deliberately SKIPS seed. Phase-2 adds a spread fault oracle.
SNAP_DIR="${HVX_SNAP_DIR:-/var/tmp/hvx-snap}"
CMDLINE="root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"

echo ">> staging bzImage + spread initramfs + partitioned spread disk into L1:/tmp"
# a prior sudo run can leave these root-owned in /tmp -> a plain scp overwrite is denied;
# clear them first (as the dev user via sudo) so staging always refreshes the artifacts.
L1SSH 'sudo rm -f /tmp/bzImage /tmp/initramfs-spread.cpio.gz /tmp/spread-disk.img'
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs-spread.cpio.gz" \
      "$HOME/hydo/build/spread-disk.img" dev@localhost:/tmp/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

if [ "$MODE" = "all" ] || [ "$MODE" = "serve" ]; then
  echo ">> 1. BOOT + SERVE: boot real Linux -> switch_root -> spread broker (expect HYDO-ORACLE: OK). Timing it."
  L1SSH "cd /tmp; /usr/bin/time -v sudo timeout 1200 env HVX_DISK=/tmp/spread-disk.img \
         HVX_CPU_V2=1 HVX_IDLE_HEARTBEAT=1 \
         HVX_INPUT='echo hydo-spread-shell\n' HVX_EXPECT='hydo-spread-marker' \
         /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-spread.cpio.gz \"$CMDLINE\" $MEM \
         >/tmp/hvx_spread_ok.out 2>/tmp/hvx_spread_ok.err; echo \"hvx_linux rc=\$?\"; \
         grep -E 'Elapsed|Maximum resident' /tmp/hvx_spread_ok.err | tail -3"
  L1SSH "grep -aE 'hydo-spread|HYDO-ORACLE:|hydo-spread-marker|switched into|broker healthy' /tmp/hvx_spread_ok.out | tail -20"
fi

run_verify() {  # $1 = replay | roundtrip | branch
  local vm="$1"
  echo ">> hvx_spread_verify (mode=$vm, file-backed snapshot in $SNAP_DIR)"
  L1SSH "sudo env HVX_SNAP_DIR=$SNAP_DIR timeout 3600 /tmp/hvx-src/hvx_spread_verify /tmp/bzImage \
         /tmp/initramfs-spread.cpio.gz /tmp/spread-disk.img \"$CMDLINE\" $MEM $vm 2>&1 | \
         grep -aiE 'REPLAY|ROUND-TRIP|BRANCH|BASELINE|ro-root|verdict|PASS|FAIL|MISMATCH|IDENTICAL|partitions|mode=|oracle' | tail -26"
}
if [ "$MODE" = "all" ] || [ "$MODE" = "replay" ];    then run_verify replay;    fi
if [ "$MODE" = "all" ] || [ "$MODE" = "roundtrip" ]; then run_verify roundtrip; fi
if [ "$MODE" = "all" ] || [ "$MODE" = "branch" ];    then run_verify branch;    fi

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops|bad page" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
