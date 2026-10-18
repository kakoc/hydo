#!/bin/bash
# The hand-written suite + both boot verifies of hvx/deploy-metal.sh, run on the box against
# a side-by-side module (default /dev/hvxpre) through the devshim LD_PRELOAD. Sources from
# the tree this script is in; the main hvx.ko and ~/hydo are not touched.
set -u
DEV=${HVX_DEV:-/dev/hvxpre}
cd "$(dirname "$0")/.."
B=~/hydo/build; R=~/bin/hvx-run
cc -O2 -shared -fPIC -o race/devshim.so race/devshim.c -ldl || exit 1
SHIM=$PWD/race/devshim.so
TESTS="hvx_test hvx_rdtsc_test hvx_step_test hvx_tscclock_test hvx_snapshot_test hvx_intr_test
hvx_verify hvx_console_test hvx_bigmem_test hvx_scale_test hvx_cpuid_test hvx_sregs_test
hvx_input_test hvx_smp_test hvx_virtio_iso_test hvx_virtio_pci_test hvx_fault_test hvx_net_test
hvx_net_fault_test hvx_bug_f0_test hvx_fuzzresume_test hvx_pmu_test hvx_bigram_test"
declare -A EXP=(
  [hvx_virtio_iso_test]="7e586473f6e494d7" [hvx_virtio_pci_test]="3f95ef833747f3ec"
  [hvx_fault_test]="91170d4dbf101c5b 88f680fc22cc9bb0 9090286f3fc59682" [hvx_net_test]="0e2b7acf2f4f94ee"
  [hvx_net_fault_test]="883a00eb6a14a6f0 37490fb78da33064 40659aabb208aaa0" [hvx_bug_f0_test]="1083e400458728ff"
  [hvx_smp_test]="27ead8c880ef7820 88ee51bd9f8e40e0 d042bb01201c269d" [hvx_pmu_test]="86c4afcd0a799450")
pass=0; fail=0
check() { local name=$1 rc=$2 out=$3; shift 3; local miss=""
  for fp in "$@"; do grep -q "$fp" <<< "$out" || miss="$miss $fp"; done
  if [ "$rc" = 0 ] && [ -z "$miss" ]; then pass=$((pass+1)); echo "  PASS $name"
  else fail=$((fail+1)); echo "  FAIL $name rc=$rc missing:$miss"; fi; }
for t in $TESTS hvx_boot_verify hvx_disk_boot_verify; do cc -O2 -Wall -o /tmp/side-$t $t.c 2>/dev/null || echo "COMPILE FAIL $t"; done
for t in $TESTS; do
  out=$($R sudo env LD_PRELOAD=$SHIM HVX_DEV=$DEV timeout 150 /tmp/side-$t 2>&1); rc=$?
  check "$t" "$rc" "$out" ${EXP[$t]:-}
done
out=$($R sudo env LD_PRELOAD=$SHIM HVX_DEV=$DEV timeout 300 /tmp/side-hvx_boot_verify $B/bzImage $B/initramfs.cpio.gz 2>&1); rc=$?
check "boot (no disk)" "$rc" "$(grep -a 'fp1\|fp_a' <<< "$out")" 7a5559028ffbf52c
out=$($R sudo env LD_PRELOAD=$SHIM HVX_DEV=$DEV timeout 300 /tmp/side-hvx_disk_boot_verify $B/bzImage $B/initramfs-disk.cpio.gz $B/vblk-disk.img 2>&1); rc=$?
check "boot (with disk)" "$rc" "$(grep -a 'fp1\|fp_a' <<< "$out")" 36583d533d7c989b
echo "side-metal ($DEV): $([ $fail = 0 ] && echo "ALL PASS ($pass checks)" || echo "FAIL ($fail of $((pass+fail)))")"
