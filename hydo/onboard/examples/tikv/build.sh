#!/bin/sh
# Build the test client in client/ (`tkc`: the workload's operations, the history read-backs and
# the checks) as static Linux programs: bin/tkc-amd64 (hvx and KVM) and bin/tkc-arm64 (the Mac
# engine and Docker on a Mac). TiKV and PD come from their official images (hydo/fetch-images.sh).
# Needs Go 1.25.12 or newer (GOTOOLCHAIN fetches it). Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd client && CGO_ENABLED=0 GOOS=linux GOARCH=$arch GOFLAGS=-mod=mod \
     go build -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o "../bin/tkc-$arch" .)
done
ls -l bin/tkc-amd64 bin/tkc-arm64
