#!/bin/sh
# Finding 1 without hydo: a backward step of the primary's wall clock, while the primary waits
# repl-diskless-sync-delay (5 s) before it starts a replica's full sync, delays that sync by the
# size of the step.
#
#   sh fullsync-clock-step.sh                  # the steps +0, -60s and -120s
#   sh fullsync-clock-step.sh -3600s           # one step of an hour (gives up after LIMIT s)
#
# For each step: a fresh primary with 1,000 keys, a fresh replica, the step one second after the
# replica connects, and the time until the replica reports a finished sync. Needs Docker with
# Compose; builds the image of ./Dockerfile once. Removes its containers and volumes at the end.
set -eu
cd "$(dirname "$0")"
LIMIT=${LIMIT:-200}
docker compose build -q primary
info() { docker compose exec -T "$1" valkey-cli INFO replication | tr -d '\r'; }
for step in ${@:-+0 -60s -120s}; do
  docker compose --profile replica down -v > /dev/null 2>&1
  docker compose up -d primary > /dev/null 2>&1
  docker compose exec -T primary sh -c 'echo +0 > /clock/offset'
  n=0
  until [ "$(docker compose exec -T primary valkey-cli PING 2>/dev/null | tr -d '\r')" = PONG ]; do
    n=$((n + 1)); [ $n -lt 30 ] || { echo "the primary does not start" >&2; exit 1; }; sleep 1
  done
  docker compose exec -T primary sh -c 'for i in $(seq 1 1000); do echo "SET k$i v$i"; done | valkey-cli > /dev/null'
  docker compose --profile replica up -d replica > /dev/null 2>&1
  sleep 1
  docker compose exec -T primary sh -c "echo $step > /clock/offset"
  t0=$(date +%s)
  sleep 3
  echo "step $step: 3 s later the primary shows: $(info primary | grep '^slave0:')"
  synced=no
  while [ $(( $(date +%s) - t0 )) -lt "$LIMIT" ]; do
    r=$(info replica)
    if echo "$r" | grep -q '^master_link_status:up' && echo "$r" | grep -q '^master_sync_in_progress:0'; then
      synced=yes; break
    fi
    sleep 1
  done
  keys=$(docker compose exec -T replica valkey-cli DBSIZE | tr -d '\r')
  echo "step $step: replica synced=$synced after $(( $(date +%s) - t0 )) s, replica DBSIZE $keys"
done
docker compose --profile replica down -v > /dev/null 2>&1
