#!/bin/sh
# Build the test client in client/ (`dfc`: the workload's operations, the wait for the replicas
# and the checks) as static Linux programs: bin/dfc-amd64 (hvx and KVM) and bin/dfc-arm64 (the
# Mac engine). Dragonfly itself comes from its official image (hydo/fetch-images.sh).
# Needs Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w" -o "../bin/dfc-$arch" .)
done
ls -l bin/dfc-amd64 bin/dfc-arm64
