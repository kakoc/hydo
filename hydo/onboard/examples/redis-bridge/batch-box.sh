#!/bin/sh
# M95 helper for the bare-metal test machine: run-box.sh over a list, two VMs at a time.
#   batch-box.sh <list>      list lines: <tag>|<image>|<ram-mib>|<schedule>
# A finished tag (its .out exists) is skipped, so a re-run continues. Pid in $OUT/batch.<list name>.pid.
set -u
L=$1
OUT=${OUT:-/var/tmp/m95-netns}
H=$(cd "$(dirname "$0")" && pwd)
echo $$ > "$OUT/batch.$(basename "$L").pid"
pids=""
while IFS='|' read -r tag img ram sched; do
  [ -n "$tag" ] || continue
  [ -f "$OUT/$tag.out" ] && continue
  "$H/run-box.sh" "$tag" "$img" "$ram" "$sched" &
  pids="$pids $!"
  set -- $pids
  if [ $# -ge 2 ]; then wait $1; shift; pids="$*"; fi
done < "$L"
wait
rm -f "$OUT/batch.$(basename "$L").pid"
