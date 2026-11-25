#!/bin/sh
# init.sh: write the cluster's metadata once: the cluster c1, BookKeeper's /ledgers, the
# namespaces public/default and pulsar/system, and the transaction coordinators' topic. The
# command keeps what exists, so it runs again after a restart without harm. It tries again until
# the ZooKeeper quorum answers, for at most 300 s.
set -u
cd /pulsar
export PULSAR_LOG_DIR=${PULSAR_LOG_DIR:-/tmp/logs}
mkdir -p "$PULSAR_LOG_DIR"
zk=zk1:2181,zk2:2181,zk3:2181
end=$(( $(cut -d. -f1 /proc/uptime) + 300 ))
while [ "$(cut -d. -f1 /proc/uptime)" -lt "$end" ]; do
  if bin/pulsar initialize-cluster-metadata --cluster c1 \
      --metadata-store "zk:$zk" --configuration-metadata-store "zk:$zk" \
      --web-service-url http://broker1:8080,broker2:8080 \
      --broker-service-url pulsar://broker1:6650,broker2:6650; then
    echo "init.sh: the cluster's metadata is there"
    exit 0
  fi
  sleep 2
done
echo "init.sh: ZooKeeper did not take the cluster's metadata within 300 s" >&2
exit 1
