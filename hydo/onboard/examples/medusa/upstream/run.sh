#!/bin/sh
# Reproduce the refund race without hydo:  MEDUSA_VERSION=2.18.0 sh run.sh [trials]
# Builds and starts the stack, seeds a store and an admin user, runs race.mjs, then lists from
# PostgreSQL the order versions that hold two order changes and the order items written twice at
# one version. `docker compose down -v` removes everything afterwards.
set -eu
cd "$(dirname "$0")"
TRIALS=${1:-20}
export MEDUSA_VERSION=${MEDUSA_VERSION:-2.18.0}
docker compose up -d --build
i=0
until [ "$(docker inspect -f '{{.State.Health.Status}}' "$(docker compose ps -q medusa)")" = healthy ]; do
  i=$((i + 1)); [ $i -le 150 ] || { echo "medusa did not become healthy in 300 s"; docker compose logs --tail 50 medusa; exit 1; }
  sleep 2
done
docker compose exec -T medusa ./node_modules/.bin/medusa exec ./src/scripts/repro-seed.js
docker compose exec -T medusa ./node_modules/.bin/medusa user -e admin@repro.test -p repro-secret || true
echo "== Medusa $MEDUSA_VERSION: $TRIALS trials of two concurrent refunds"
docker compose exec -T medusa node /repro/race.mjs "$TRIALS"
echo "== order versions with more than one order change"
docker compose exec -T postgres psql -U postgres -d medusa-store -c \
  "select order_id, version, count(*) as changes, string_agg(change_type, ',') as types from order_change group by order_id, version having count(*) > 1 order by order_id;"
echo "== order items written twice at one version"
docker compose exec -T postgres psql -U postgres -d medusa-store -c \
  "select order_id, item_id, version, count(*) as rows from order_item where deleted_at is null group by order_id, item_id, version having count(*) > 1 order by order_id;"
