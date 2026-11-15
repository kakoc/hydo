#!/bin/sh
# Look again, at least 6 minutes after the probes: for each probe line in the file given (the
# output of measure.sh), how many of its scheduled runs have still not run. Hatchet's
# PostgreSQL queue drops a message 5 minutes after it was sent, so a run that has not run by then
# never will.
#
#   sh verify.sh measure.out
set -u
cd "$(dirname "$0")/.." || exit 2
P=${HQ_PROJECT:-hqnative}
grep '^tag=' "$1" | while read -r line; do
  tag=$(echo "$line" | sed 's/^tag=\([0-9]*\).*/\1/')
  n=$(echo "$line" | sed 's/.*scheduled=\([0-9]*\).*/\1/')
  ran=$(docker compose -p "$P" exec -T postgres psql -U hatchet -d app -tAc \
    "SELECT count(DISTINCT m) FROM app_run WHERE m LIKE 's${tag}\_%' AND outcome = 'ok'" < /dev/null)
  echo "$line never=$((n - ran))"
done
