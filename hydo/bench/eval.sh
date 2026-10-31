#!/usr/bin/env bash
# hydo/bench/eval.sh -- the M46b strategy comparison on the bug benchmark (run on the box).
# Strategies one after another (each uses all the workers); results in $OUT/<strategy>/.
# The runner is hydo-bench (hydo/crates/hydo-bench; run.py until M108): $HYDO_BENCH_BIN, else
# hydo-bench on PATH, else the tree's target/release/hydo-bench. M46's fifth column, portfolio,
# came from the archived portfolio.py, removed in M108.
set -u
cd "$(dirname "$0")"
BENCH=${HYDO_BENCH_BIN:-$(command -v hydo-bench || echo ../../target/release/hydo-bench)}
OUT=${OUT:-/var/tmp/m46-bench}
W=${W:-3}
REPS=${REPS:-1-3}
BUDGET=${BUDGET:-60}
for s in "random" "swarm" "random --new-faults" "swarm --new-faults"; do
  name=$(echo "$s" | sed 's/ --new-faults/-nf/')
  echo "$(date) start $name" >> "$OUT/progress.log"
  "$BENCH" run --strategy $s --defects all --reps "$REPS" --budget "$BUDGET" --workers "$W" \
    --out "$OUT/$name" > "$OUT/$name.out" 2> "$OUT/$name.err"
  echo "$(date) done $name rc=$?" >> "$OUT/progress.log"
done
"$BENCH" table "$OUT/random" "$OUT/swarm" "$OUT/random-nf" "$OUT/swarm-nf" > "$OUT/table.md"
echo "$(date) all done" >> "$OUT/progress.log"
