#!/bin/sh
# Build the test client in client/ (`kvc`: the setup, the workload's writes, the failover, the
# read-back and the final checks) as static Linux programs: bin/kvc-amd64 (hvx and KVM) and
# bin/kvc-arm64 (the Mac engine). Kvrocks itself comes from its official image
# (hydo/fetch-images.sh). Needs Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o "../bin/kvc-$arch" .)
done
ls -l bin/kvc-amd64 bin/kvc-arm64
