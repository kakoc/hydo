#!/usr/bin/env bash
# Prove that a module change is inert for valid use (Milestone 97). Run FROM THE MAC.
# Builds this tree's hvx.c as a SIDE module hvxsafe.ko (device /dev/hvxsafe) in
# ~/hydo-safety on the bare-metal test machine, loads it next to the shared hvx.ko (which is never
# touched), and runs the whole hvx/deploy-metal.sh suite (31 checks: 24 hand-written tests,
# 4 several-vCPU tests, 3 boots) plus the two pinned explorer reproducers against it, one VM
# at a time, every VM through ~/bin/hvx-run, through the devshim LD_PRELOAD that sends
# open("/dev/hvx") to /dev/hvxsafe. Every expected fingerprint is the recorded baseline.
#   side-check.sh [build|run|unload|all]   (default all = build, load, run; unload is separate)
# It waits (up to 2 hours) while /var/tmp/hydo-QUIET exists on the box.
set -euo pipefail
BOX="${HVX_METAL:?}"
HERE="$(cd "$(dirname "$0")" && pwd)"
HVX="$(cd "$HERE/.." && pwd)"
what=${1:-all}

if [ "$what" = unload ]; then
  ssh -o BatchMode=yes "$BOX" "lsmod | grep -q '^hvxsafe ' && sudo rmmod hvxsafe; lsmod | grep '^hvx' || true"
  exit 0
fi

if [ "$what" = build ] || [ "$what" = all ]; then
  ssh -o BatchMode=yes "$BOX" 'mkdir -p ~/hydo-safety/hvxsafe ~/hydo-safety/hvx/race ~/hydo-safety/hvx/explore'
  rsync -az "$HVX/hvx.c" "$HVX/hvx_abi.h" "$BOX:hydo-safety/hvxsafe/"
  rsync -az "$HVX/hvx_abi.h" "$HVX/hvx_boot.h" "$HVX/hvx_sched_util.h" "$HVX"/hvx_*_test.c "$HVX"/hvx_test.c \
    "$HVX"/hvx_verify.c "$HVX"/hvx_*_verify.c "$HVX/hvx_explore.c" "$BOX:hydo-safety/hvx/"
  rsync -az "$HVX/explore/ptedges.h" "$BOX:hydo-safety/hvx/explore/"
  rsync -az "$HVX/race/devshim.c" "$BOX:hydo-safety/hvx/race/"
  ssh -o BatchMode=yes "$BOX" 'cd ~/hydo-safety/hvxsafe && cp -f hvx.c hvxsafe.c && cat > Makefile <<"EOF"
obj-m := hvxsafe.o
ccflags-y := -DHVX_DEVNAME=\"hvxsafe\"
OBJECT_FILES_NON_STANDARD_hvxsafe.o := y
KDIR ?= /lib/modules/$(shell uname -r)/build
all:
	$(MAKE) -C $(KDIR) M=$(CURDIR) modules
EOF
make 2>&1 | grep -E " error |warning: " ; test -f hvxsafe.ko && modinfo -F srcversion hvxsafe.ko'
  ssh -o BatchMode=yes "$BOX" "lsmod | grep -q '^hvxsafe ' && sudo rmmod hvxsafe; sudo insmod ~/hydo-safety/hvxsafe/hvxsafe.ko && ls -la /dev/hvxsafe && sudo dmesg | grep 'hvx.*hvxsafe\|/dev/hvxsafe' | tail -2"
fi
[ "$what" = build ] && exit 0

ssh -o BatchMode=yes "$BOX" 'bash -s' <<'REMOTE'
set -u
cd ~/hydo-safety/hvx
B=~/hydo/build; R=~/bin/hvx-run; DEV=/dev/hvxsafe; T=~/hydo-safety/bin
mkdir -p $T /var/tmp/m97-side
quiet() { local n=0; while [ -e /var/tmp/hydo-QUIET ] && [ $n -lt 720 ]; do sleep 10; n=$((n+1)); done; }
cc -O2 -shared -fPIC -o $T/devshim.so race/devshim.c -ldl || exit 1
TESTS="hvx_test hvx_rdtsc_test hvx_step_test hvx_tscclock_test hvx_snapshot_test hvx_intr_test
hvx_verify hvx_console_test hvx_bigmem_test hvx_scale_test hvx_cpuid_test hvx_sregs_test
hvx_input_test hvx_smp_test hvx_virtio_iso_test hvx_virtio_pci_test hvx_fault_test hvx_net_test
hvx_net_fault_test hvx_bug_f0_test hvx_fuzzresume_test hvx_pmu_test hvx_bigram_test hvx_avx_test"
SMPTESTS="hvx_sched_pmu_test hvx_sched_race_test hvx_sipi_test hvx_sched_sweep_test"
for t in $TESTS $SMPTESTS hvx_boot_verify hvx_disk_boot_verify hvx_smp_boot_verify; do
  cc -O2 -Wall -o $T/$t $t.c 2>/dev/null || echo "COMPILE FAIL $t"; done
