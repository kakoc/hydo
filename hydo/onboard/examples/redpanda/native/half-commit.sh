#!/bin/sh
# Finding 3 without hydo: a commit that returns no error while one of its records is aborted.
# Starts the three-node cluster of ../docker-compose.yaml, creates the topic t, builds
# native/halfcommit (Go) and runs it in the client container:
#   steps: producer A writes to partition 1, producer B with the same transactional id sends
#          InitProducerId, A writes to partition 2 and commits;
#   race:  pairs of producers with one transactional id at the same time, with no fault;
# then reads partitions 1 and 2 with read_committed and lists every record of a commit that
# returned no error and that the reader never reads. Needs Docker and Go, and
# ../bin/rpclient-<arch> (for the topic).
#   sh native/half-commit.sh [steps] [race-pairs]      (defaults: 20 and 0)
set -eu
cd "$(dirname "$0")/.."
P=rphalf
nsteps=${1:-20}
pairs=${2:-0}
arch=$(uname -m); case $arch in arm64|aarch64) arch=arm64 ;; *) arch=amd64 ;; esac
dc() { docker compose -p $P "$@"; }
bin=$(mktemp -d)
(cd native/halfcommit && CGO_ENABLED=0 GOOS=linux GOARCH=$arch GOFLAGS=-mod=mod go build -o "$bin/halfcommit" .)
dc down -v >/dev/null 2>&1 || true
dc up -d n1 n2 n3 client >/dev/null
dc exec -T -e HYDO_HIST_OUT=off client /rp/rpclient-$arch create
docker cp "$bin/halfcommit" "$(dc ps -q client)":/halfcommit >/dev/null
rm -rf "$bin"
dc exec -T client sh -c 'cd /tmp && rm -f committed.txt'
if [ "$nsteps" -gt 0 ]; then dc exec -T client sh -c "cd /tmp && /halfcommit steps -n $nsteps"; fi
if [ "$pairs" -gt 0 ]; then dc exec -T client sh -c "cd /tmp && /halfcommit race -n $pairs"; fi
dc exec -T client sh -c 'cd /tmp && /halfcommit check' || true
echo "the raw partitions: docker compose -p $P exec client /rp/rpclient-$arch dump 1 0"
echo "clean up: docker compose -p $P down -v"
