#!/bin/sh
# vlog-tail-cut.sh: finding 1 of ../RESULTS.md without hydo, with Docker and SurrealDB's own
# image. SurrealDB runs with its defaults: SurrealKV, sync=every. The script
#   1. creates 30 records with a 9,000-byte value each (SurrealKV keeps values over 4 KiB in its
#      value log), each followed by a small record, and then 10 more small records;
#   2. kills the server (SIGKILL) and zeroes 512 bytes inside the newest value-log entry;
#   3. starts the server again and counts the records.
# Every creation was acknowledged, and each was synced before its answer. With one damaged value,
# the open drops that record and the 11 small records after it, and logs a warning only.
# With LATER_BIG=1 the script creates one more large record after the small ones and damages the
# same value: then the open refuses to start, and names the sync mark that claims the record.
# Needs Docker. It removes its container and volume at the end.
set -eu
IMG=surrealdb/surrealdb:v4.0.1
NAME=hydo-surreal-vlog
VOL=hydo-surreal-vlog
PORT=${PORT:-18765}
docker rm -f $NAME >/dev/null 2>&1 || true
docker volume rm $VOL >/dev/null 2>&1 || true
docker volume create $VOL >/dev/null
start() {
  docker run -d --name $NAME -u root -p 127.0.0.1:$PORT:8000 -v $VOL:/data $IMG \
    start --user root --pass root surrealkv:///data/db > /dev/null
  i=0; until curl -fs "http://127.0.0.1:$PORT/health" > /dev/null; do
    i=$((i+1)); [ $i -lt 60 ] || { echo "the server did not start"; docker logs $NAME | tail -5; exit 1; }
    sleep 0.5
  done
}
q() {
  curl -s -u root:root -H 'Accept: application/json' -H 'surreal-ns: t' -H 'surreal-db: t' \
    -X POST "http://127.0.0.1:$PORT/sql" --data "$1"
}
counts() { q 'SELECT VALUE count() FROM ONLY big GROUP ALL; SELECT VALUE count() FROM ONLY small GROUP ALL;' |
  sed 's/.*"result":\([0-9]*\).*"result":\([0-9]*\).*/big \1, small \2/'; }

start
q 'DEFINE NAMESPACE IF NOT EXISTS t; USE NS t; DEFINE DATABASE IF NOT EXISTS t;' > /dev/null
pad=$(head -c 9000 /dev/zero | tr '\0' x)
i=1
while [ $i -le 30 ]; do
  q "CREATE big:$i SET pad = '$pad'; CREATE small:$i SET k = $i;" | grep -q ERR && { echo "a write failed"; exit 1; }
  i=$((i+1))
done
i=101
while [ $i -le 110 ]; do
  q "CREATE small:$i SET k = $i;" | grep -q ERR && { echo "a write failed"; exit 1; }
  i=$((i+1))
done
big=30
if [ "${LATER_BIG:-0}" = 1 ]; then
  q "CREATE big:31 SET pad = '$pad';" | grep -q ERR && { echo "a write failed"; exit 1; }
  big=31
fi
echo "acknowledged: big $big, small 40"
echo "read back:    $(counts)"

docker kill $NAME > /dev/null
docker rm $NAME > /dev/null
# zero 512 bytes inside the value of big:30: 4,000 bytes before the end of the value log, or
# 13,000 bytes before it when big:31 follows
back=4000; [ "$big" = 31 ] && back=13000
docker run --rm -u root -e back=$back -v $VOL:/data alpine:3.20 sh -c '
  f=/data/db/vlog/00000000000000000001.vlog
  size=$(wc -c < $f); off=$((size - back))
  dd if=/dev/zero of=$f bs=1 seek=$off count=512 conv=notrunc 2>/dev/null
  echo "zeroed 512 bytes at $off of $f ($size bytes)"'

docker run -d --name $NAME -u root -p 127.0.0.1:$PORT:8000 -v $VOL:/data $IMG \
  start --user root --pass root surrealkv:///data/db > /dev/null
i=0; until curl -fs "http://127.0.0.1:$PORT/health" > /dev/null; do
  i=$((i+1))
  if [ $i -ge 20 ] || [ "$(docker inspect -f '{{.State.Running}}' $NAME)" != true ]; then
    echo "the server did not start:"; docker logs $NAME 2>&1 | grep -E 'ERROR' | sed 's/^/  /'
    docker rm -f $NAME > /dev/null; docker volume rm $VOL > /dev/null; exit 0
  fi
  sleep 0.5
done
echo "after the restart: $(counts)"
echo "missing small records: $(q 'SELECT VALUE k FROM small ORDER BY k;' | tr -d ' ' |
  sed 's/.*"result":\[\([^]]*\)\].*/\1/' | tr ',' '\n' |
  awk 'BEGIN{for(i=1;i<=30;i++)w[i]=1;for(i=101;i<=110;i++)w[i]=1}{delete w[$1]}END{for(k in w)printf "%s ",k;print ""}')"
echo "the server's log:"
docker logs $NAME 2>&1 | grep -E 'WARN surrealkv|ERROR' | sed 's/^/  /'
docker rm -f $NAME > /dev/null
docker volume rm $VOL > /dev/null