cc -O2 -Wall -o $T/hvx_explore hvx_explore.c -lipt 2>/dev/null || cc -O2 -Wall -DHYDO_NO_LIBIPT -o $T/hvx_explore hvx_explore.c
declare -A EXP=(
  [hvx_virtio_iso_test]="7e586473f6e494d7" [hvx_virtio_pci_test]="3f95ef833747f3ec"
  [hvx_fault_test]="91170d4dbf101c5b 88f680fc22cc9bb0 9090286f3fc59682" [hvx_net_test]="0e2b7acf2f4f94ee"
  [hvx_net_fault_test]="883a00eb6a14a6f0 37490fb78da33064 40659aabb208aaa0" [hvx_bug_f0_test]="1083e400458728ff"
  [hvx_avx_test]="3bb428a134ce542a" [hvx_smp_test]="27ead8c880ef7820 88ee51bd9f8e40e0 d042bb01201c269d"
  [hvx_pmu_test]="86c4afcd0a799450" [hvx_sched_pmu_test]="73a465516c4e0094 b544e114dc426e85"
  [hvx_sched_race_test]="8cb7c61a2393f44e" [hvx_sipi_test]="eef5c1ebd4df2e7d c8d015ab3b57252d"
  [hvx_sched_sweep_test]="39971eed9c2f35fe")
pass=0; fail=0
check() { local name=$1 rc=$2 out=$3; shift 3; local miss=""
  for fp in "$@"; do grep -q "$fp" <<< "$out" || miss="$miss $fp"; done
  local fps; fps=$(grep -aoE '\b[0-9a-f]{16}\b' <<< "$out" | sort -u | tr '\n' ' ' | cut -c1-120)
  if [ "$rc" = 0 ] && [ -z "$miss" ]; then pass=$((pass+1)); printf '  PASS %-24s %s\n' "$name" "$fps"
  else fail=$((fail+1)); printf '  FAIL %-24s rc=%s missing:%s got: %s\n' "$name" "$rc" "${miss:- none}" "$fps"; fi; }
run() { quiet; $R sudo env LD_PRELOAD=$T/devshim.so HVX_DEV=$DEV "$@" 2>&1; }
for t in $TESTS; do out=$(run timeout 150 $T/$t); rc=$?; check "$t" "$rc" "$out" ${EXP[$t]:-}; done
for t in $SMPTESTS; do a=""; [ $t = hvx_sched_sweep_test ] && a=2000
  out=$(run timeout 900 $T/$t $a); rc=$?; check "$t" "$rc" "$out" ${EXP[$t]:-}; done
out=$(run timeout 300 $T/hvx_boot_verify $B/bzImage $B/initramfs.cpio.gz); rc=$?
check "boot (no disk)" "$rc" "$(grep -a 'fp1\|fp_a' <<< "$out")" 7a5559028ffbf52c
out=$(run timeout 300 $T/hvx_disk_boot_verify $B/bzImage $B/initramfs-disk.cpio.gz $B/vblk-disk.img); rc=$?
check "boot (with disk)" "$rc" "$(grep -a 'fp1\|fp_a' <<< "$out")" 36583d533d7c989b
out=$(run timeout 600 $T/hvx_smp_boot_verify $B/bzImage $B/initramfs.cpio.gz 2); rc=$?
check "boot (2 vCPUs)" "$rc" "$out" 6584c954c2839491
S='disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24'
for img in explore-disk.img:2403499c9a3f8419 explore-fix-disk.img:f87130453609a5d6; do
  rm -rf /var/tmp/m97-side/${img%%:*}
  out=$(cd $T && run timeout 1800 $T/hvx_explore one $B/bzImage $B/initramfs-kafka.cpio.gz $B/${img%%:*} /var/tmp/m97-side/${img%%:*} "$S"); rc=$?
  echo "$out" > /var/tmp/m97-side/${img%%:*}.out; check "repro ${img%%:*} (rc=$rc)" 0 "$out" ${img##*:}
done
lock=$(sudo dmesg | grep -aiE 'soft lockup|rcu.*stall|BUG:|Oops' | tail -3)
[ -n "$lock" ] && { echo ">> host dmesg warnings:"; echo "$lock"; }
echo "side-check (/dev/hvxsafe, srcversion $(cat /sys/module/hvxsafe/srcversion)): $([ $fail = 0 ] && echo "ALL PASS ($pass checks)" || echo "FAIL ($fail of $((pass+fail)))")"
REMOTE
