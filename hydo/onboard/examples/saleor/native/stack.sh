#!/bin/sh
# The native Saleor stack (plain Docker, no hydo): ../docker-compose.yaml plus the payment app.
#   sh stack.sh up              start it, create the staff user and the shop (helper_client.py setup)
#   sh stack.sh client <args>   run the test client (../hydo/workload/helper_client.py) in the API container
#   sh stack.sh sql "<query>"   one query on Saleor's database
#   sh stack.sh logs <service>  a service's log
#   sh stack.sh down            remove the containers and the volumes
# SALEOR_PROJECT names the Compose project (default hydosaleor); PAYAPP_DELAY delays the payment
# app's answers (seconds).
set -eu
cd "$(dirname "$0")"
P=${SALEOR_PROJECT:-hydosaleor}
dc() { docker compose -p "$P" -f ../docker-compose.yaml -f compose.payapp.yaml "$@"; }
case "${1:-}" in
  up)
    dc up -d --wait --wait-timeout 900
    dc exec -T api python3 manage.py createsuperuser --noinput
    dc exec -T -e SALEOR_URL=http://localhost:8000/graphql/ api \
      python3 /hydo/workload/helper_client.py setup http://payapp:9001
    ;;
  client)
    shift
    dc exec -T -e SALEOR_URL=http://localhost:8000/graphql/ api python3 /hydo/workload/helper_client.py "$@"
    ;;
  sql) dc exec -T db psql -U saleor -d saleor -Atc "$2" ;;
  logs) shift; dc logs --no-log-prefix "$@" ;;
  down) dc down -v --remove-orphans ;;
  *) echo "usage: sh stack.sh up | client ARGS | sql QUERY | logs SERVICE | down" >&2; exit 2 ;;
esac
