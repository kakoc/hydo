#!/bin/sh
# Temporal 1.32.2 stops running workflows for as long as the clock was stepped back, after one
# crash of PostgreSQL (RESULTS.md, finding 2). Plain Docker, no hydo, no patches.
#
#   sh repro-clock-db.sh            the clock goes back STEP seconds (default 3600), then
#                                   PostgreSQL's server is killed and comes back 5 s later;
#                                   the script starts one order every 10 s for PROBE seconds
#                                   (default 180), checks which ones completed, and puts the
#                                   clock forward again
#   CONTROL=1 sh repro-clock-db.sh  the same kill without the clock step
#
# The clock belongs to the machine that runs the containers, so run this on a Linux machine or
# VM of your own where root may set the clock, with its time synchronisation stopped: every
# container there sees the step. CLOCK names the command prefix that runs `date` as root on
# that machine (default: sudo). README.md says how to use a Colima VM on a Mac. KEEP=1 leaves
# the containers running. Needs ../hydo/bin/tw for the machine's architecture (sh ../build.sh).
set -eu
cd "$(dirname "$0")"
P=${PROJECT:-hydo-native-temporal}
STEP=${STEP:-3600}
PROBE=${PROBE:-180}
CLOCK=${CLOCK:-sudo}
DC="docker compose -p $P -f compose.yaml"
T() { $DC exec -T admin temporal "$@" --namespace default --address temporal:7233; }
now() { $CLOCK date -u +%Y-%m-%dT%H:%M:%SZ; }
status() { T workflow describe --workflow-id "$1" -o json 2>/dev/null |
  grep -o '"status": *"WORKFLOW_EXECUTION_STATUS_[A-Z_]*"' | head -1 |
  sed 's/.*WORKFLOW_EXECUTION_STATUS_\([A-Z_]*\)"$/\1/'; }
order() { T workflow start --type Order --task-queue orders --workflow-id "$1" \
  --input '{"TimerMs":1000}' >/dev/null 2>&1; }

cleanup() {
  [ "${KEEP:-0}" = 1 ] || $DC down -v >/dev/null 2>&1 || true
}
trap cleanup EXIT

[ -x ../hydo/bin/tw ] || { echo "build the worker first: sh ../build.sh" >&2; exit 1; }
echo "== start Temporal 1.32.2, PostgreSQL 16 and the worker ($(docker --version))"
$DC up -d >/dev/null 2>&1
i=0
until T workflow list --limit 1 >/dev/null 2>&1; do
  i=$((i + 1)); [ $i -lt 90 ] || { echo "the server did not answer in 180 s" >&2; exit 1; }
  sleep 2
done
order before
i=0
until [ "$(status before)" = COMPLETED ]; do
  i=$((i + 1)); [ $i -lt 30 ] || { echo "the first order did not complete in 60 s" >&2; exit 1; }
  sleep 2
done
echo "server and worker ready: order 'before' COMPLETED; clock $(now)"

if [ "${CONTROL:-0}" = 1 ]; then
  echo "== control: no clock step"
else
  echo "== step the clock back $STEP s"
  $CLOCK date -u -s "@$(( $($CLOCK date +%s) - STEP ))" >/dev/null
  echo "clock now $(now)"
fi
echo "== kill PostgreSQL's server; its container starts it again 5 s later"
$DC exec -T postgresql sh -c 'kill -9 $(head -1 /var/lib/postgresql/data/postmaster.pid)'
since=$(date +%s)
i=0
until $DC exec -T postgresql pg_isready -U temporal -h localhost -p 5432 >/dev/null 2>&1; do
  i=$((i + 1)); [ $i -lt 60 ] || { echo "PostgreSQL did not come back in 60 s" >&2; exit 1; }
  sleep 1
done
echo "PostgreSQL answers again after $(( $(date +%s) - since )) s"

n=0 started=0
end=$(( $(date +%s) + PROBE ))
while [ "$(date +%s)" -lt $end ]; do
  n=$((n + 1))
  if order "probe-$n"; then r=started; started=$((started + 1)); else r="not started"; fi
  printf '%4ss  order probe-%s: %s; order probe-%s: %s\n' "$(( $(date +%s) - since ))" "$n" "$r" \
    "$((n - 1))" "$([ $n -gt 1 ] && status "probe-$((n - 1))" || echo -)"
  sleep 10
done
sleep 10
done=0
for k in $(seq 1 $n); do [ "$(status "probe-$k")" = COMPLETED ] && done=$((done + 1)); done
logs=$($DC logs temporal 2>&1)
throttled=$(printf '%s\n' "$logs" | grep -c 'did not refresh database connection pool because the last refresh was too close' || true)
unusable=$(printf '%s\n' "$logs" | grep -c 'no usable database connection found' || true)

if [ "${CONTROL:-0}" != 1 ]; then
  echo "== put the clock forward again"
  $CLOCK date -u -s "@$(( $($CLOCK date +%s) + STEP ))" >/dev/null
  echo "clock now $(now)"
fi
echo "== result: $started of $n orders started, $done completed within $((PROBE + 10)) s of PostgreSQL's return"
echo "   server log: $throttled x 'did not refresh database connection pool because the last refresh was too close'"
echo "   server log: $unusable x 'no usable database connection found'"
