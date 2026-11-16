#!/bin/sh
# Without hydo: which snapshot Dragonfly 2.1.0 loads when an older snapshot has the later file
# time, as it has after a backward step of the wall clock. A clock step of the host is not done
# here; `touch` gives the first snapshot's files a time 1 hour ahead, which is what the files of
# a snapshot written before a 1-hour step back look like to every later start.
# Needs Docker. Leaves nothing behind (container and volume dfnative).
set -eu
I=ghcr.io/dragonflydb/dragonfly:v2.1.0@sha256:94891bb22ce2fb0f7a5a3ff1bb3b2fbbc5830e2523293e47723d87fa3e5b1184
docker rm -f dfnative >/dev/null 2>&1 || true
docker volume rm dfnative >/dev/null 2>&1 || true
docker volume create dfnative >/dev/null
up() {
  docker run -d --name dfnative -v dfnative:/data "$I" dragonfly --logtostderr \
    --proactor_threads=2 --dir=/data --dbfilename='dump-{timestamp}' --version_check=false >/dev/null
  for _ in $(seq 1 50); do
    [ "$(docker exec dfnative redis-cli GET probe 2>/dev/null)" != "" ] || break
    sleep 0.2
  done
  sleep 1
}
cli() { docker exec dfnative redis-cli "$@"; }
up
cli SET k first >/dev/null; cli SAVE
first=$(docker exec dfnative sh -c 'ls /data | grep summary')
sleep 2
cli SET k second >/dev/null; cli SAVE
echo "files:"; docker exec dfnative ls -l --time-style=+%T /data
echo "before the restart: k=$(cli GET k)"
# the first snapshot's files get a time 1 hour ahead
docker exec dfnative sh -c 'for f in /data/*; do case "$f" in *'"${first%-summary.dfs}"'*) touch -d "@$(( $(date +%s) + 3600 ))" "$f";; esac; done'
docker exec dfnative ls -l --time-style=+%T /data
docker stop dfnative >/dev/null   # SIGTERM: a clean shutdown, which saves a third snapshot
docker start dfnative >/dev/null; sleep 3
docker logs dfnative 2>&1 | grep -E 'Load snapshot|Loading' | tail -3
echo "files after the restart:"; docker exec dfnative ls -l --time-style=+%T /data | grep summary
echo "after the restart: k=$(cli GET k)"
docker rm -f dfnative >/dev/null; docker volume rm dfnative >/dev/null
