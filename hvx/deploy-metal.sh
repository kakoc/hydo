#!/usr/bin/env bash
#
# deploy-metal.sh -- regression runner for the dedicated bare-metal test machine (run FROM THE MAC).
# hvx runs directly on that host (no nested L1), so this replaces deploy-hvx.sh there.
#
#   ./deploy-metal.sh              sync userspace sources, compile them on the box, run the
#                                  hand-written suite + both boot verifies against the module
#                                  that is ALREADY loaded, print PASS/FAIL per baseline
#   ./deploy-metal.sh load         also sync hvx.c/hvx_abi.h/Makefile and rebuild + reload the
#                                  module first (via ~/bin/hvx-reload: exclusive module lock)
#   ./deploy-metal.sh flagships    also run the real-service verifies (hydokv, SQLite, Redis,
#                                  rootfs, system, JVM, Kafka, MySQL) and print their results
#   (options combine: ./deploy-metal.sh load flagships)
#
# Every command that opens /dev/hvx runs under ~/bin/hvx-run (shared flock), so a concurrent
# module reload waits for running VMs instead of killing them. Guest images are expected in
# ~/hydo/build on the box (built on the build machine, shipped with rsync).
# Env: HVX_METAL (the bare-metal machine).
set -euo pipefail

BOX="${HVX_METAL:?}"
HERE="$(cd "$(dirname "$0")" && pwd)"
LOAD=0; FLAG=0
for a in "$@"; do
  case "$a" in
    load) LOAD=1;;
    flagships) FLAG=1;;
    *) echo "usage: $0 [load] [flagships]" >&2; exit 2;;
  esac
done

TESTS="hvx_test hvx_rdtsc_test hvx_step_test hvx_tscclock_test hvx_snapshot_test hvx_intr_test
hvx_verify hvx_console_test hvx_bigmem_test hvx_scale_test hvx_cpuid_test hvx_sregs_test
hvx_input_test hvx_smp_test hvx_virtio_iso_test hvx_virtio_pci_test hvx_fault_test hvx_net_test
hvx_net_fault_test hvx_bug_f0_test hvx_fuzzresume_test hvx_pmu_test hvx_bigram_test"
HARNESSES="hvx_boot_verify hvx_disk_boot_verify hvx_bug_verify hvx_sqlite_verify hvx_redis_verify
hvx_rootfs_verify hvx_system_verify hvx_jvm_verify hvx_kafka_verify hvx_mysql_verify"

# Explicit file list: userspace sources only. hvx.c/hvx_abi.h go over only with `load`,
# so a plain run never overwrites a newer module source that is being worked on on the box.
FILES="hvx_boot.h"
for t in $TESTS $HARNESSES; do FILES="$FILES $t.c"; done
[ $LOAD = 1 ] && FILES="$FILES hvx.c hvx_abi.h Makefile"
echo ">> syncing to $BOX:hydo/hvx/ ($([ $LOAD = 1 ] && echo 'with' || echo 'without') module sources)"
(cd "$HERE" && rsync -az $FILES "$BOX:hydo/hvx/")

ssh -o BatchMode=yes "$BOX" "LOAD=$LOAD FLAG=$FLAG TESTS='$TESTS' HARNESSES='$HARNESSES' bash -s" <<'REMOTE'
set -u
cd ~/hydo/hvx
B=~/hydo/build
R=~/bin/hvx-run
K="root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"
[ -f ~/hydo/COORD.md ] && { echo ">> COORD.md:"; cat ~/hydo/COORD.md; }

if [ "$LOAD" = 1 ]; then
  echo ">> rebuilding + reloading the module"
  ~/bin/hvx-reload || { echo "FAIL: module reload"; exit 1; }
fi
lsmod | grep -q '^hvx' || { echo "FAIL: hvx module is not loaded (run with 'load')"; exit 1; }

echo ">> compiling userspace tests + harnesses"
cfail=0
for t in $TESTS $HARNESSES; do
  # build to a temp name and rename: a harness that is running right now keeps its old
  # inode instead of failing the build with "Text file busy"
  if cc -O2 -Wall -o "$t.new" "$t.c" 2> "/tmp/cc-$t.log"; then mv -f "$t.new" "$t"
  else echo "  COMPILE FAIL: $t"; cat "/tmp/cc-$t.log"; cfail=1; fi
done

