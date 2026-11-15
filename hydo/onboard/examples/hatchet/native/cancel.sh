#!/bin/sh
# Finding 2 without hydo, on the running stack (sh stack.sh up): N times, trigger a run of the
# task `slow` that does not look at its cancellation (1 to 3 s of work), cancel it through the API
# 0.2 to 1.5 s later, read its status until the API reports CANCELLED, and read the status again
# after the work would have ended.
#
#   N=10 sh cancel.sh                          # the control: no fault
#   STOP=hatchet-engine-1 N=10 sh cancel.sh    # SIGKILL that engine 1.6 s after each trigger,
#                                              # and start it again 10 s later
set -u
cd "$(dirname "$0")/.." || exit 2
P=${HQ_PROJECT:-hqnative}
N=${N:-10}
STOP=${STOP:-}
i=0
while [ $i -lt "$N" ]; do
  i=$((i + 1))
  docker compose -p "$P" up -d hatchet-engine-1 hatchet-engine-2 > /dev/null 2>&1
  sleep 12
  out=$(mktemp)
  # the client talks to engine 2, so that a stop of engine 1 never fails the trigger itself
  timeout 120 docker compose -p "$P" run --rm --no-deps -e HYDO_HIST_OUT=off -e HQ_STUBBORN=1 \
    -e HATCHET_CLIENT_HOST_PORT=hatchet-engine-2:7070 \
    -e HYDO_CMD_ID="$(( $(date +%s) % 100000 ))$i" -e HYDO_SEED="$i" worker-2 op cancel < /dev/null > "$out" 2>/dev/null &
  pid=$!
  if [ -n "$STOP" ]; then
    sleep 1.6
    docker kill -s KILL "$P-$STOP-1" > /dev/null
    sleep 10
    docker compose -p "$P" up -d "$STOP" > /dev/null 2>&1
  fi
  wait $pid
  grep -E '^(cancel of|then) ' "$out" | tr '\n' ' '
  echo
  rm -f "$out"
done
