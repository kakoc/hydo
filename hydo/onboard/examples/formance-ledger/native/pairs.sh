#!/bin/sh
# Send pairs of deposits to new accounts at once, and count the pairs that fail one request.
#   sh pairs.sh <ledger-binary> <label> [mode] [pairs]
# mode:
#   two-assets  (default) one deposit in USD/2 and one in EUR/2 to the same new account
#   one-asset   both deposits in USD/2
#   meta        USD/2 with the account metadata a=1, and EUR/2 dated one hour back with b=2
# pairs: 200 by default; each pair goes to its own new account shop:<n>.
# A fresh PostgreSQL 16.16 container (the image the ledger's own docker-compose.yml names, pinned)
# and the ledger on this machine, with a new database every time. Both are removed at the end.
# Writes work/runs/<label>/: the answer codes of every pair, the ledger's log, and a summary.
# Ports: PG_PORT (55432) and LEDGER_PORT (3068) on 127.0.0.1.
set -u
cd "$(dirname "$0")"
BIN=${1:?usage: sh pairs.sh <ledger-binary> <label> [mode] [pairs]}
LABEL=${2:?usage: sh pairs.sh <ledger-binary> <label> [mode] [pairs]}
MODE=${3:-two-assets}
N=${4:-200}
case $MODE in two-assets|one-asset|meta) ;; *) echo "pairs.sh: unknown mode $MODE" >&2; exit 1 ;; esac
case $BIN in /*) ;; *) BIN=$(pwd)/$BIN ;; esac
PG_PORT=${PG_PORT:-55432}
LEDGER_PORT=${LEDGER_PORT:-3068}
PG=hydo-native-formance-pg
IMG=postgres:16.16-alpine3.24
OUT=work/runs/$LABEL
rm -rf "$OUT"; mkdir -p "$OUT"
LP=
cleanup() {
  [ -n "$LP" ] && kill "$LP" 2>/dev/null && wait "$LP" 2>/dev/null
  docker rm -f "$PG" >/dev/null 2>&1
}
trap cleanup EXIT INT TERM

docker rm -f "$PG" >/dev/null 2>&1
docker run -d --name "$PG" -e POSTGRES_USER=ledger -e POSTGRES_PASSWORD=ledger -e POSTGRES_DB=ledger \
  -p 127.0.0.1:"$PG_PORT":5432 "$IMG" >/dev/null || exit 1
i=0
until docker exec "$PG" pg_isready -U ledger -h 127.0.0.1 >/dev/null 2>&1; do
  i=$((i + 1)); [ $i -gt 60 ] && { echo "pairs.sh: PostgreSQL did not start" >&2; exit 1; }; sleep 1
done
sleep 2
"$BIN" serve --postgres-uri "postgresql://ledger:ledger@127.0.0.1:$PG_PORT/ledger?sslmode=disable" \
  --auto-upgrade --bind 127.0.0.1:"$LEDGER_PORT" >"$OUT/ledger.log" 2>&1 &
LP=$!
B=http://127.0.0.1:$LEDGER_PORT
i=0
while :; do
  code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$B/v2/main" -d '{}')
  [ "$code" = 204 ] || [ "$code" = 200 ] && break
  i=$((i + 1)); [ $i -gt 60 ] && { echo "pairs.sh: the ledger did not start (log: $OUT/ledger.log)" >&2; exit 1; }
  sleep 1
done
echo "the ledger main: $code"

# The timestamp one hour back, for the meta mode (BSD date on macOS, GNU date on Linux).
BACK=$(date -u -v-1H +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '1 hour ago' +%Y-%m-%dT%H:%M:%SZ)
L=$B/v2/main/transactions
one=0; two=0; other=0
i=1
while [ "$i" -le "$N" ]; do
  a="shop:$i"
  case $MODE in
    two-assets)
      ba='{"postings":[{"source":"world","destination":"'$a'","asset":"USD/2","amount":100}]}'
      bb='{"postings":[{"source":"world","destination":"'$a'","asset":"EUR/2","amount":100}]}' ;;
    one-asset)
      ba='{"postings":[{"source":"world","destination":"'$a'","asset":"USD/2","amount":100}]}'
      bb=$ba ;;
    meta)
      ba='{"postings":[{"source":"world","destination":"'$a'","asset":"USD/2","amount":100}],"accountMetadata":{"'$a'":{"a":"1"}}}'
      bb='{"postings":[{"source":"world","destination":"'$a'","asset":"EUR/2","amount":100}],"timestamp":"'$BACK'","accountMetadata":{"'$a'":{"b":"2"}}}' ;;
  esac
  curl -s -o /dev/null -w '%{http_code}\n' -X POST "$L" -d "$ba" >"$OUT/a.$i" &
  pa=$!
  curl -s -o /dev/null -w '%{http_code}\n' -X POST "$L" -d "$bb" >"$OUT/b.$i" &
  pb=$!
  wait "$pa" "$pb"
  r="$(cat "$OUT/a.$i") $(cat "$OUT/b.$i")"
  echo "$i $r" >>"$OUT/codes"
  case "$r" in
    "200 200") two=$((two + 1)) ;;
    "200 500"|"500 200") one=$((one + 1)) ;;
    *) other=$((other + 1)) ;;
  esac
  i=$((i + 1))
done
rm -f "$OUT"/a.* "$OUT"/b.*

# What the ledger holds afterwards, read from PostgreSQL (the ledger keeps `main` in the bucket
# `_default`).
q() { docker exec "$PG" psql -U ledger -At -c "$1" | tr '\n' ' ' | sed 's/ $//'; }
ACC=$(q "select count(*) from _default.accounts where address like 'shop:%'")
VOL=$(q "select asset, count(*), sum(input) from _default.accounts_volumes where accounts_address like 'shop:%' group by asset order by asset")
EXTRA=
if [ "$MODE" = meta ]; then
  AB=$(q "select count(*) from _default.accounts where address like 'shop:%' and metadata @> '{\"a\":\"1\",\"b\":\"2\"}'")
  EARLY=$(q "select count(*) from _default.accounts where address like 'shop:%' and first_usage < now() - interval '30 minutes'")
  EXTRA=" metadata_a_and_b=$AB first_usage_backdated=$EARLY"
fi
DUP=$(grep -c 'duplicate key' "$OUT/ledger.log")
echo "$LABEL: mode=$MODE pairs=$N one500=$one two200=$two other=$other shop_accounts=$ACC$EXTRA volumes=[$VOL] duplicate_key_log_lines=$DUP" | tee "$OUT/summary"
