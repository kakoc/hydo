#!/bin/sh
# Start or stop a fresh Blnk stack on this machine:
#   sh stack.sh up [blnk-binary]   # new PostgreSQL and Redis containers, then `blnk migrate up`,
#                                  # `blnk start` and `blnk workers` (default binary: work/blnk)
#   sh stack.sh down               # stop Blnk and remove both containers
# PostgreSQL 16.16 and Redis 7.2.4, the versions of Blnk's docker-compose.yaml, pinned by tag.
# Ports on 127.0.0.1: PostgreSQL 55432, Redis 56379, Blnk's API 55001. Blnk's files are in work/.
set -u
cd "$(dirname "$0")"
HERE=$(pwd)
PG=hydo-native-blnk-pg
REDIS=hydo-native-blnk-redis
W=$HERE/work
mkdir -p "$W"

down() {
  pids=
  for f in "$W/server.pid" "$W/workers.pid"; do
    [ -f "$f" ] && { pids="$pids $(cat "$f")"; kill "$(cat "$f")" 2>/dev/null; }
    rm -f "$f"
  done
  # Blnk stops its workers gracefully; wait up to 30 s, then stop them hard.
  for p in $pids; do
    i=0
    while kill -0 "$p" 2>/dev/null && [ $i -lt 30 ]; do sleep 1; i=$((i + 1)); done
    kill -9 "$p" 2>/dev/null
  done
  docker rm -f "$PG" "$REDIS" >/dev/null 2>&1
  return 0
}

case ${1:-} in
  down) down; echo "stopped"; exit 0 ;;
  up) ;;
  *) echo "usage: sh stack.sh up [blnk-binary] | down" >&2; exit 1 ;;
esac

BIN=${2:-$W/blnk}
case $BIN in /*) ;; *) BIN=$HERE/$BIN ;; esac
[ -x "$BIN" ] || { echo "stack.sh: no Blnk program at $BIN (run sh build-blnk.sh)" >&2; exit 1; }
down
docker run -d --name "$PG" -e POSTGRES_PASSWORD=password -e POSTGRES_DB=blnk \
  -p 127.0.0.1:55432:5432 postgres:16.16-alpine3.24 -c max_connections=200 >/dev/null || exit 1
docker run -d --name "$REDIS" -p 127.0.0.1:56379:6379 redis:7.2.4 >/dev/null || exit 1
i=0
until docker exec "$PG" pg_isready -q -U postgres -h 127.0.0.1; do
  i=$((i + 1)); [ $i -gt 60 ] && { echo "stack.sh: PostgreSQL did not start" >&2; exit 1; }; sleep 1
done
sleep 3
# Blnk reads blnk.json from its working directory.
cd "$W"
cat > blnk.json <<'EOF'
{
  "project_name": "Blnk",
  "data_source": { "dns": "postgres://postgres:password@127.0.0.1:55432/blnk?sslmode=disable" },
  "redis": { "dns": "127.0.0.1:56379" },
  "server": { "port": "55001" },
  "queue": { "monitoring_port": "55004" }
}
EOF
"$BIN" migrate up > migrate.log 2>&1 || { tail -5 migrate.log; exit 1; }
nohup "$BIN" start > server.log 2>&1 &
echo $! > server.pid
nohup "$BIN" workers > workers.log 2>&1 &
echo $! > workers.pid
i=0
while [ $i -lt 30 ]; do
  curl -sf http://127.0.0.1:55001/ledgers >/dev/null 2>&1 && { echo "up"; exit 0; }
  sleep 1; i=$((i + 1))
done
echo "stack.sh: Blnk did not start"; tail -20 server.log; exit 1
