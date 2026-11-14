#!/bin/sh
# Measure the finding: N crash restarts and N clean restarts of PostgreSQL on a fresh stack,
# alternating, and how many of each gave a period with two leaders' periodic jobs.
#   N=10 sh measure.sh
set -eu
cd "$(dirname "$0")"
N=${N:-10}
sh stack.sh down >/dev/null
sh stack.sh up
trap 'sh stack.sh down' EXIT
sleep 20
crash=0
graceful=0
k=0
while [ $k -lt "$N" ]; do
  k=$((k + 1))
  for m in crash graceful; do
    out=$(MODE=$m sh probe-restart.sh)
    echo "$out" | sed "s/^/[$k] /"
    d=$(echo "$out" | sed -n "s/^$m: \([0-9]*\) periods.*/\1/p")
    if [ "${d:-0}" -gt 0 ]; then
      if [ $m = crash ]; then crash=$((crash + 1)); else graceful=$((graceful + 1)); fi
    fi
    sleep 5
  done
done
echo "measure.sh: two leaders' periodic jobs after $crash of $N crash restarts and $graceful of $N clean restarts"