# test -> fingerprints that must ALL appear in its output (empty = rc only)
declare -A EXP=(
  [hvx_virtio_iso_test]="7e586473f6e494d7"
  [hvx_virtio_pci_test]="3f95ef833747f3ec"
  [hvx_fault_test]="91170d4dbf101c5b 88f680fc22cc9bb0 9090286f3fc59682"
  [hvx_net_test]="0e2b7acf2f4f94ee"
  [hvx_net_fault_test]="883a00eb6a14a6f0 37490fb78da33064 40659aabb208aaa0"
  [hvx_bug_f0_test]="1083e400458728ff"
  [hvx_smp_test]="27ead8c880ef7820 88ee51bd9f8e40e0 d042bb01201c269d"
  [hvx_pmu_test]="86c4afcd0a799450"
)
pass=0; fail=0; failed=""
check() {   # name rc output expected...
  local name=$1 rc=$2 out=$3; shift 3
  local missing=""
  for fp in "$@"; do grep -q "$fp" <<< "$out" || missing="$missing $fp"; done
  local fps; fps=$(grep -aoE '\b[0-9a-f]{16}\b' <<< "$out" | sort -u | tr '\n' ' ' | cut -c1-120)
  if [ "$rc" = 0 ] && [ -z "$missing" ]; then
    pass=$((pass+1)); printf '  PASS %-24s %s\n' "$name" "$fps"
  else
    fail=$((fail+1)); failed="$failed $name"
    printf '  FAIL %-24s rc=%s missing:%s got: %s\n' "$name" "$rc" "${missing:- none}" "$fps"
  fi
}

echo ">> hand-written suite"
for t in $TESTS; do
  out=$($R sudo timeout 150 ./$t 2>&1); rc=$?
  check "$t" "$rc" "$out" ${EXP[$t]:-}
done

echo ">> boot verifies"
out=$($R sudo timeout 300 ./hvx_boot_verify $B/bzImage $B/initramfs.cpio.gz 2>&1); rc=$?
check "boot (no disk)" "$rc" "$(grep -a 'fp1\|fp_a' <<< "$out")" 7a5559028ffbf52c
out=$($R sudo timeout 300 ./hvx_disk_boot_verify $B/bzImage $B/initramfs-disk.cpio.gz $B/vblk-disk.img 2>&1); rc=$?
check "boot (with disk)" "$rc" "$(grep -a 'fp1\|fp_a' <<< "$out")" 36583d533d7c989b

if [ "$FLAG" = 1 ]; then
  echo ">> flagship verifies (PASS = the harness's own MILESTONE PASS line)"
  flag() {   # name timeout cmd...
    local name=$1 to=$2; shift 2
    local s=$(date +%s)
    out=$($R sudo env HVX_SNAP_DIR=/var/tmp/hvx-snap timeout "$to" "$@" 2>&1); rc=$?
    local fps; fps=$(grep -aE '^(SANITY|F[123]|CONTRAST|\([1-4]\))' <<< "$out" | grep -aoE 'fp[0-9a-z_]*=[0-9a-f]{16}' | sort -u | tr '\n' ' ' | cut -c1-260)
    if [ $rc = 0 ] && grep -qa 'MILESTONE [0-9]* PASS\|mode=all PASS' <<< "$out"; then
      pass=$((pass+1)); printf '  PASS %-8s %4ss %s\n' "$name" "$(( $(date +%s)-s ))" "$fps"
    else
      fail=$((fail+1)); failed="$failed $name"
      printf '  FAIL %-8s rc=%s %s\n' "$name" "$rc" "$(grep -a 'FAIL' <<< "$out" | tail -1 | cut -c1-200)"
    fi
  }
  flag hydokv 1500 ./hvx_bug_verify    $B/bzImage $B/initramfs-kv.cpio.gz     $B/kv-disk.img
  flag sqlite 1500 ./hvx_sqlite_verify $B/bzImage $B/initramfs-sql.cpio.gz    $B/sql-disk.img
  flag redis  1500 ./hvx_redis_verify  $B/bzImage $B/initramfs-redis.cpio.gz  $B/redis-disk.img
  flag rootfs 1500 ./hvx_rootfs_verify $B/bzImage $B/initramfs-rootfs.cpio.gz $B/rootfs-disk.img
  flag system 1500 ./hvx_system_verify $B/bzImage $B/initramfs-system.cpio.gz $B/system-disk.img
  flag jvm    1500 ./hvx_jvm_verify    $B/bzImage $B/initramfs-jvm.cpio.gz    $B/jvm-disk.img "$K hydo_jit=c1sync" 640 all
  flag kafka  1500 ./hvx_kafka_verify  $B/bzImage $B/initramfs-kafka.cpio.gz  $B/kafka-disk.img "$K" 1024 all
  flag mysql  1500 ./hvx_mysql_verify  $B/bzImage $B/initramfs-kafka.cpio.gz  $B/mysql-disk.img "$K" 1024 all
fi

lock=$(sudo dmesg | grep -aiE 'soft lockup|rcu.*stall|BUG:|Oops' | tail -3)
[ -n "$lock" ] && { echo ">> host dmesg warnings:"; echo "$lock"; }
echo "=================================================================="
if [ $fail = 0 ] && [ $cfail = 0 ]; then
  echo "deploy-metal: ALL PASS ($pass checks)"
else
  echo "deploy-metal: FAIL ($fail of $((pass+fail)) checks failed:$failed$([ $cfail = 1 ] && echo ' + compile errors'))"
  exit 1
fi
REMOTE
