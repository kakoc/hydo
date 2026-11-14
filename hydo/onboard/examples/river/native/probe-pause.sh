#!/bin/sh
# The leader is stopped for a few seconds (SIGSTOP, as a long GC pause or a stalled VM would) while
# PostgreSQL restarts, under the running stack (sh stack.sh up).
#   MODE=crash sh probe-pause.sh       SIGKILL PostgreSQL and start it again (crash recovery)
#   MODE=graceful sh probe-pause.sh    stop PostgreSQL with SIGTERM and start it again
#   MODE=none sh probe-pause.sh        leave PostgreSQL alone: only the pause
#   PAUSE=<s> (default 6): how long the leader stays stopped; River's lease is 15 s
# It prints the leader before, every leader row seen afterwards, and each run of the periodic job
# (its scheduled time) that two clients inserted.
set -eu
cd "$(dirname "$0")"
PG=hydo-native-river-pg
MODE=${MODE:-crash}
PAUSE=${PAUSE:-6}
q() { docker exec "$PG" psql -U postgres -d river -At -c "$1" 2>/dev/null; }
i=0
until [ -n "$(q 'SELECT leader_id FROM river_leader' || true)" ]; do
  i=$((i + 1)); [ $i -lt 60 ] || { echo "probe-pause.sh: no leader" >&2; exit 1; }; sleep 1
done
before=$(q 'SELECT leader_id FROM river_leader')
pid=${before##*-}
id0=$(q "SELECT coalesce(max(id), 0) FROM river_job")
kill -STOP "$pid"
case "$MODE" in
crash) docker kill -s KILL "$PG" >/dev/null; sleep 1; docker start "$PG" >/dev/null ;;
graceful) docker stop -t 30 "$PG" >/dev/null; sleep 1; docker start "$PG" >/dev/null ;;
none) ;;
*) kill -CONT "$pid"; echo "MODE is crash, graceful or none" >&2; exit 2 ;;
esac
seen=""
last=""
t0=$(date +%s)
cont=no
n=0
while [ $n -lt 50 ]; do
  if [ $cont = no ] && [ $(( $(date +%s) - t0 )) -ge "$PAUSE" ]; then
    kill -CONT "$pid"; cont=yes; seen="$seen $(date +%H:%M:%S):(leader resumed)"
  fi
  l=$(q 'SELECT leader_id FROM river_leader' || echo "?")
  [ -n "$l" ] || l="(none)"
  if [ "$l" != "$last" ]; then
    seen="$seen $(date +%H:%M:%S):$l"
    last=$l
  fi
  n=$((n + 1))
  sleep 0.5
done
[ $cont = yes ] || kill -CONT "$pid"
dups=$(q "SELECT to_char(scheduled_at, 'HH24:MI:SS') || ':' || string_agg(args->>'leader', '+' ORDER BY id)
            FROM river_job WHERE kind = 'tick' AND id > $id0
            GROUP BY scheduled_at HAVING count(DISTINCT args->>'leader') > 1 ORDER BY 1" | tr '\n' ' ')
ndups=$(q "SELECT count(*) FROM (SELECT 1 FROM river_job WHERE kind = 'tick' AND id > $id0
            GROUP BY scheduled_at HAVING count(DISTINCT args->>'leader') > 1) x")
echo "$MODE: leader before $before, stopped for $PAUSE s; leader rows after:$seen"
echo "$MODE: $ndups runs of the periodic job inserted by two clients ${dups}"
