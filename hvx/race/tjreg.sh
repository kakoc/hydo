#!/bin/bash
# Run the deploy-metal hand-written suite + both boot verifies against the side-by-side
# module /dev/hvxtj: copies of the test sources with the device path rewritten.
set -u
D=/var/tmp/tjreg; rm -rf $D; mkdir -p $D; cd $D
B=~/hydo/build; R=~/bin/hvx-run
TESTS="hvx_test hvx_rdtsc_test hvx_step_test hvx_tscclock_test hvx_snapshot_test hvx_intr_test
hvx_verify hvx_console_test hvx_bigmem_test hvx_scale_test hvx_cpuid_test hvx_sregs_test
hvx_input_test hvx_smp_test hvx_virtio_iso_test hvx_virtio_pci_test hvx_fault_test hvx_net_test
hvx_net_fault_test hvx_bug_f0_test hvx_fuzzresume_test hvx_pmu_test hvx_bigram_test"
for f in $TESTS hvx_boot_verify hvx_disk_boot_verify; do cp ~/hydo/hvx/$f.c .; done
cp ~/hydo/hvx/hvx_boot.h ~/hydo/hvxtj/hvx_abi.h .
sed -i 's#"/dev/hvx"#"/dev/hvxtj"#' *.c hvx_boot.h
cfail=0
for t in $TESTS hvx_boot_verify hvx_disk_boot_verify; do
  cc -O2 -Wall -o $t $t.c 2>/tmp/cc-tj-$t.log || { echo "COMPILE FAIL $t"; cfail=1; }
done
grep -l '/dev/hvx"' *.c hvx_boot.h && echo "WARNING: unrewritten device path"
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

lock=$(sudo dmesg | grep -aiE 'soft lockup|rcu.*stall|BUG:|Oops' | tail -3)
[ -n "$lock" ] && { echo ">> host dmesg warnings:"; echo "$lock"; }
echo "tjreg: pass=$pass fail=$fail$([ $fail != 0 ] && echo " failed:$failed") compile_fail=$cfail"
