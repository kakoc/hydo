#!/bin/bash
# M138: the full suite against the side module hvxgc (/dev/hvxgc, hv/hvx/gc/build-hvxgc.sh), run ON
# THE BARE-METAL TEST MACHINE from the tree it is in (default ~/hydo-mod), after
#   cargo build --release --locked -p hydo-hvx -p hydo-engine --bin hydo-hvx \
#     --example lockstep_check --example agent_boot_check
# The shared hvx.ko is never touched; every VM goes through ~/bin/hvx-run; the C tests reach the
# side module through the devshim LD_PRELOAD, the Rust engine through HVX_DEV.
#
#   side-suite.sh off|on [part ...]
#     off: every M138 feature off (the engine's defaults): every recorded value must hold.
#     on:  HYDO_HVX_REUSE=1 HYDO_HVX_CHECK_RESET=1 HYDO_HVX_HOSTCALL=1 for the engine's checks
#          (the C tests do not use the features and run in "off" only).
#   parts: tests smp boots repro lockstep agent (default: all of off, or repro lockstep agent)
# repro: the two pinned reproducers, three runs each in one `hydo-hvx serve` (the first in fresh
# VMs; with reuse on, the next two in the VMs the first left: the in-place reset), each RESULT must
# carry the recorded fingerprint. agent: M121's agent boot check on M121's image and on M131's
# Redis agent image (recorded values with the features off; with the host-call port offered these
# old agents stay on the serial line, the kernel command line has one more word, so only the
# check's own comparisons must hold). Results under /var/tmp/m138/suite-<mode>/ ($SUITE_OUT instead of
# /var/tmp/m138; M140 ran it against the main module under the debug kernel with HVX_DEV=/dev/hvx).
set -u
MODE=${1:-off}
[ $# -gt 0 ] && shift
case $MODE in
  off) FEAT=(); PARTS=${*:-tests smp boots repro lockstep agent};;
  on) FEAT=(HYDO_HVX_REUSE=1 HYDO_HVX_CHECK_RESET=1 HYDO_HVX_HOSTCALL=1); PARTS=${*:-repro lockstep agent};;
  *) echo "usage: side-suite.sh off|on [part ...]" >&2; exit 2;;
esac
DEV=${HVX_DEV:-/dev/hvxgc}
H=$(cd "$(dirname "$0")/.." && pwd)	# hv/hvx
T=$(cd "$H/../.." && pwd)		# the tree
B=~/hydo/build
R=~/bin/hvx-run
O=${SUITE_OUT:-/var/tmp/m138}/suite-$MODE
BIN=$O/bin
mkdir -p "$BIN"
[ -e "$DEV" ] || { echo "side-suite: $DEV is not there (load it with build-hvxgc.sh load)"; exit 1; }
quiet() { local n=0; while [ -e /var/tmp/hydo-QUIET ] && [ $n -lt 720 ]; do sleep 10; n=$((n+1)); done; }
memwait() { local n=0; while [ "$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)" -lt "$1" ] && [ $n -lt 720 ]; do sleep 10; n=$((n+1)); done; }
pass=0; fail=0
check() { local name=$1 rc=$2 out=$3; shift 3; local miss=""
  for fp in "$@"; do grep -q "$fp" <<< "$out" || miss="$miss $fp"; done
  local fps; fps=$(grep -aoE '\b[0-9a-f]{16}\b' <<< "$out" | sort -u | tr '\n' ' ' | cut -c1-120)
  if [ "$rc" = 0 ] && [ -z "$miss" ]; then pass=$((pass+1)); printf '  PASS %-28s %s\n' "$name" "$fps"
  else fail=$((fail+1)); printf '  FAIL %-28s rc=%s missing:%s got: %s\n' "$name" "$rc" "${miss:- none}" "$fps"; fi; }
cc -O2 -shared -fPIC -o "$BIN/devshim.so" "$H/race/devshim.c" -ldl || exit 1
shim() { quiet; $R sudo env LD_PRELOAD="$BIN/devshim.so" HVX_DEV="$DEV" "$@" 2>&1; }
TESTS="hvx_test hvx_rdtsc_test hvx_step_test hvx_tscclock_test hvx_snapshot_test hvx_intr_test
hvx_verify hvx_console_test hvx_bigmem_test hvx_scale_test hvx_cpuid_test hvx_sregs_test
hvx_input_test hvx_smp_test hvx_virtio_iso_test hvx_virtio_pci_test hvx_fault_test hvx_net_test
hvx_net_fault_test hvx_bug_f0_test hvx_fuzzresume_test hvx_pmu_test hvx_bigram_test hvx_avx_test"
SMPTESTS="hvx_sched_pmu_test hvx_sched_race_test hvx_sipi_test hvx_sched_sweep_test"
declare -A EXP=(
  [hvx_virtio_iso_test]="7e586473f6e494d7" [hvx_virtio_pci_test]="3f95ef833747f3ec"
  [hvx_fault_test]="91170d4dbf101c5b 88f680fc22cc9bb0 9090286f3fc59682" [hvx_net_test]="0e2b7acf2f4f94ee"
  [hvx_net_fault_test]="883a00eb6a14a6f0 37490fb78da33064 40659aabb208aaa0" [hvx_bug_f0_test]="1083e400458728ff"
  [hvx_avx_test]="3bb428a134ce542a" [hvx_smp_test]="27ead8c880ef7820 88ee51bd9f8e40e0 d042bb01201c269d"
  [hvx_pmu_test]="86c4afcd0a799450" [hvx_sched_pmu_test]="73a465516c4e0094 b544e114dc426e85"
  [hvx_sched_race_test]="8cb7c61a2393f44e" [hvx_sipi_test]="eef5c1ebd4df2e7d c8d015ab3b57252d"
  [hvx_sched_sweep_test]="39971eed9c2f35fe")
