#!/usr/bin/env bash
# evaluate.sh -- does coverage-guided exploration beat random at the same budget? (M41)
# Run on the box from ~/hydo/hvx/explore. Writes everything under $OUT (default /var/tmp/g).
#   (a) time-to-bug: the OLD buggy front door (explore-ptold); random vs guided; REPS starts
#       each; runs until the first durable-defaults failure (capped at CAP).
#   (b) coverage growth: the FIXED front door (explore-ptfix); guided vs random to CAP runs.
set -u
B=~/hydo/build
OUT="${OUT:-/var/tmp/g}"
REPS="${REPS:-3}"
CAP="${CAP:-90}"
GROW="${GROW:-150}"
cd "$(dirname "$0")"
mkdir -p "$OUT"

echo "== (a) time-to-bug on the OLD buggy image, cap $CAP, $REPS reps =="
for mode in random guided; do
  for r in $(seq 1 "$REPS"); do
    d="$OUT/ttb-$mode-$r"; sudo rm -rf "$d"
    python3 guided.py --mode "$mode" --image explore-ptold-disk.img --elf "$B/explore-ptold-hydostack" \
      --out "$d" --budget "$CAP" --workers 3 --rng0 "$((100 + r))" --stop-on durable-defaults \
      > "$d.json" 2> "$d.err"
    echo "$mode rep $r: $(cat "$d.json")"
  done
done

echo "== (b) coverage growth on the FIXED image, $GROW runs =="
for mode in random guided; do
  d="$OUT/grow-$mode"; sudo rm -rf "$d"
  python3 guided.py --mode "$mode" --image explore-ptfix-disk.img --elf "$B/explore-ptfix-hydostack" \
    --out "$d" --budget "$GROW" --workers 3 --rng0 1 > "$d.json" 2> "$d.err"
  echo "$mode: $(cat "$d.json")"
done
echo "EVAL-DONE"
