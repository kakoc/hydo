#!/usr/bin/env bash
#
# deploy-metal.sh -- regression runner for the dedicated bare-metal machine (run FROM THE MAC).
# hvx runs directly on that host (no nested L1), so this replaces deploy-hvx.sh there.
#
#   ./deploy-metal.sh              sync userspace sources and the Rust workspace, compile them on
#                                  the bare-metal machine, run the hand-written suite + the boot verifies + the
#                                  two pinned reproducers (agent images of the stack example, M145:
#                                  `hydo run`, the host controller over the Rust engine) against
#                                  the module that is ALREADY loaded, print PASS/FAIL per baseline
#   ./deploy-metal.sh load         also sync hvx.c/hvx_abi.h/Makefile and rebuild + reload the
#                                  module first (via ~/bin/hvx-reload: exclusive module lock)
#   ./deploy-metal.sh flagships    also run the real-service verifies (hydokv, SQLite, Redis,
#                                  rootfs, system, JVM, Kafka, MySQL) and print their results
#   (options combine: ./deploy-metal.sh load flagships)
#
# Every command that opens /dev/hvx runs under ~/bin/hvx-run (shared flock), so a concurrent
# module reload waits for running VMs instead of killing them. Guest images are expected in
# ~/hydo/build on the bare-metal machine (built on the build machine, shipped with rsync); the pinned reproducers'
# images in ~/hydo/build/m204 (hydo/onboard/examples/stack/build-reproducers.sh; rebuilt at the fingerprint
# epoch 2 in M204, which turns the CPUID cache leaves on: the module needs HVX_VM_CPU_CACHE). The run engine
# and `hydo` are built with the bare-metal machine's Rust (~/hydo-rust/env.sh) in ~/$HVX_METAL_TREE (cargo build
# --release -p hydo-hvx -p hydo).
# The bare-metal machine is reached through machines.local.sh (machines.sh at the repository root).
# Env: HVX_METAL_TREE, the repository's directory in the bare-metal machine's home
# (default hydo, the main tree; another session's own tree, e.g. hydo-layout, for a run that must
# not touch the main tree). The engine sources go to ~/$HVX_METAL_TREE/hv/hvx on the bare-metal machine.
set -euo pipefail

. "$(cd "$(dirname "$0")/../.." && pwd)/machines.sh"; need_bare_metal; BOX="$BARE_METAL"
TREE="${HVX_METAL_TREE:-hydo}"
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
hvx_net_fault_test hvx_bug_f0_test hvx_fuzzresume_test hvx_pmu_test hvx_bigram_test hvx_avx_test
hvx_cpucache_test"
# several vCPUs (CLAUDE.md M66/M69): hand-written tests, run with a longer timeout below
SMPTESTS="hvx_sched_pmu_test hvx_sched_race_test hvx_sipi_test hvx_sched_sweep_test"
# guest RAM above 4 GiB (M179; in the main module since 2026-11-08): an 8 GiB guest, three rounds
BIGTESTS="hvx_hugeram_test"
HARNESSES="hvx_boot_verify hvx_disk_boot_verify hvx_smp_boot_verify hvx_bug_verify hvx_sqlite_verify hvx_redis_verify
hvx_rootfs_verify hvx_system_verify hvx_jvm_verify hvx_kafka_verify hvx_mysql_verify"

