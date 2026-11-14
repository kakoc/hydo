#!/bin/sh
# Finding 1, lago-api issue 6901: a subscription cancelled while Redis is down is terminated and
# never invoiced. Starts Lago v1.57.0 from compose.yaml as the Docker Compose project
# hydo-native-lago-6901, runs the five requests, and removes the project with its volumes.
#   sh repro-6901.sh             # KEEP=1 leaves the project running
# Needs Docker with Compose v2, curl and python3. The API listens on 127.0.0.1:13000.
set -u
cd "$(dirname "$0")"
P="docker compose --progress quiet -p hydo-native-lago-6901 -f compose.yaml"
L="python3 -I lago.py"
cleanup() { [ "${KEEP:-0}" = 1 ] || $P down -v --remove-orphans >/dev/null 2>&1; }
trap cleanup EXIT INT TERM

$P up -d >/dev/null || exit 1
i=0
until curl -sf http://127.0.0.1:13000/health >/dev/null 2>&1; do
  i=$((i + 1)); [ $i -gt 150 ] && { echo "repro-6901.sh: the API did not start" >&2; exit 1; }; sleep 2
done
echo "1. seed: two customers with a calendar subscription each and one event of 12 units"
$L seed t6901 calendar - cust1 cust2 || exit 1
sleep 5
echo "2. control: cancel sub-cust2 with Redis up"
$L terminate sub-cust2
sleep 20
$L invoices cust2
echo "3. stop Redis and cancel sub-cust1"
$P stop redis >/dev/null
$L terminate sub-cust1
$P start redis >/dev/null
i=0
until [ "$($P ps --format '{{.Health}}' redis 2>/dev/null)" = healthy ]; do
  i=$((i + 1)); [ $i -gt 60 ] && break; sleep 1
done
sleep 5
echo "4. retry the cancellation"
$L terminate sub-cust1
echo "5. the subscription and its invoices, now and 60 s later"
$L status sub-cust1
$L invoices cust1
sleep 60
$L invoices cust1
