#!/bin/sh
# Reproduce without hydo: a namespace setting made through sqld's admin API is lost when sqld
# restarts.   SQLD_TAG=v0.24.33 sh run.sh      (needs Docker and curl)
# Sets durability_mode=strong (and, in a second part, block_writes=true) on the default namespace,
# reads it back, restarts sqld three ways (SIGKILL, a clean stop, `docker compose restart`) and
# reads it back again. `PRAGMA synchronous` shows what the connections use (1 = NORMAL, 2 = FULL).
# `docker compose down -v` removes everything afterwards.
set -eu
cd "$(dirname "$0")"
ADMIN=http://127.0.0.1:19090/v1/namespaces/default/config
SQL=http://127.0.0.1:18080/v2/pipeline

wait_up() {
  i=0
  until curl -sf http://127.0.0.1:18080/health >/dev/null && curl -sf "$ADMIN" >/dev/null; do
    i=$((i + 1)); [ $i -le 60 ] || { echo "sqld did not come up in 60 s"; docker compose logs --tail 30; exit 1; }
    sleep 1
  done
}
show() {
  printf '%-34s config: %s\n' "$1" "$(curl -s "$ADMIN")"
  printf '%-34s PRAGMA synchronous: %s\n' "" "$(curl -s "$SQL" -H 'content-type: application/json' \
    -d '{"requests":[{"type":"execute","stmt":{"sql":"PRAGMA synchronous"}},{"type":"close"}]}' \
    | sed -n 's/.*"rows":\[\[{"type":"integer","value":"\([0-9]*\)"}\]\].*/\1/p')"
}
write() {
  printf '%-34s INSERT: %s\n' "" "$(curl -s "$SQL" -H 'content-type: application/json' \
    -d '{"requests":[{"type":"execute","stmt":{"sql":"CREATE TABLE IF NOT EXISTS t(x)"}},{"type":"execute","stmt":{"sql":"INSERT INTO t VALUES (1)"}},{"type":"close"}]}' \
    | grep -o '"error":{[^}]*}' | head -1 | grep . || echo ok)"
}
set_config() {
  curl -sf -X POST "$ADMIN" -H 'content-type: application/json' -d "$1" >/dev/null
}

docker compose pull -q
docker compose up -d
wait_up
docker compose exec -T sqld /bin/sqld --version 2>/dev/null || true
show "fresh start"

echo "== 1. durability_mode=strong, then restarts"
set_config '{"block_reads":false,"block_writes":false,"durability_mode":"strong"}'
show "after POST strong"
sleep 5
show "5 s later"
docker compose kill -s KILL sqld >/dev/null 2>&1
docker compose start sqld >/dev/null 2>&1
wait_up
show "after SIGKILL and start"
set_config '{"block_reads":false,"block_writes":false,"durability_mode":"strong"}'
show "after POST strong again"
docker compose stop sqld >/dev/null 2>&1
docker compose start sqld >/dev/null 2>&1
wait_up
show "after clean stop and start"
set_config '{"block_reads":false,"block_writes":false,"durability_mode":"strong"}'
docker compose restart sqld >/dev/null 2>&1
wait_up
show "after docker compose restart"

echo "== 2. block_writes=true, then a clean restart"
set_config '{"block_reads":false,"block_writes":true,"block_reason":"maintenance","durability_mode":"strong"}'
show "after POST block_writes"
write
docker compose stop sqld >/dev/null 2>&1
docker compose start sqld >/dev/null 2>&1
wait_up
show "after clean stop and start"
write
