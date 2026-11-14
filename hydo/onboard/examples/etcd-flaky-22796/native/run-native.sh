#!/bin/sh
# Run TestCacheCompactionResync without a CPU limit, as the rows of RESULTS.md that never failed:
#   sh run-native.sh <program> [processes] [count] [parallel]
# Each process runs `-test.count=<count> -test.cpu=1`, as the CI job's `CPU=1`. Defaults: 100
# processes of 10, one at a time. Settings, each optional:
#   CPUS=0    run every process under `taskset -c 0` (Linux only)
#   BUSY=8    start that many busy shell loops beside the runs (under CPUS too when set)
# Writes work/runs/<n>.out and prints the test runs and the failures.
set -u
cd "$(dirname "$0")"
PROG=${1:?usage: sh run-native.sh <program> [processes] [count] [parallel]}
PROCS=${2:-100}
COUNT=${3:-10}
PAR=${4:-1}
case $PROG in /*) ;; *) PROG=$(pwd)/$PROG ;; esac
PIN=
if [ -n "${CPUS:-}" ]; then
  command -v taskset >/dev/null || { echo "run-native.sh: CPUS needs taskset (Linux)" >&2; exit 1; }
  PIN="taskset -c $CPUS"
fi
OUT=work/runs
rm -rf "$OUT"; mkdir -p "$OUT"
PIDS=
cleanup() { for p in $PIDS; do kill "$p" 2>/dev/null; done; }
trap cleanup EXIT INT TERM
i=0
while [ "$i" -lt "${BUSY:-0}" ]; do
  $PIN sh -c 'while :; do :; done' &
  PIDS="$PIDS $!"
  i=$((i + 1))
done
n=0
while [ "$n" -lt "$PROCS" ]; do
  batch=
  k=0
  while [ "$k" -lt "$PAR" ] && [ "$n" -lt "$PROCS" ]; do
    n=$((n + 1))
    $PIN "$PROG" -test.run '^TestCacheCompactionResync$' -test.count="$COUNT" -test.cpu=1 -test.v \
      > "$OUT/$n.out" 2>&1 &
    batch="$batch $!"
    k=$((k + 1))
  done
  for p in $batch; do wait "$p"; done
done
runs=$(cat "$OUT"/*.out | grep -c -e '^--- PASS: TestCacheCompactionResync' -e '^--- FAIL: TestCacheCompactionResync')
fails=$(cat "$OUT"/*.out | grep -c '^--- FAIL: TestCacheCompactionResync')
echo "failed $fails of $runs test runs"
grep -h 'expected cached revision' "$OUT"/*.out | sed 's/^ *//' | sort | uniq -c
