#!/bin/bash
# M156, run ON the bare-metal machine: N rounds of a sweep program, one VM at a time, each through
# ~/bin/hvx-run; a round runs the program once against each device of the comma-separated DEVS,
# in that order (so two modules are compared under the same conditions, alternately).
#   series.sh LABEL DEVS N PROGRAM [ARGS...]
# Logs /var/tmp/m156/LABEL/<round>-<device>.log, one line per run in summary.txt, `done` at the
# end; its pid in /var/tmp/m156/LABEL/pid (stop it with kill "$(cat pid)" and nothing else).
set -u
LABEL=$1; DEVS=$2; N=$3; shift 3
D=/var/tmp/m156/$LABEL; mkdir -p "$D"; echo $$ > "$D/pid"; rm -f "$D/done"
date -u > "$D/started"
for i in $(seq -w 1 "$N"); do
  for dev in ${DEVS//,/ }; do
    tag=$i-$(basename "$dev"); s=$(date +%s)
    ~/bin/hvx-run sudo env HVX_DEV="$dev" timeout 1200 "$@" > "$D/$tag.log" 2>&1; rc=$?
    echo "$tag rc=$rc secs=$(( $(date +%s) - s )) mismatches=$(grep -c MISMATCH "$D/$tag.log") $(grep -a 'sweep' "$D/$tag.log" | tail -1)" >> "$D/summary.txt"
  done
done
date -u > "$D/done"
