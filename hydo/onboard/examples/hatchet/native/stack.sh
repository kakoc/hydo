#!/bin/sh
# The example's stack without hydo: PostgreSQL, Hatchet v0.120.5's migration, setup, two engines and
# API, and three workers of hq, with Docker Compose (../docker-compose.yaml).
#
#   sh stack.sh up      build hq (Go) and start everything; waits until the workers run
#   sh stack.sh down    stop it and remove its containers, volumes and the workers' image
set -eu
cd "$(dirname "$0")/.."
P=${HQ_PROJECT:-hqnative}
case "${1:-}" in
  up)
    sh build.sh > /dev/null
    docker compose -p "$P" up -d --build
    i=0
    until [ "$(docker compose -p "$P" logs worker-1 worker-2 worker-3 2>/dev/null | grep -c 'started$')" -ge 3 ]; do
      i=$((i + 1)); [ $i -lt 120 ] || { echo "stack.sh: the workers did not start in 120 s" >&2; exit 1; }
      sleep 1
    done
    echo "stack.sh: up" ;;
  down)
    docker compose -p "$P" down -v --rmi local ;;
  *) echo "usage: sh stack.sh up|down" >&2; exit 2 ;;
esac
