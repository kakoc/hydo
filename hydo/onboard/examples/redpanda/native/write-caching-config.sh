#!/bin/sh
# Show that conf/bootstrap-write-caching.yaml turns write caching on: start the cluster of
# ../docker-compose.yaml with it and print the cluster's properties that the file sets.
set -eu
cd "$(dirname "$0")/.."
P=rpwc
dc() { RP_BOOTSTRAP=bootstrap-write-caching.yaml docker compose -p $P "$@"; }
dc down -v >/dev/null 2>&1 || true
dc up -d n1 n2 n3 >/dev/null
i=0
until dc exec -T n1 curl -s -f http://localhost:9644/v1/cluster_config > /tmp/rpwc.json 2>/dev/null; do
  i=$((i + 1)); [ $i -lt 60 ] || { echo "no admin API after 60 s"; exit 1; }; sleep 1
done
tr ',' '\n' < /tmp/rpwc.json | grep -E '"(write_caching_default|transaction_coordinator_partitions|segment_fallocation_step)"'
dc down -v >/dev/null 2>&1
