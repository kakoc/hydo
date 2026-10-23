#!/bin/bash
# smp_after.sh <pid> -- after process <pid> (a run.py cell) exits: the d9 Q=2 probe on 1 and 2 vCPUs
# (listrun.py, 2 workers, every schedule twice), then the remaining cells (smp_cells.sh c rb rd).
# One step at a time, so never more than 2 VMs.
cd "$(dirname "$0")" || exit 1
OUT=${OUT:-/var/tmp/bench2}
while kill -0 "$1" 2>/dev/null; do sleep 30; done
IMG=explore-bench-m59-d9-redis-unlocked-pin-disk.img
echo "=== probe d9 Q=2 on 1 vCPU $(date -u +%FT%TZ)"
python3 listrun.py --images $IMG --list smp-d9-q2-v1.list --workers 2 --repeat 2 --out $OUT/probe-v1
echo "=== probe d9 Q=2 on 2 vCPUs $(date -u +%FT%TZ)"
python3 listrun.py --images $IMG --list smp-d9-q2.list --workers 2 --repeat 2 --vcpus 2 --out $OUT/probe-v2
bash smp_cells.sh ${CELLS:-c rb rd}
