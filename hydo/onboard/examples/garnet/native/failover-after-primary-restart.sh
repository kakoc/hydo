#!/bin/sh
# failover-after-primary-restart.sh: what a default CLUSTER FAILOVER does in finding 1's state,
# with Docker (RESULTS.md, finding 1 and "Not bugs").
#
# A primary and one replica of Garnet 2.2.14 with Garnet's default
# --cluster-replication-reestablishment-timeout 0. The script kills the primary with SIGKILL,
# starts it again with --recover, writes 10 keys, and sends CLUSTER FAILOVER (no option) to the
# replica, which has none of the 10. It then sends a write to the primary, prints both roles and
# the replica's failover state three times, counts the 10 keys on the replica, and prints the
# replica's log lines about the failover.
#
#   sh failover-after-primary-restart.sh [image]
#
# It removes its containers, volumes and network at the end. It needs Docker and the image
# redis:7-alpine for redis-cli.
set -u
IMG=${1:-ghcr.io/microsoft/garnet:2.2.14}
NET=gm371b P=gm371b-p R=gm371b-r
PIP=10.89.72.11 RIP=10.89.72.12
cleanup() {
  docker rm -f $P $R > /dev/null 2>&1
  docker volume rm -f ${P}-v ${R}-v > /dev/null 2>&1
  docker network rm $NET > /dev/null 2>&1
}
cleanup
docker network create --subnet 10.89.72.0/24 $NET > /dev/null
cli() { docker run --rm --network $NET redis:7-alpine redis-cli -h "$@"; }
start2() { # start2 <name> <ip>
  docker run -d --name "$1" --network $NET --ip "$2" --user root -v "$1-v:/data" "$IMG" \
    --cluster --bind "$2" --port 6379 --checkpointdir /data/garnet --recover \
    --aof --aof-commit-wait --lua > /dev/null
}
waitping() { # waitping <ip>
  for _ in $(seq 1 60); do [ "$(cli "$1" PING 2>/dev/null)" = PONG ] && return 0; sleep 1; done
  echo "$1 does not answer PING"; return 1
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
for i in 1 2 3 4 5; do cli $PIP SET "{t}a$i" "v$i" > /dev/null; done
sleep 2
docker kill -s KILL $P > /dev/null
docker start $P > /dev/null
waitping $PIP
for i in 1 2 3 4 5 6 7 8 9 10; do cli $PIP SET "{t}b$i" "w$i" > /dev/null; done
echo "== CLUSTER FAILOVER on the replica: $(cli $RIP CLUSTER FAILOVER)"
sleep 2
echo "a write on the primary during the failover: $(cli $PIP SET '{t}c1' x 2>&1)"
for k in 5 15 30; do
  sleep 5
  echo "== after about $k s: the primary is $(cli $PIP ROLE | head -1), the replica $(cli $RIP ROLE | head -1); $(cli $RIP INFO replication | grep -E 'master_failover_state|last_failover_state' | tr '\r\n' '  ')"
done
n=0
for i in 1 2 3 4 5 6 7 8 9 10; do
  v=$(docker run --rm --network $NET redis:7-alpine sh -c "printf 'READONLY\r\nGET {t}b$i\r\n' | redis-cli -h $RIP" | tail -1)
  [ "$v" = "w$i" ] && n=$((n + 1))
done
echo "the replica holds $n of the 10 keys written after the restart"
docker logs $R 2>&1 | grep -i -E 'failover|catch' | tail -6
cleanup
