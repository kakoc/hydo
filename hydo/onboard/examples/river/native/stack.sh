#!/bin/sh
# The native stack, without hydo: PostgreSQL 16.16 in Docker and three riverq workers on this
# machine, each a River v0.51.2 client with the periodic job (every 2 s) and leader election.
#   sh stack.sh up      start PostgreSQL, run the migrations, start the workers w1, w2, w3
#   sh stack.sh down    stop the workers, remove the container and its volume
# The programs log to work/; the container is hydo-native-river-pg on 127.0.0.1:55433.
set -eu
cd "$(dirname "$0")"
PG=hydo-native-river-pg
PORT=55433
export RIVERQ_DATABASE_URL="postgres://postgres:river@127.0.0.1:$PORT/river?sslmode=disable&connect_timeout=5"
export HYDO_HIST_OUT=off
mkdir -p work
case "${1:-}" in
up)
  [ -x work/riverq ] || (cd ../app && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 go build -trimpath -o ../native/work/riverq .)
  docker run -d --name "$PG" -p "127.0.0.1:$PORT:5432" -e POSTGRES_PASSWORD=river -e POSTGRES_DB=river \
    -v hydo-native-river-pgdata:/var/lib/postgresql/data postgres:16.16-alpine3.24 >/dev/null
  i=0
  until docker exec "$PG" pg_isready -q -U postgres -h 127.0.0.1 2>/dev/null; do
    i=$((i + 1)); [ $i -lt 60 ] || { echo "stack.sh: PostgreSQL did not start" >&2; exit 1; }; sleep 1
  done
  sleep 1
  ./work/riverq migrate
  for w in w1 w2 w3; do
    nohup ./work/riverq worker "$w" > "work/$w.log" 2>&1 &
    echo $! > "work/$w.pid"
    sleep 1
  done
  echo "stack.sh: up (workers $(cat work/w1.pid) $(cat work/w2.pid) $(cat work/w3.pid))"
  ;;
down)
  for w in w1 w2 w3; do
    [ -f "work/$w.pid" ] && kill "$(cat "work/$w.pid")" 2>/dev/null || true
    rm -f "work/$w.pid"
  done
  docker rm -f "$PG" >/dev/null 2>&1 || true
  docker volume rm hydo-native-river-pgdata >/dev/null 2>&1 || true
  echo "stack.sh: down"
  ;;
*)
  echo "usage: sh stack.sh up|down" >&2; exit 2 ;;
esac