# Explicit file list: userspace sources only. hvx.c/hvx_abi.h go over only with `load`,
# so a plain run never overwrites a newer module source that is being worked on on the bare-metal machine.
FILES="hvx_boot.h hvx_sched_util.h"
for t in $TESTS $SMPTESTS $BIGTESTS $HARNESSES; do FILES="$FILES $t.c"; done
[ $LOAD = 1 ] && FILES="$FILES hvx.c hvx_abi.h Makefile"
echo ">> syncing to ~/$TREE/hv/hvx/ on the bare-metal machine ($([ $LOAD = 1 ] && echo 'with' || echo 'without') module sources) and the Rust workspace"
on_bare_metal "mkdir -p $TREE/hv/hvx"
(cd "$HERE" && rsync -az $FILES "$BOX:$TREE/hv/hvx/")
# the run engine's sources (the Rust workspace's crates; never hvx.c or hvx_abi.h), with the files
# the crates compile in through links (hydo-agents/embed: the guide, MANIFEST.md, the skill since
# M170; every crate's LICENSE; hydo-cli/pins: the four pin files since M154; hydo-image/pins: the
# release index since M162)
(cd "$HERE/../.." && rsync -azR --exclude target Cargo.toml Cargo.lock rust-toolchain.toml .cargo hydo/crates \
    LICENSE hydo/guide hydo/onboard/MANIFEST.md skills/hydo-onboard hydo/dist/releases.txt \
    hydo/dist/linux/assets.sha256 hydo/dist/linux/guest-programs.sha256 \
    hydo/dist/mac/assets.sha256 hydo/dist/mac/guest-programs.sha256 "$BOX:$TREE/")

on_bare_metal "TREE=$TREE LOAD=$LOAD FLAG=$FLAG TESTS='$TESTS' SMPTESTS='$SMPTESTS' BIGTESTS='$BIGTESTS' HARNESSES='$HARNESSES' bash -s" <<'REMOTE'
set -u
cd ~/$TREE/hv/hvx
B=~/hydo/build
R=~/bin/hvx-run
K="root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"
[ -f ~/hydo/COORD.md ] && { echo ">> COORD.md:"; cat ~/hydo/COORD.md; }

if [ "$LOAD" = 1 ]; then
  # ~/bin/hvx-reload builds the module in one fixed directory; refuse to reload anything else
  grep -q "$HOME/$TREE/hv/hvx\b" ~/bin/hvx-reload || { echo "FAIL: ~/bin/hvx-reload does not build ~/$TREE/hv/hvx (it says: $(grep -o 'cd [^ ]*' ~/bin/hvx-reload | head -1)); not reloading"; exit 1; }
  echo ">> rebuilding + reloading the module"
  ~/bin/hvx-reload || { echo "FAIL: module reload"; exit 1; }
fi
lsmod | grep -q '^hvx' || { echo "FAIL: hvx module is not loaded (run with 'load')"; exit 1; }

echo ">> compiling userspace tests + harnesses"
cfail=0
for t in $TESTS $SMPTESTS $BIGTESTS $HARNESSES; do
  # build to a temp name and rename: a harness that is running right now keeps its old
  # inode instead of failing the build with "Text file busy"
  if cc -O2 -Wall -o "$t.new" "$t.c" 2> "/tmp/cc-$t.log"; then mv -f "$t.new" "$t"
  else echo "  COMPILE FAIL: $t"; cat "/tmp/cc-$t.log"; cfail=1; fi
done

echo ">> building the run engine (hydo-hvx) and hydo"
if [ -f ~/hydo-rust/env.sh ] && ( . ~/hydo-rust/env.sh && cd ~/$TREE && cargo build --release --locked -q -p hydo-hvx -p hydo ) 2> /tmp/cc-hydo-hvx.log; then :
else echo "  COMPILE FAIL: hydo-hvx, hydo (~/hydo-rust/env.sh and cargo)"; tail -20 /tmp/cc-hydo-hvx.log; cfail=1; fi

