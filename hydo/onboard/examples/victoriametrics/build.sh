#!/bin/sh
# Build the test client in client/ (`vmc`: the workload's operations, the wait for the cluster,
# the drain and the query check) as static Linux programs: bin/vmc-amd64 (hvx and KVM) and
# bin/vmc-arm64 (the Mac engine). VictoriaMetrics itself comes from its official images
# (hydo/fetch-images.sh). Needs Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o "../bin/vmc-$arch" .)
done
ls -l bin/vmc-amd64 bin/vmc-arm64
