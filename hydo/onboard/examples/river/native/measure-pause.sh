#!/bin/sh
# Measure finding 1 with the leader stopped across the restart (probe-pause.sh): N rounds of a
# crash restart, a clean restart and no restart, each with the leader stopped for PAUSE seconds,
# on a fresh stack, and how many of each gave a run of the periodic job inserted by two clients.
#   N=10 PAUSE=6 sh measure-pause.sh
set -eu
cd "$(dirname "$0")"
N=${N:-10}
sh stack.sh down >/dev/null
sh stack.sh up
trap 'sh stack.sh down' EXIT
sleep 20
crash=0
graceful=0
none=0
k=0
while [ $k -lt "$N" ]; do
  k=$((k + 1))
  for m in crash graceful none; do
    out=$(MODE=$m sh probe-pause.sh)
    echo "$out" | sed "s/^/[$k] /"
    d=$(echo "$out" | sed -n "s/^$m: \([0-9]*\) runs.*/\1/p")
    if [ "${d:-0}" -gt 0 ]; then
      case $m in crash) crash=$((crash + 1)) ;; graceful) graceful=$((graceful + 1)) ;; none) none=$((none + 1)) ;; esac
    fi
    sleep 5
  done
done
echo "measure-pause.sh: a run inserted by two clients after $crash of $N crash restarts, $graceful of $N clean restarts and $none of $N pauses alone"
