#!/bin/sh
# Build the test client in client/ (`sdbc`: the workload's operations, the history read-backs and
# the checks) as static Linux programs: bin/sdbc-amd64 (hvx and KVM) and bin/sdbc-arm64 (the Mac
# engine). SurrealDB itself comes from its official image (hydo/fetch-images.sh).
# Needs Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w" -o "../bin/sdbc-$arch" .)
done
ls -l bin/sdbc-amd64 bin/sdbc-arm64
