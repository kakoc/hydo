#!/bin/sh
# Run TestCacheCompactionResync under a CPU limit that busy neighbours use up, as a CI pod's
# limit does. This is the native reproduction of RESULTS.md: with 10% and two busy loops it
# failed 100 of 100 runs, with 20% and one busy loop 90 of 100, and with 20% alone 0 of 100.
#   sh cpu-limit.sh <program> [quota] [busy] [runs]
#   sh cpu-limit.sh work/cache.test 10% 2 100
# Linux with systemd only. `systemd-run --user --scope -p CPUQuota=<quota>` puts this script's
# second half into a new cgroup of the user's own systemd instance, with the limit; it needs no
# root. The busy loops and every test process run in that cgroup, all on CPU 0 (`taskset -c 0`).
# Each test process runs `-test.count=1 -test.cpu=1`. Writes work/limit/<n>.out.
set -u
cd "$(dirname "$0")"
PROG=${1:?usage: sh cpu-limit.sh <program> [quota] [busy] [runs]}
QUOTA=${2:-10%}
BUSY=${3:-2}
RUNS=${4:-100}
case $PROG in /*) ;; *) PROG=$(pwd)/$PROG ;; esac

if [ "${CPU_LIMIT_INNER:-0}" != 1 ]; then
  command -v systemd-run >/dev/null || { echo "cpu-limit.sh: needs systemd-run (Linux with systemd)" >&2; exit 1; }
  command -v taskset >/dev/null || { echo "cpu-limit.sh: needs taskset" >&2; exit 1; }
  rm -rf work/limit; mkdir -p work/limit
  exec systemd-run --user --scope --quiet -p CPUQuota="$QUOTA" \
    env CPU_LIMIT_INNER=1 sh "$(pwd)/cpu-limit.sh" "$PROG" "$QUOTA" "$BUSY" "$RUNS"
fi

# Inside the scope.
echo "cgroup: $(cat /proc/self/cgroup)"
PIDS=
cleanup() { for p in $PIDS; do kill "$p" 2>/dev/null; done; }
trap cleanup EXIT INT TERM
i=0
while [ "$i" -lt "$BUSY" ]; do
  taskset -c 0 sh -c 'while :; do :; done' &
  PIDS="$PIDS $!"
  i=$((i + 1))
done
n=0
while [ "$n" -lt "$RUNS" ]; do
  n=$((n + 1))
  taskset -c 0 "$PROG" -test.run '^TestCacheCompactionResync$' -test.count=1 -test.cpu=1 -test.v \
    -test.timeout=60s > "work/limit/$n.out" 2>&1
done
fails=$(cat work/limit/*.out | grep -c '^--- FAIL: TestCacheCompactionResync')
echo "CPUQuota=$QUOTA busy=$BUSY: failed $fails of $RUNS"
grep -h 'expected cached revision' work/limit/*.out | sed 's/^ *//' | sort | uniq -c
