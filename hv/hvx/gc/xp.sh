#!/bin/bash
# M138, on the bare-metal test machine: one exploration with this tree's explorer, the engine in
# the explorer's process on the side module (HVX_DEV, default /dev/hvxgc), the whole explorer
# under ~/bin/hvx-run sudo. Extra NAME=value words before `--` go into the environment (for
# example HYDO_HVX_REUSE=1).
#   xp.sh <out> [NAME=value ...] -- <explorer args...>
# Appends "exit=<rc> wall=<seconds>" to <out>.log.
set -u
out=$1; shift
envs=()
while [ $# -gt 0 ] && [ "$1" != -- ]; do envs+=("$1"); shift; done
[ "${1:-}" = -- ] && shift
T=$(cd "$(dirname "$0")/../../.." && pwd)
B=$HOME/hydo/build
sudo rm -rf "$out"; mkdir -p "$(dirname "$out")"
n=0; while [ -e /var/tmp/hydo-QUIET ] && [ $n -lt 720 ]; do sleep 10; n=$((n+1)); done
t0=$(date +%s.%N)
(cd "$T" && timeout -s KILL 14400 ~/bin/hvx-run sudo env HOME="$HOME" HVX_DEV="${HVX_DEV:-/dev/hvxgc}" \
  HYDO_ENGINE=inproc HYDO_BUILD="$B" HYDO_ROOT="$T" HYDO_QUIET_FILE=/var/tmp/hydo-QUIET HYDO_SNAPSHOTS=memory \
  "${envs[@]}" target/release/hydo-explore --out "$out" "$@" > "$out.log" 2>&1)
rc=$?
t1=$(date +%s.%N)
echo "exit=$rc wall=$(echo "$t1 - $t0" | bc)" >> "$out.log"