# test -> fingerprints that must ALL appear in its output (empty = rc only)
declare -A EXP=(
  [hvx_virtio_iso_test]="7e586473f6e494d7"
  [hvx_virtio_pci_test]="3f95ef833747f3ec"
  [hvx_fault_test]="91170d4dbf101c5b 88f680fc22cc9bb0 9090286f3fc59682"
  [hvx_net_test]="0e2b7acf2f4f94ee"
  [hvx_net_fault_test]="883a00eb6a14a6f0 37490fb78da33064 40659aabb208aaa0"
  [hvx_bug_f0_test]="1083e400458728ff"
  [hvx_avx_test]="3bb428a134ce542a"
  [hvx_cpucache_test]="64329d56a2c9b638"
  [hvx_smp_test]="27ead8c880ef7820 88ee51bd9f8e40e0 d042bb01201c269d"
  [hvx_pmu_test]="86c4afcd0a799450"
  [hvx_sched_pmu_test]="73a465516c4e0094 b544e114dc426e85"
  [hvx_sched_race_test]="8cb7c61a2393f44e"
  [hvx_sipi_test]="eef5c1ebd4df2e7d c8d015ab3b57252d"
  [hvx_sched_sweep_test]="39971eed9c2f35fe"
  [hvx_hugeram_test]="b71aaf1096f96f02"
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

echo ">> several-vCPU suite"
for t in $SMPTESTS; do
  a=""; [ "$t" = hvx_sched_sweep_test ] && a=2000
  out=$($R sudo timeout 900 ./$t $a 2>&1); rc=$?
  check "$t" "$rc" "$out" ${EXP[$t]:-}
done

echo ">> guest RAM above 4 GiB (8 GiB guests, one at a time; waits for 9 GiB of free memory)"
for t in $BIGTESTS; do
  n=0; while [ $n -lt 1800 ] && [ "$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)" -lt 9216 ]; do sleep 30; n=$((n+30)); done
  out=$($R sudo timeout 900 ./$t 8192 2>&1); rc=$?
  check "$t" "$rc" "$out" ${EXP[$t]:-}
done

echo ">> boot verifies"
out=$($R sudo timeout 300 ./hvx_boot_verify $B/bzImage $B/initramfs.cpio.gz 2>&1); rc=$?
check "boot (no disk)" "$rc" "$(grep -a 'fp1\|fp_a' <<< "$out")" 7a5559028ffbf52c
out=$($R sudo timeout 300 ./hvx_disk_boot_verify $B/bzImage $B/initramfs-disk.cpio.gz $B/vblk-disk.img 2>&1); rc=$?
check "boot (with disk)" "$rc" "$(grep -a 'fp1\|fp_a' <<< "$out")" 36583d533d7c989b
out=$($R sudo timeout 600 ./hvx_smp_boot_verify $B/bzImage $B/initramfs.cpio.gz 2 2>&1); rc=$?
check "boot (2 vCPUs)" "$rc" "$out" 6584c954c2839491

echo ">> the pinned reproducers: the stack example as agent images (M145; a stack VM each; waits for 6 GiB of free memory)"
# hydo finds its guest kernel under <its root>/guest: a minimal prefix around this tree's hydo
P=/var/tmp/deploy-metal-hydo; rm -rf $P; mkdir -p $P/bin $P/guest $P/libexec
cp ~/$TREE/target/release/hydo $P/bin/hydo
ln -s $B/bzImage $P/guest/bzImage; ln -s $B/initramfs-kafka.cpio.gz $P/guest/initramfs-kafka.cpio.gz
ln -s $R $P/libexec/hvx-run
S="disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24"
for p in explore-ob-stack-old-disk.img:a76d7fe857fdb37c explore-ob-stack-fix-disk.img:efbde7b57264be0f; do
  img=${p%%:*}; want=${p##*:}
  n=0; while [ $n -lt 1800 ] && [ "$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)" -lt 6144 ]; do sleep 30; n=$((n+30)); done
  o=/var/tmp/deploy-metal-pin-$img; rm -rf "$o"
  out=$(HYDO_HVX_RUN=$R HYDO_HVX_EXPLORE=~/$TREE/target/release/hydo-hvx timeout 1800 $P/bin/hydo run $B/m204/$img "$S" --out "$o" 2>&1); rc=$?
  check "pinned $img" "$rc" "$(grep -a '^RESULT' <<< "$out" | grep -oE ' fp=[0-9a-f]{16}')" $want
done

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