has() { [[ " $PARTS " == *" $1 "* ]]; }
if has tests || has smp || has boots; then
  for t in $TESTS $SMPTESTS hvx_boot_verify hvx_disk_boot_verify hvx_smp_boot_verify; do
    cc -O2 -Wall -o "$BIN/$t" "$H/$t.c" 2>/dev/null || echo "COMPILE FAIL $t"; done
fi
if has tests; then
  echo ">> the hand-written tests"
  for t in $TESTS; do out=$(shim timeout 150 "$BIN/$t"); rc=$?; check "$t" "$rc" "$out" ${EXP[$t]:-}; done
fi
if has smp; then
  echo ">> the several-vCPU tests"
  for t in $SMPTESTS; do a=""; [ $t = hvx_sched_sweep_test ] && a=2000
    out=$(shim timeout 900 "$BIN/$t" $a); rc=$?; check "$t" "$rc" "$out" ${EXP[$t]:-}; done
fi
if has boots; then
  echo ">> the boots"
  out=$(shim timeout 300 "$BIN/hvx_boot_verify" $B/bzImage $B/initramfs.cpio.gz); rc=$?
  check "boot (no disk)" "$rc" "$(grep -a 'fp1\|fp_a' <<< "$out")" 7a5559028ffbf52c
  out=$(shim timeout 300 "$BIN/hvx_disk_boot_verify" $B/bzImage $B/initramfs-disk.cpio.gz $B/vblk-disk.img); rc=$?
  check "boot (with disk)" "$rc" "$(grep -a 'fp1\|fp_a' <<< "$out")" 36583d533d7c989b
  out=$(shim timeout 600 "$BIN/hvx_smp_boot_verify" $B/bzImage $B/initramfs.cpio.gz 2); rc=$?
  check "boot (2 vCPUs)" "$rc" "$out" 6584c954c2839491
fi
ENG=$T/target/release/hydo-hvx
EX=$T/target/release/examples
if has repro; then
  echo ">> the pinned reproducers (three runs each in one serve)"
  S='disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24'
  for img in explore-disk.img:2403499c9a3f8419 explore-fix-disk.img:f87130453609a5d6; do
    d=$O/repro-${img%%:*}; sudo rm -rf "$d"; mkdir -p "$d"
    memwait 10240; quiet
    out=$(printf 'RUN r1 %s\nRUN r2 %s\nRUN r3 %s\nQUIT\n' "$S" "$S" "$S" | \
      timeout 3600 $R sudo env HVX_DEV="$DEV" HVX_EXPLORE_PHASES=1 "${FEAT[@]}" "$ENG" serve \
        $B/bzImage $B/initramfs-kafka.cpio.gz $B/${img%%:*} "$d" 2> "$d/engine.err"); rc=$?
    echo "$out" > "$d/out.txt"
    n=$(grep -c "^RESULT r[123] fp=${img##*:}" <<< "$out")
    check "repro ${img%%:*} x3 ($n of 3)" "$([ "$n" = 3 ] && echo $rc || echo 1)" "$out" ${img##*:}
    grep -a "^PHASES" "$d/engine.err" | sed 's/^/     /' | cut -c1-200
  done
fi
if has lockstep; then
  echo ">> M118's lock-step check"
  d=$O/lockstep; sudo rm -rf "$d"; mkdir -p "$d"; quiet
  out=$(timeout 7200 $R sudo env HOME="$HOME" HVX_DEV="$DEV" HYDO_ENGINE=inproc "${FEAT[@]}" \
    "$EX/lockstep_check" check "$ENG" "$R" $B/bzImage $B/initramfs-kafka.cpio.gz \
    $B/explore-ob-redis-nq-disk.img "$d" 2>&1); rc=$?
  echo "$out" > "$d/out.txt"
  check "lockstep_check" "$rc" "$(tail -n 3 <<< "$out")" "ALL PASS"
fi
if has agent; then
  echo ">> M121's agent boot check"
  K="$B/bzImage $B/initramfs-kafka.cpio.gz"
  for w in m121:$HOME/hydo-cw-work/img/explore-ob-redis-agent-disk.img:c24c475ea7a448ad:8328c792aa149b20 \
           m131:$HOME/hydo-anf-work/img/redis_hydo/agent/explore-ob-redis-agent-disk.img:fe22b0f1a8e84d68:18b5113ce43a1d1f; do
    IFS=: read -r name img boot s1 <<< "$w"
    d=$O/agent-$name; sudo rm -rf "$d"; mkdir -p "$d"; quiet
    out=$(timeout 3600 $R sudo env HOME="$HOME" HVX_DEV="$DEV" HYDO_ENGINE=inproc "${FEAT[@]}" \
      "$EX/agent_boot_check" hvx x - $K "$img" "$d" 16 2>&1); rc=$?
    echo "$out" > "$d/out.txt"
    if [ "$MODE" = off ]; then check "agent_boot_check $name" "$rc" "$out" "ALL PASS" "$boot" "$s1"
    else check "agent_boot_check $name" "$rc" "$out" "ALL PASS"; fi
  done
fi
lock=$(sudo dmesg | grep -aiE 'soft lockup|rcu.*stall|BUG:|Oops|WARNING:' | tail -3)
[ -n "$lock" ] && { echo ">> host dmesg warnings:"; echo "$lock"; }
echo "side-suite $MODE ($DEV, srcversion $(cat /sys/module/$(basename "$DEV")/srcversion 2>/dev/null)): $([ $fail = 0 ] && echo "ALL PASS ($pass checks)" || echo "FAIL ($fail of $((pass+fail)))")"
