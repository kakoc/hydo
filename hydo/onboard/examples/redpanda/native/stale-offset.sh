#!/bin/sh
# Finding 4 without hydo: a produce answer with the offset of an earlier epoch's batch.
# Starts the three-node cluster of ../docker-compose.yaml, builds native/staleoffset (Go, with
# franz-go 1.22.2 as in the guest's client) and runs it in the client container:
#   retry: transaction 1 writes A0, transaction 2 (the same transactional id) writes B0, the
#          partition's leader moves, and B0's Produce request is sent again to the new leader;
#   loss:  transaction 1 writes A0 and A1, transaction 2 writes B0, the leader moves, and
#          transaction 2 writes B1 and commits;
# each mode then reads the partition back and lists every record that is not at the offset its
# producer was given. `-transfer=false` is the control: the leader stays. Needs Docker and Go.
#   sh native/stale-offset.sh [rounds]      (default 10)
set -eu
cd "$(dirname "$0")/.."
P=rpoff
n=${1:-10}
arch=$(uname -m); case $arch in arm64|aarch64) arch=arm64 ;; *) arch=amd64 ;; esac
dc() { docker compose -p $P "$@"; }
bin=$(mktemp -d)
(cd native/staleoffset && CGO_ENABLED=0 GOOS=linux GOARCH=$arch GOFLAGS=-mod=mod go build -o "$bin/staleoffset" .)
dc down -v >/dev/null 2>&1 || true
dc up -d n1 n2 n3 client >/dev/null
docker cp "$bin/staleoffset" "$(dc ps -q client)":/staleoffset >/dev/null
rm -rf "$bin"
dc exec -T client /staleoffset create
dc exec -T client /staleoffset retry -n "$n" -p 0 || true
dc exec -T client /staleoffset retry -n "$n" -p 0 -transfer=false || true
dc exec -T client /staleoffset loss -n "$n" -p 1 || true
dc exec -T client /staleoffset loss -n "$n" -p 2 -transfer=false || true
echo "clean up: docker compose -p $P down -v"
