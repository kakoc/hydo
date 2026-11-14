#!/bin/sh
# Run test_wal_replay_loads_pending_to_queue many times, one test per process, as cargo-nextest
# does in Qdrant's CI, and count the failures and the queue lengths the test read.
#   sh run-native.sh <program> [runs] [parallel]
# <program> is a test program of the crate `collection` (build-program.sh, or ../build-test.sh on
# Linux x86_64). runs: 100 by default. parallel: processes at once, 1 by default.
# Settings, each optional:
#   CPUS=0           run every process under `taskset -c 0` (Linux only), as the one-CPU rows
#   BUSY=6           start that many busy shell loops beside the runs (under CPUS too when set)
#   TOKIO_WORKER_THREADS=1   tokio's worker threads, passed through to the test
#   TMPDIR=/dev/shm  where the test writes its data (Qdrant's Linux CI uses tmpfs)
# Writes work/runs/<n>.out and prints `failed F of N` and a count of each queue length.
set -u
cd "$(dirname "$0")"
PROG=${1:?usage: sh run-native.sh <program> [runs] [parallel]}
RUNS=${2:-100}
PAR=${3:-1}
case $PROG in /*) ;; *) PROG=$(pwd)/$PROG ;; esac
TEST=tests::wal_recovery_test::test_wal_replay_loads_pending_to_queue
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
while [ "$n" -lt "$RUNS" ]; do
  batch=
  k=0
  while [ "$k" -lt "$PAR" ] && [ "$n" -lt "$RUNS" ]; do
    n=$((n + 1))
    $PIN "$PROG" --exact "$TEST" --test-threads 1 --nocapture > "$OUT/$n.out" 2>&1 &
    batch="$batch $!"
    k=$((k + 1))
  done
  for p in $batch; do wait "$p"; done
done

failed=$(grep -L '^test result: ok. 1 passed' "$OUT"/*.out | wc -l | tr -d ' ')
echo "failed $failed of $RUNS"
echo "queue lengths after the load (count length):"
grep -h -m1 'Post-load update queue info' "$OUT"/*.out | sed -n 's/.*length: \([0-9]*\).*/\1/p' | sort -n | uniq -c
