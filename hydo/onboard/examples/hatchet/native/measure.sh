#!/bin/sh
# Finding 1 without hydo: R rounds of probes on the running stack (sh stack.sh up). Each round
# runs one SIGKILL, one SIGTERM and one control without a stop, alternating the two engines and
# moving the stop between 0.2 and 1.0 s after the instant the runs are due. Then, 6 minutes
# after the last probe, verify.sh counts the runs that never ran.
#
#   R=6 sh measure.sh > measure.out; cat measure.out
set -u
cd "$(dirname "$0")"
R=${R:-6}
A=; [ "${ALONE:-0}" = 1 ] && A=alone-   # ALONE=1: the stopped engine is the only one that reads the queue
N=${N:-150}   # scheduled runs per probe (with N=600 also set AT_MS=45000)
OFFSETS=${OFFSETS:-0.2 0.4 0.6 0.8 1.0}   # five offsets of the stop, in seconds
out=$(mktemp)
i=0
while [ $i -lt "$R" ]; do
  eng=hatchet-engine-$((1 + i % 2))
  after=$(echo "$OFFSETS" | cut -d' ' -f$((1 + i % 5)))
  bash probe.sh "${A}kill" "$eng" "$N" "$after" | tee -a "$out" >&2
  bash probe.sh "${A}term" "$eng" "$N" "$after" | tee -a "$out" >&2
  bash probe.sh none "$eng" "$N" "$after" | tee -a "$out" >&2
  i=$((i + 1))
done
sleep 360
sh verify.sh "$out"
rm -f "$out"
