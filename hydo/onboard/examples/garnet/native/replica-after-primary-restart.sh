#!/bin/sh
# replica-after-primary-restart.sh: finding 1 without hydo, with Docker (RESULTS.md).
#
# A cluster of one primary and one replica of Garnet 2.2.14, both with --aof --aof-commit-wait
# and --recover, as the example runs them. The script writes 10 keys and reads them from the
# replica, kills the primary with SIGKILL, starts it again (it recovers from its AOF), writes 10
# more keys, waits WAIT seconds (default 60), reads all 10 from the replica with READONLY, and
# prints INFO replication of both nodes.
#
#   sh replica-after-primary-restart.sh [image]
#   REEST=5 sh replica-after-primary-restart.sh    # both nodes with
#                                                  # --cluster-replication-reestablishment-timeout 5
#
# It removes its containers, volumes and network at the end. It needs Docker and the image
# redis:7-alpine for redis-cli.
set -u
IMG=${1:-ghcr.io/microsoft/garnet:2.2.14}
WAIT=${WAIT:-60}
NET=gm371 P=gm371-p R=gm371-r
PIP=10.89.71.11 RIP=10.89.71.12
cleanup() {
  docker rm -f $P $R > /dev/null 2>&1
  docker volume rm -f ${P}-v ${R}-v > /dev/null 2>&1
  docker network rm $NET > /dev/null 2>&1
}
cleanup
docker network create --subnet 10.89.71.0/24 $NET > /dev/null
cli() { docker run --rm --network $NET redis:7-alpine redis-cli -h "$@"; }
# rget <ip> <key>: a read with READONLY first, on one connection
rget() { docker run --rm --network $NET redis:7-alpine sh -c "printf 'READONLY\r\nGET %s\r\n' '$2' | redis-cli -h $1" | tail -1; }
start2() { # start2 <name> <ip>: start one node at its own address
  set -- "$1" "$2" --cluster --bind "$2" --port 6379 --checkpointdir /data/garnet --recover \
    --aof --aof-commit-wait --lua
  [ -z "${REEST:-}" ] || set -- "$@" --cluster-replication-reestablishment-timeout "$REEST"
  n=$1 ip=$2; shift 2
  docker run -d --name "$n" --network $NET --ip "$ip" --user root -v "$n-v:/data" "$IMG" "$@" > /dev/null
}
waitping() { # waitping <ip>
  for _ in $(seq 1 60); do [ "$(cli "$1" PING 2>/dev/null)" = PONG ] && return 0; sleep 1; done
  echo "$1 does not answer PING"; return 1
}
count() { # count <prefix> <value prefix>: how many of the 10 keys the replica holds
  n=0
  for i in 1 2 3 4 5 6 7 8 9 10; do [ "$(rget $RIP "{t}$1$i")" = "$2$i" ] && n=$((n + 1)); done
  echo $n
}
start2 $P $PIP; start2 $R $RIP
waitping $PIP; waitping $RIP
cli $PIP CLUSTER SET-CONFIG-EPOCH 1 > /dev/null
cli $RIP CLUSTER SET-CONFIG-EPOCH 2 > /dev/null
cli $PIP CLUSTER ADDSLOTSRANGE 0 16383 > /dev/null
cli $PIP CLUSTER MEET $RIP 6379 > /dev/null
sleep 3
PID=$(cli $PIP CLUSTER MYID)
for _ in $(seq 1 30); do [ "$(cli $RIP CLUSTER REPLICATE "$PID")" = OK ] && break; sleep 1; done
for _ in $(seq 1 60); do cli $RIP INFO replication | grep -q 'master_link_status:up' && break; sleep 1; done
echo "== 10 writes before the kill"
for i in 1 2 3 4 5 6 7 8 9 10; do cli $PIP SET "{t}a$i" "v$i" > /dev/null; done
sleep 2
echo "the replica holds $(count a v) of the 10 keys written before the kill"
echo "== SIGKILL of the primary, and its start with --recover"
docker kill -s KILL $P > /dev/null
docker start $P > /dev/null
waitping $PIP
echo "the primary holds $(cli $PIP GET '{t}a10') for {t}a10 after its recovery"
echo "== 10 writes after the restart, then $WAIT s"
for i in 1 2 3 4 5 6 7 8 9 10; do cli $PIP SET "{t}b$i" "w$i" > /dev/null; done
sleep "$WAIT"
echo "the replica holds $(count b w) of the 10 keys written after the primary's restart"
echo "== INFO replication of the primary"
cli $PIP INFO replication | grep -E '^(role|connected_slaves|master_repl_offset|slave0)'
echo "== INFO replication of the replica"
cli $RIP INFO replication | grep -E '^(role|master_repl_offset|master_link_status|master_sync_in_progress)'
cleanup
