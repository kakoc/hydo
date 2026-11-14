#!/bin/sh
# One restart of PostgreSQL under the running stack (sh stack.sh up), and what River's leader
# election did around it.
#   MODE=crash sh probe-restart.sh       SIGKILL PostgreSQL, start it again (crash recovery)
#   MODE=graceful sh probe-restart.sh    stop PostgreSQL with SIGTERM (a clean shutdown), start it
#   DOWN=<s> (default 3): how long PostgreSQL stays down between the stop and the start
# It prints the leader before the restart, every leader row seen in the 25 s after it, and the
# periods in which the periodic job was inserted by two different clients. River documents that
# leader election ensures that only one client manages periodic jobs at a time.
set -eu
cd "$(dirname "$0")"
PG=hydo-native-river-pg
MODE=${MODE:-crash}
DOWN=${DOWN:-3}   # seconds PostgreSQL stays down
q() { docker exec "$PG" psql -U postgres -d river -At -c "$1" 2>/dev/null; }
i=0
until [ -n "$(q 'SELECT leader_id FROM river_leader' || true)" ]; do
  i=$((i + 1)); [ $i -lt 60 ] || { echo "probe-restart.sh: no leader" >&2; exit 1; }; sleep 1
done
before=$(q 'SELECT leader_id FROM river_leader')
id0=$(q "SELECT coalesce(max(id), 0) FROM river_job")
case "$MODE" in
crash) docker kill -s KILL "$PG" >/dev/null; sleep "$DOWN"; docker start "$PG" >/dev/null ;;
graceful) docker stop -t 30 "$PG" >/dev/null; sleep "$DOWN"; docker start "$PG" >/dev/null ;;
*) echo "MODE is crash or graceful" >&2; exit 2 ;;
esac
seen=""
last=""
n=0
while [ $n -lt 50 ]; do
  l=$(q 'SELECT leader_id FROM river_leader' || echo "?")
  [ -n "$l" ] || l="(none)"
  if [ "$l" != "$last" ]; then
    seen="$seen $(date +%H:%M:%S):$l"
    last=$l
  fi
  n=$((n + 1))
  sleep 0.5
done
dups=$(q "SELECT (args->>'period') || ':' || string_agg(args->>'leader', '+' ORDER BY id)
            FROM river_job WHERE kind = 'tick' AND id > $id0
            GROUP BY args->>'period' HAVING count(DISTINCT args->>'leader') > 1 ORDER BY 1" | tr '\n' ' ')
ndups=$(q "SELECT count(*) FROM (SELECT 1 FROM river_job WHERE kind = 'tick' AND id > $id0
            GROUP BY args->>'period' HAVING count(DISTINCT args->>'leader') > 1) x")
echo "$MODE: leader before $before; leader rows after:$seen"
echo "$MODE: $ndups periods with the periodic job inserted by two clients ${dups}"
