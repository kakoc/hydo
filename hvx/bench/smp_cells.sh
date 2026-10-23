#!/bin/bash
# smp_cells.sh [cell...] -- the several-vCPU benchmark cells of SMP-RESULTS.md, one after another
# (one run.py at a time: 1 worker + its control VM = 2 VMs). Default: every cell in the order below.
#   a  swarm, 1 vCPU, --concurrency          b  swarm, 2 vCPUs --turns, --concurrency
#   d  swarm, 2 vCPUs --turns, no concurrency c  swarm, 1 vCPU, no concurrency
#   rb / rd  as b / d with strategy random
# Results: $OUT/<cell>/results.jsonl (default OUT=/var/tmp/bench2).
cd "$(dirname "$0")" || exit 1
OUT=${OUT:-/var/tmp/bench2}
DEF=${DEF:-d8-no-order-lock,d9-redis-unlocked,d12-shared-key-buffer}
common=(--defects "$DEF" --reps 1-3 --budget 60 --workers 1 --window 1)
cells=("$@")
[ ${#cells[@]} -eq 0 ] && cells=(a b d c rb rd)
for c in "${cells[@]}"; do
    case $c in
        a)  args=(--strategy swarm --concurrency) ;;
        b)  args=(--strategy swarm --concurrency --vcpus 2 --turns) ;;
        c)  args=(--strategy swarm) ;;
        d)  args=(--strategy swarm --vcpus 2 --turns) ;;
        rb) args=(--strategy random --concurrency --vcpus 2 --turns) ;;
        rd) args=(--strategy random --vcpus 2 --turns) ;;
        *)  echo "unknown cell $c" >&2; exit 2 ;;
    esac
    echo "=== cell $c: ${args[*]} $(date -u +%FT%TZ)"
    python3 run.py "${args[@]}" "${common[@]}" --out "$OUT/$c"
done
echo "=== all done $(date -u +%FT%TZ)"
