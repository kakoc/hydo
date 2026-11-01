#!/bin/bash
# Run a list of schedules through one engine program in serve mode (target/release/hydo-hvx; it
# was hvx_explore until M137) and keep its stdout
# (READY / RESULT lines) and stderr (PHBOOT / PHASES lines). Measurement only (PERF-RUN-COST.md).
#
#   perf/serve.sh <engine binary> <image under $BUILD> <outdir> <schedules file> [VAR=value ...]
#
# The schedules file has one "<tag> <schedule>" per line. VAR=value pairs go into the engine's
# environment (for example HVX_EXPLORE_PHASES=1, HVX_EXPLORE_DIRTY=1). The engine runs under
# ~/bin/hvx-run (the shared module lock) with a 2-hour timeout; its pid is written to
# <outdir>/engine.pid so that it can be stopped by pid, never by name.
set -u
ENGINE=$1; IMAGE=$2; OUT=$3; LIST=$4; shift 4
BUILD=${BUILD:-$HOME/hydo/build}
SNAP=${HYDO_SNAP_DIR:-/var/tmp/hvx-snap}
mkdir -p "$OUT/consoles"
{
  while read -r tag sched; do
    [ -z "$tag" ] && continue
    echo "RUN $tag $sched"
  done < "$LIST"
  echo QUIT
} > "$OUT/input"
start=$(date +%s.%N)
timeout 7200 ~/bin/hvx-run sudo env HVX_SNAP_DIR="$SNAP" "$@" "$ENGINE" ${ENGINE_ARGS:-} serve \
  "$BUILD/bzImage" "$BUILD/initramfs-kafka.cpio.gz" "$BUILD/$IMAGE" "$OUT/consoles" \
  < "$OUT/input" > "$OUT/stdout" 2> "$OUT/stderr" &
echo $! > "$OUT/engine.pid"
wait $!
rc=$?
end=$(date +%s.%N)
echo "rc=$rc wall=$(echo "$end - $start" | bc)" > "$OUT/done"
