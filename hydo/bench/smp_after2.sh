#!/bin/bash
# smp_after2.sh <pid> -- after process <pid> exits: d9 Q=2 on 1 vCPU with tick jitter / shorter
# period / inserted ticks, the crashing 2-vCPU Q=2 schedules on the control, then the cells
# ${CELLS:-rb rd}. One step at a time (never more than 2 VMs).
cd "$(dirname "$0")" || exit 1
BENCH=${HYDO_BENCH_BIN:-$(command -v hydo-bench || echo ../../target/release/hydo-bench)}   # run.py/listrun.py until M108
OUT=${OUT:-/var/tmp/bench2}
while kill -0 "$1" 2>/dev/null; do sleep 30; done
echo "=== probe d9 Q=2 on 1 vCPU with timing levers $(date -u +%FT%TZ)"
"$BENCH" list --images explore-bench-m59-d9-redis-unlocked-pin-disk.img --list smp-d9-q2-v1-timing.list \
    --workers 2 --repeat 2 --out $OUT/probe-v1t
echo "=== probe crashing 2-vCPU Q=2 schedules on the control $(date -u +%FT%TZ)"
"$BENCH" list --images explore-bench-m59-control-pin-disk.img --list smp-d9-q2-control.list \
    --workers 2 --vcpus 2 --out $OUT/probe-ctl
bash smp_cells.sh ${CELLS:-rb rd}
