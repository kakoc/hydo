#!/bin/sh
# start.sh [<master host>]: start one Kvrocks node.
#
# At the node's first start, copy the template /etc/kvc/kvrocks.conf into the data directory as node.conf,
# with rocksdb.write_options.sync from KVROCKS_SYNC and use-rsid-psync from KVROCKS_RSID, and
# with `slaveof <master host> 6666` for a replica. Later starts keep the copy: Kvrocks rewrites
# it at every start and at CONFIG REWRITE, which the failover of the test client uses to make a
# node's role last across restarts, as Redis Sentinel does.
set -eu
d=/var/lib/kvrocks
conf=$d/node.conf
if [ ! -e "$conf" ]; then
  sed -e "s/^rocksdb.write_options.sync .*/rocksdb.write_options.sync ${KVROCKS_SYNC:-yes}/" \
      -e "s/^use-rsid-psync .*/use-rsid-psync ${KVROCKS_RSID:-yes}/" \
      /etc/kvc/kvrocks.conf > "$conf.new"
  if [ -n "${1:-}" ]; then
    echo "slaveof $1 6666" >> "$conf.new"
  fi
  sync "$conf.new"
  mv "$conf.new" "$conf"
  sync "$d"
fi
mkdir -p /var/run/kvrocks
exec kvrocks -c "$conf" --pidfile /var/run/kvrocks/kvrocks.pid
