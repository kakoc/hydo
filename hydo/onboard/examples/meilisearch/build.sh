#!/bin/sh
# Build the test client in client/ (`mlc`: the workload's operations and the checks) as static
# Linux programs: bin/mlc-amd64 (hvx and KVM) and bin/mlc-arm64 (the Mac engine). Meilisearch
# itself comes from its official image (hydo/fetch-images.sh).
# Needs Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w" -o "../bin/mlc-$arch" .)
done
ls -l bin/mlc-amd64 bin/mlc-arm64
