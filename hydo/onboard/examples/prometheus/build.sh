#!/bin/sh
# Build the test client in client/ (`promc`: the exporter, the workload's remote writes,
# snapshots and reads, and the checks) as static Linux programs: bin/promc-amd64 (hvx and KVM) and
# bin/promc-arm64 (the Mac engine). Prometheus itself comes from its official image
# (hydo/fetch-images.sh). Needs Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o "../bin/promc-$arch" .)
done
ls -l bin/promc-amd64 bin/promc-arm64
