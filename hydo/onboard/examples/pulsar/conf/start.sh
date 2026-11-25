#!/bin/sh
# start.sh zookeeper|bookie|broker <n>: start one Pulsar process of the cluster with its
# configuration: the image's file under conf/ with the keys of /etc/pulsar-hydo/<role>.conf and
# the node's own keys set (pconf.sh), written to /tmp. The environment chooses the durability
# settings that the example explores: JOURNAL_SYNC_DATA (bookies, default true) and ACK_QUORUM
# (brokers, default 2).
set -eu
role=$1
n=$2
h=/etc/pulsar-hydo
cd /pulsar
export PULSAR_LOG_DIR=${PULSAR_LOG_DIR:-/tmp/logs}
mkdir -p "$PULSAR_LOG_DIR"
case $role in
  zookeeper)
    mkdir -p /pulsar/data/zookeeper
    [ -f /pulsar/data/zookeeper/myid ] || echo "$n" > /pulsar/data/zookeeper/myid
    sh $h/pconf.sh conf/zookeeper.conf $h/zookeeper.conf > /tmp/zookeeper.conf
    export PULSAR_ZK_CONF=/tmp/zookeeper.conf
    ;;
  bookie)
    mkdir -p /pulsar/data/bookkeeper/journal /pulsar/data/bookkeeper/ledgers
    sh $h/pconf.sh conf/bookkeeper.conf $h/bookkeeper.conf "advertisedAddress=bookie$n" \
      "journalSyncData=${JOURNAL_SYNC_DATA:-true}" > /tmp/bookkeeper.conf
    export PULSAR_BOOKKEEPER_CONF=/tmp/bookkeeper.conf
    ;;
  broker)
    sh $h/pconf.sh conf/broker.conf $h/broker.conf "advertisedAddress=broker$n" \
      "managedLedgerDefaultAckQuorum=${ACK_QUORUM:-2}" > /tmp/broker.conf
    export PULSAR_BROKER_CONF=/tmp/broker.conf
    ;;
  *)
    echo "start.sh: unknown role $role" >&2
    exit 2
    ;;
esac
exec bin/pulsar "$role"
