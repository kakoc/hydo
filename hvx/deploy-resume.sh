#!/usr/bin/env bash
#
# Lean iterate loop for the deep-snapshot-resume investigation: copy hvx.c + the
# reproducer into L1, build the module + hvx_resume_test only, stage the deep-guest
# artifacts once, load, run a scenario, dump the DBG dmesg. Run ON THE HOST.
#
#   ./deploy-resume.sh                 # build only
#   ./deploy-resume.sh shell           # interactive shell, feed input AFTER restore
#   ./deploy-resume.sh redis [marker]  # redis solo deep resume
#
set -uo pipefail

SRC="${HYDO_HVX_SRC:-$HOME/hydo/hvx}"
BUILD="${HYDO_BUILD:-$HOME/hydo/build}"
K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }
SCEN="${1:-}"

echo ">> copying module + reproducer -> L1:/tmp/hvx-src"
L1SSH 'mkdir -p /tmp/hvx-src'
L1SCP "$SRC/hvx.c" "$SRC/hvx_abi.h" "$SRC/hvx_boot.h" "$SRC/hvx_resume_test.c" \
      "$SRC/Makefile" dev@localhost:/tmp/hvx-src/ >/dev/null

echo ">> building module + hvx_resume_test in L1"
L1SSH 'cd /tmp/hvx-src && make >/tmp/mkmod.log 2>&1; echo "make rc=$?"; \
       cc -O2 -Wall -o hvx_resume_test hvx_resume_test.c; echo "cc rc=$?"'

echo ">> staging deep-guest artifacts into L1:/tmp (once)"
L1SSH 'test -f /tmp/bzImage && test -f /tmp/initramfs.cpio.gz && test -f /tmp/initramfs-redis.cpio.gz && test -f /tmp/redis-disk.img && echo "artifacts present"' \
  || L1SCP "$BUILD/bzImage" "$BUILD/initramfs.cpio.gz" "$BUILD/initramfs-redis.cpio.gz" "$BUILD/redis-disk.img" dev@localhost:/tmp/ >/dev/null

run_it() {
  local args="$1"
  L1SSH "lsmod | grep -q hvx && sudo rmmod hvx; \
         sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
         sudo dmesg -C; \
         sudo insmod /tmp/hvx-src/hvx.ko; echo \"insmod rc=\$?\"; \
         sudo timeout 300 /tmp/hvx-src/hvx_resume_test $args \
              2>&1 | grep -aiE 'booting|snap marker|captured|RESTORED|RESUME|FAIL|fed .* input|console tail' | tail -20; \
         echo \"reproducer rc=\$?\"; \
         echo '---- DBG dmesg ----'; sudo dmesg | grep -aE 'hvx DBG' | tail -40; \
         echo '---- health ----'; sudo dmesg | grep -aiE 'bug|oops|stall|rcu|taint' | tail -6; \
         sudo rmmod hvx; echo \"rmmod rc=\$?\""
}

case "$SCEN" in
  shell)
    echo ">> SCENARIO shell: idle-blocked serial read, feed input AFTER restore"
    run_it "/tmp/bzImage /tmp/initramfs.cpio.gz - '~ #' 'WOKEN123' 'echo WOKEN123\n'"
    ;;
  redis)
    MARKER="${2:-workload SET acked}"
    echo ">> SCENARIO redis: deep resume at '$MARKER'"
    run_it "/tmp/bzImage /tmp/initramfs-redis.cpio.gz /tmp/redis-disk.img '$MARKER' 'hydo-redis-marker: oracle complete'"
    ;;
  "") echo ">> build only (no scenario)";;
  *) echo "unknown scenario '$SCEN' (use: shell | redis)";;
esac
