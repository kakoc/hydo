#!/bin/sh
# Build the test client in client/ (`rqc`: the setup, the workload's writes and transactions,
# the reads, the backups, the read-backs and the final checks) as static Linux programs:
# bin/rqc-amd64 (hvx and KVM) and bin/rqc-arm64 (the Mac engine). rqlite itself comes from its
# official image (hydo/fetch-images.sh). Needs Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a Mac
# and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o "../bin/rqc-$arch" .)
done
ls -l bin/rqc-amd64 bin/rqc-arm64
