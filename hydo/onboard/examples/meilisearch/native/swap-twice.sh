#!/bin/sh
# swap-twice.sh [ROUNDS]: Meilisearch v1.57.0 in Docker, without hydo. Each round enqueues an
# index swap of sa and sb with a burst of document additions, kills the server with SIGKILL a few
# milliseconds later, starts it again, and compares the number of succeeded swaps with where the
# indexes are. A swap that the restart runs again leaves sa and sb swapped back: "REPRODUCED".
# The kill comes from inside the container, so its delay is not the docker CLI's. With ALL=1 it
# runs every round and counts the rounds that ran a swap twice; without, it stops at the first.
# Needs docker and curl. It removes its container and its volume at the end.
set -eu
IMG=getmeili/meilisearch:v1.57.0@sha256:91bf09897482c17205c8faeca8daa8d57999fb9de20d862763cdfa92ba4ec790
NAME=m295-meili-swap
VOL=m295-meili-swap
PORT=7711
URL=http://127.0.0.1:$PORT
ROUNDS=${1:-60}

cleanup() { docker rm -f $NAME >/dev/null 2>&1 || true; docker volume rm $VOL >/dev/null 2>&1 || true; }
trap cleanup EXIT
cleanup
docker volume create $VOL >/dev/null
docker run -d --name $NAME -p 127.0.0.1:$PORT:7700 -v $VOL:/meili_data \
  -e MEILI_ENV=development -e MEILI_NO_ANALYTICS=true $IMG >/dev/null

up() {   # wait for /health and an empty queue, at most 60 s
  i=0
  until curl -sf $URL/health >/dev/null 2>&1; do i=$((i+1)); [ $i -lt 600 ] || { echo "no health"; exit 1; }; sleep 0.1; done
  i=0
  until curl -s "$URL/tasks?statuses=enqueued,processing&limit=1" | grep -q '"total":0'; do
    i=$((i+1)); [ $i -lt 600 ] || { echo "the queue does not drain"; exit 1; }; sleep 0.1
  done
}
post() { curl -s -X POST -H 'Content-Type: application/json' "$URL$1" --data "$2"; }

up
post /indexes '{"uid":"sa","primaryKey":"id"}' >/dev/null
post /indexes '{"uid":"sb","primaryKey":"id"}' >/dev/null
post /indexes '{"uid":"docs","primaryKey":"id"}' >/dev/null
post /indexes/sa/documents '[{"id":"m","home":"sa"}]' >/dev/null
post /indexes/sb/documents '[{"id":"m","home":"sb"}]' >/dev/null
up

# the burst runs in the container: a swap, 16 additions at once, a sleep, then SIGKILL
BURST='
curl -s -X POST -H "Content-Type: application/json" http://127.0.0.1:7700/swap-indexes --data "[{\"indexes\":[\"sa\",\"sb\"]}]" >/dev/null
for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16; do
  curl -s -X POST -H "Content-Type: application/json" http://127.0.0.1:7700/indexes/docs/documents --data "[{\"id\":\"r${R}_$i\"}]" >/dev/null &
done
sleep $D
pkill -9 meilisearch
'
r=0 extra=0 hits=0
for r in $(seq 1 "$ROUNDS"); do
  # the delay steps through 0 to 45 ms
  D=$(awk -v r="$r" 'BEGIN { printf "%.3f", ((r - 1) % 16) * 0.003 }')
  docker exec -e R="$r" -e D="$D" $NAME sh -c "$BURST" >/dev/null 2>&1 || true
  docker start $NAME >/dev/null 2>&1 || true
  sleep 0.5
  docker start $NAME >/dev/null 2>&1 || true
  up
  n=$(curl -s "$URL/tasks?types=indexSwap&statuses=succeeded&limit=0" | sed -n 's/.*"total":\([0-9]*\).*/\1/p')
  home=$(curl -s "$URL/indexes/sa/documents/m" | sed -n 's/.*"home":"\([a-z]*\)".*/\1/p')
  want=sa; [ $(((n + extra) % 2)) -eq 1 ] && want=sb
  if [ "$home" != "$want" ]; then
    echo "REPRODUCED in round $r (kill ${D}s after the swap): $n succeeded swaps, and sa holds home=$home (want $want)"
    curl -s "$URL/tasks?types=indexSwap&limit=2"; echo
    [ "${ALL:-0}" = 1 ] || exit 0
    extra=$((extra + 1)) hits=$((hits + 1))
    continue
  fi
  echo "round $r (kill ${D}s after the swap): $n succeeded swaps, sa holds home=$home: as it should be"
done
if [ "$hits" -gt 0 ]; then echo "a swap ran twice in $hits of $ROUNDS rounds"; exit 0; fi
echo "not reproduced in $ROUNDS rounds"
exit 1
