#!/bin/sh
# Reproduce without hydo: one node's zeroed partition snapshot stops the node at every start.
# Starts the three-node cluster of ../docker-compose.yaml, creates the topic t, stops n1, zeroes
# the first 53 bytes of n1's kafka/t/1_*/tx.snapshot (the file G= changed in hydo), and starts
# n1 again. Prints n1's state and the log line of its stop. Needs Docker and ../bin/rpclient-<arch>.
set -eu
cd "$(dirname "$0")/.."
P=rpnative
arch=$(uname -m); case $arch in arm64|aarch64) arch=arm64 ;; *) arch=amd64 ;; esac
dc() { docker compose -p $P "$@"; }
dc down -v >/dev/null 2>&1 || true
dc up -d n1 n2 n3 client >/dev/null
dc exec -T -e HYDO_HIST_OUT=off client /rp/rpclient-$arch create
dc exec -T -e HYDO_HIST_OUT=off client /rp/rpclient-$arch txn 1 1 || true
dc stop n1 >/dev/null
vol=$(docker volume ls -q | grep "^${P}_n1-data$")
docker run --rm -v "$vol:/d" busybox:1.36 sh -c \
  'f=$(ls /d/kafka/t/1_*/tx.snapshot); ls -l $f; dd if=/dev/zero of=$f bs=53 count=1 conv=notrunc 2>/dev/null; echo zeroed $f'
dc start n1 >/dev/null
sleep 20
docker inspect -f 'n1: {{.State.Status}}, restarts {{.RestartCount}}' "$(dc ps -aq n1)"
dc logs n1 2>&1 | grep -E "Can't load snapshot|Corrupt snapshot|Assert" | tail -3 | cut -c1-400
echo "clean up: docker compose -p $P down -v"
