#!/bin/bash
# smp_after.sh <pid> -- after process <pid> (a run.py cell) exits, one step at a time (never more
# than 2 VMs):
#  1. second-CPU check: the lists in $OUT/diag/<defect>.list (cell b's detecting schedules, the
#     confirm trigger, three no-concurrency schedules) on 2 vCPUs, through a tree whose hvx_explore
#     has the stderr-only HVX_EXPLORE_VCPUSTAT diagnostic ($DIAG, default ~/hydo-bench2/diag/hvx);
#  2. the d9 Q=2 probe on 1 and 2 vCPUs (listrun.py, every schedule twice);
#  3. the remaining cells (smp_cells.sh ${CELLS:-c rb rd}).
cd "$(dirname "$0")" || exit 1
OUT=${OUT:-/var/tmp/bench2}
DIAG=${DIAG:-$HOME/hydo-bench2/diag/hvx}
while kill -0 "$1" 2>/dev/null; do sleep 30; done
for d in d8-no-order-lock d9-redis-unlocked d12-shared-key-buffer; do
    echo "=== diag $d $(date -u +%FT%TZ)"
    python3 "$DIAG/bench/listrun.py" --images explore-bench-m59-$d-pin-disk.img --list "$OUT/diag/$d.list" \
        --workers 2 --vcpus 2 --env HVX_EXPLORE_VCPUSTAT=1 --out "$OUT/diag/$d"
done
IMG=explore-bench-m59-d9-redis-unlocked-pin-disk.img
echo "=== probe d9 Q=2 on 1 vCPU $(date -u +%FT%TZ)"
python3 listrun.py --images $IMG --list smp-d9-q2-v1.list --workers 2 --repeat 2 --out $OUT/probe-v1
echo "=== probe d9 Q=2 on 2 vCPUs $(date -u +%FT%TZ)"
python3 "$DIAG/bench/listrun.py" --images $IMG --list smp-d9-q2.list --workers 2 --repeat 2 --vcpus 2 \
    --env HVX_EXPLORE_VCPUSTAT=1 --out $OUT/probe-v2
bash smp_cells.sh ${CELLS:-c rb rd}
