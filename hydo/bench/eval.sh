#!/usr/bin/env bash
# hydo/bench/eval.sh -- the M46b strategy comparison on the bug benchmark (run on the box).
# Strategies one after another (each uses all the workers); results in $OUT/<strategy>/.
set -u
cd "$(dirname "$0")"
OUT=${OUT:-/var/tmp/m46-bench}
W=${W:-3}
REPS=${REPS:-1-3}
BUDGET=${BUDGET:-60}
for s in "random" "swarm" "random --new-faults" "swarm --new-faults" "portfolio"; do
  name=$(echo "$s" | sed 's/ --new-faults/-nf/')
  echo "$(date) start $name" >> "$OUT/progress.log"
  python3 run.py --strategy $s --defects all --reps "$REPS" --budget "$BUDGET" --workers "$W" \
    --out "$OUT/$name" > "$OUT/$name.out" 2> "$OUT/$name.err"
  echo "$(date) done $name rc=$?" >> "$OUT/progress.log"
done
python3 run.py --table "$OUT/random" "$OUT/swarm" "$OUT/random-nf" "$OUT/swarm-nf" "$OUT/portfolio" > "$OUT/table.md"
echo "$(date) all done" >> "$OUT/progress.log"
