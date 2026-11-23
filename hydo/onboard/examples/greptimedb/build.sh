#!/bin/sh
# Build the test client in client/ (`gtc`: the workload's operations, the wait for the cluster,
# the drain and the final checks) as static Linux programs: bin/gtc-amd64 (hvx and KVM) and
# bin/gtc-arm64 (the Mac engine). GreptimeDB itself comes from its official image
# (hydo/fetch-images.sh). Needs Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o "../bin/gtc-$arch" .)
done
ls -l bin/gtc-amd64 bin/gtc-arm64
