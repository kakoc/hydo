#!/bin/bash
# M107: a research/measurement tool. It needs the Python explorer and builder, deleted in M107
# (replaced by the hydo-explore and hydo-build crates): restore them from commit 2a58c4b with
# git checkout 2a58c4b -- hydo/explore hydo/onboard hydo/history in a scratch checkout, and run it there.
# One point of the throughput-scaling measurement (PERF-RUN-COST.md, item 3): explore.py with N
# workers on a fixed seed range, no shrinking, the engines printing their PHASES lines, vmstat
# sampled every 2 s. With N > 2 it holds the quiet window /var/tmp/hydo-QUIET (other sessions
# start no new VM while it exists) and limits the run to 18 minutes.
#
#   perf/scale.sh <N> <seeds lo-hi> <out dir> <explore.py args ...>
#
# PERF_EXTRA_ENV (for example HVX_SNAP_MALLOC=1) goes into every engine's environment.
set -u
N=$1; SEEDS=$2; OUT=$3; shift 3
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
QUIET=/var/tmp/hydo-QUIET
LIMIT=7200
if [ "$N" -gt 2 ]; then
  [ -e "$QUIET" ] && { echo "quiet file exists already; not starting" >&2; exit 1; }
  echo "perf-measure scaling N=$N $(date -u +%FT%TZ)" > "$QUIET"
  LIMIT=1080
fi
vmstat -t 2 > "$OUT/vmstat.txt" & echo $! > "$OUT/vmstat.pid"
free -m > "$OUT/free-before.txt"
cd "$HERE/../../../hydo/explore"
HYDO_HVX_RUN="$HERE/hvx-run-phases" timeout "$LIMIT" /usr/bin/time -v -o "$OUT/time.txt" \
  python3 "$HERE/pyrusage.py" explore.py --workers "$N" --seeds "$SEEDS" --no-shrink --out "$OUT" "$@" > "$OUT/explore.out" 2>&1 &
echo $! > "$OUT/explore.pid"
wait "$(cat "$OUT/explore.pid")"
echo "rc=$?" > "$OUT/done"
kill "$(cat "$OUT/vmstat.pid")" 2>/dev/null
free -m > "$OUT/free-after.txt"
if [ "$N" -gt 2 ]; then
  # the engines exit when explore.py's pipes close; wait (at most 5 minutes) for this run's
  # engines before giving the box back, matching them by our own out dir only
  for _ in $(seq 60); do
    pgrep -u root -f "hvx_explore.* $OUT/consoles" > /dev/null || break
    sleep 5
  done
  rm -f "$QUIET"
fi
