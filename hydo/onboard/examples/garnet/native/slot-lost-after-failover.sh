#!/bin/sh
# slot-lost-after-failover.sh: finding 2 without hydo, with Docker (RESULTS.md).
#
# Garnet 2.2.14 in cluster mode: three primaries a, b and c, and ra, the replica of a. The script
# writes 5 keys of the slot 8943 (hash tag {t1}) on b, moves the slot to a with MIGRATE ... SLOTS,
# waits until a and b name a as its owner, and at once sends CLUSTER FAILOVER (no option) to ra.
# It then prints which node each of the four names as the slot's owner (CLUSTER SLOTS, "-" for
# none), after the failover and three times after a SIGKILL and a start of all four nodes, and
# sends a GET of one key to b.
#
#   sh slot-lost-after-failover.sh [image]
#   MIGSLEEP=6 sh slot-lost-after-failover.sh     # wait 6 s between the migration and the failover
#
# It removes its containers, volumes and network at the end. It needs Docker and the image
# redis:7-alpine for redis-cli.
set -u
IMG=${1:-ghcr.io/microsoft/garnet:2.2.14}
NET=gm371c
A=10.89.73.11 B=10.89.73.12 C=10.89.73.13 RA=10.89.73.14
NAMES="gm371c-a gm371c-b gm371c-c gm371c-ra"
SLOT=8943
cleanup() {
  for n in $NAMES; do docker rm -f "$n" > /dev/null 2>&1; docker volume rm -f "$n-v" > /dev/null 2>&1; done
  docker network rm $NET > /dev/null 2>&1
}
cleanup
docker network create --subnet 10.89.73.0/24 $NET > /dev/null
cli() { docker run --rm --network $NET redis:7-alpine redis-cli -h "$@"; }
start2() { # start2 <name> <ip>
  docker run -d --name "$1" --network $NET --ip "$2" --user root -v "$1-v:/data" "$IMG" \
    --cluster --bind "$2" --port 6379 --checkpointdir /data/garnet --recover \
    --aof --aof-commit-wait --lua --cluster-replication-reestablishment-timeout 5 > /dev/null
}
waitping() { # waitping <ip>
  for _ in $(seq 1 60); do [ "$(cli "$1" PING 2>/dev/null)" = PONG ] && return 0; sleep 1; done
  echo "$1 does not answer PING"; return 1
}
owner() { # owner <ip>: the address of the slot's owner in that node's CLUSTER SLOTS, or -
  cli "$1" CLUSTER SLOTS | awk -v s=$SLOT '{l[NR]=$0} END {
    for (i = 1; i <= NR; i++)
      if (l[i] ~ /^[0-9]+$/ && l[i+1] ~ /^[0-9]+$/ && l[i]+0 <= s && l[i+1]+0 >= s) { print l[i+2]; exit }
    print "-" }'
}
show() { echo "$1: a says $(owner $A), b says $(owner $B), c says $(owner $C), ra says $(owner $RA)"; }
start2 gm371c-a $A; start2 gm371c-b $B; start2 gm371c-c $C; start2 gm371c-ra $RA
for ip in $A $B $C $RA; do waitping $ip; done
i=1
for ip in $A $B $C $RA; do cli $ip CLUSTER SET-CONFIG-EPOCH $i > /dev/null; i=$((i + 1)); done
cli $A CLUSTER ADDSLOTSRANGE 0 5460 > /dev/null
cli $B CLUSTER ADDSLOTSRANGE 5461 10922 > /dev/null
cli $C CLUSTER ADDSLOTSRANGE 10923 16383 > /dev/null
for ip in $B $C $RA; do cli $A CLUSTER MEET $ip 6379 > /dev/null; done
sleep 6
AID=$(cli $A CLUSTER MYID)
for _ in $(seq 1 30); do [ "$(cli $RA CLUSTER REPLICATE "$AID")" = OK ] && break; sleep 1; done
for _ in $(seq 1 60); do cli $RA INFO replication | grep -q 'master_link_status:up' && break; sleep 1; done
for i in 1 2 3 4 5; do cli $B SET "{t1}k$i" "v$i" > /dev/null; done
show "formed (a=$A b=$B c=$C ra=$RA)"
echo "MIGRATE of slot $SLOT from b to a: $(cli $B MIGRATE $A 6379 '' 0 5000 SLOTS $SLOT)"
for _ in $(seq 1 30); do [ "$(owner $B)" = "$A" ] && [ "$(owner $A)" = "$A" ] && break; sleep 1; done
sleep "${MIGSLEEP:-0}"
show "after the migration"
echo "CLUSTER FAILOVER on ra: $(cli $RA CLUSTER FAILOVER)"
for _ in $(seq 1 60); do cli $RA ROLE | head -1 | grep -q master && break; sleep 1; done
sleep 6
show "after the failover"
docker kill -s KILL $NAMES > /dev/null
docker start $NAMES > /dev/null
for ip in $A $B $C $RA; do waitping $ip; done
for k in 10 20 30; do sleep 10; show "after the restart of all four (about $k s)"; done
echo "GET {t1}k1 through b: $(cli $B GET '{t1}k1' 2>&1)"
echo "GET {t1}k1 through ra: $(cli $RA GET '{t1}k1' 2>&1)"
cleanup
