#!/bin/bash
# The stage-1/2 scheduler tests against the side module, with their recorded numbers
# (hv/hvx/SMP_STAGE1.md, hv/hvx/SMP_STAGE2.md), plus the burst test (hv/hvx/FASTTURN.md).
# On the box, from ~/hydo-ft/hvx:
#   fastturn/stage1.sh [/dev/hvxft]
# FT_MARGIN=<n> passes a turn-end margin to the tests (hvx_sched_util.h applies it).
set -u
DEV=${1:-/dev/hvxft}
T=/var/tmp/fastturn; mkdir -p $T
cd "$(dirname "$0")/.."
pass=0; fail=0
for t in hvx_sched_pmu_test hvx_sched_race_test hvx_sched_sweep_test hvx_sipi_test hvx_sched_burst_test; do
  cc -O2 -Wall -o $T/st-$t $t.c 2>/dev/null || { echo "COMPILE FAIL $t"; fail=$((fail+1)); }
done
chk() { local name=$1; shift; local out; out=$(cat); local miss=""
  for fp in "$@"; do grep -q "$fp" <<< "$out" || miss="$miss $fp"; done
  if grep -q "PASS" <<< "$out" && [ -z "$miss" ]; then pass=$((pass+1)); echo "  PASS $name"
  else fail=$((fail+1)); echo "  FAIL $name missing:$miss"; echo "$out" | tail -5; fi; }
R="$HOME/bin/hvx-run sudo env HVX_DEV=$DEV ${FT_MARGIN:+FT_MARGIN=$FT_MARGIN}"
chk pmu 97b3468c16d739fa f841184e6fdde8b5 19337dfb08c526f9 95e6c9a02d5d584d 05521a9492a3ce9f b544e114dc426e85 73a465516c4e0094 < <($R timeout 300 $T/st-hvx_sched_pmu_test 2>&1)
chk race 8cb7c61a2393f44e < <($R timeout 300 $T/st-hvx_sched_race_test 2>&1)
chk sweep 39971eed9c2f35fe < <($R timeout 900 $T/st-hvx_sched_sweep_test 2000 2>&1)
chk sipi eef5c1ebd4df2e7d c8d015ab3b57252d < <($R timeout 300 $T/st-hvx_sipi_test 2>&1)
chk burst ${FT_BURST_FP:-} < <($R timeout 600 $T/st-hvx_sched_burst_test 2>&1)
echo "stage1 ($DEV${FT_MARGIN:+, margin $FT_MARGIN}): $([ $fail = 0 ] && echo "ALL PASS ($pass)" || echo "FAIL ($fail of $((pass+fail)))")"
