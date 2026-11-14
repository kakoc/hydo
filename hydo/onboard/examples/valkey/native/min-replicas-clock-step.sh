#!/bin/sh
# The same cause, a second effect: with min-replicas-to-write 1 and min-replicas-max-lag 10, a
# primary whose only replica stops answering must refuse writes after about 10 s (NOREPLICAS).
# After a backward step of the primary's wall clock, it accepts them for the size of the step
# longer.
#
#   sh min-replicas-clock-step.sh              # the steps +0 and -120s
#
# For each step: a fresh primary and replica, the replica frozen (docker compose pause), the step,
# then one SET a second until the primary refuses one. Removes its containers and volumes.
set -eu
cd "$(dirname "$0")"
LIMIT=${LIMIT:-200}
docker compose build -q primary
P() { docker compose exec -T primary valkey-cli "$@" | tr -d '\r'; }
for step in ${@:-+0 -120s}; do
  docker compose --profile replica down -v > /dev/null 2>&1
  docker compose up -d primary > /dev/null 2>&1
  docker compose exec -T primary sh -c 'echo +0 > /clock/offset'
  n=0
  until [ "$(P PING 2>/dev/null)" = PONG ]; do
    n=$((n + 1)); [ $n -lt 30 ] || { echo "the primary does not start" >&2; exit 1; }; sleep 1
  done
  docker compose --profile replica up -d replica > /dev/null 2>&1
  n=0
  until P INFO replication | grep -q 'state=online'; do
    n=$((n + 1)); [ $n -lt 60 ] || { echo "the replica does not sync" >&2; exit 1; }; sleep 1
  done
  P CONFIG SET min-replicas-to-write 1 > /dev/null
  P CONFIG SET min-replicas-max-lag 10 > /dev/null
  echo "step $step: before the pause SET answers $(P SET k 0)"
  docker compose pause replica > /dev/null 2>&1
  docker compose exec -T primary sh -c "echo $step > /clock/offset"
  t0=$(date +%s)
  ok=0
  while [ $(( $(date +%s) - t0 )) -lt "$LIMIT" ]; do
    r=$(P SET k "$(date +%s)")
    if [ "$r" = OK ]; then ok=$((ok + 1)); else break; fi
    sleep 1
  done
  echo "step $step: the primary acknowledged $ok writes in the $(( $(date +%s) - t0 )) s after its replica stopped; then: ${r:-}"
  docker compose unpause replica > /dev/null 2>&1
done
docker compose --profile replica down -v > /dev/null 2>&1
