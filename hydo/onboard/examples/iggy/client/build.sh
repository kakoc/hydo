#!/bin/sh
# Build the static test client for the guest: client/build.sh [amd64|arm64] -> bin/iggyc-<arch>
# (CGO off, trimmed paths, no build id: the same Go release gives the same bytes on any machine).
# Needs Go 1.25 or later; go.sum pins Iggy's Go SDK v0.10.0 and its dependencies.
set -eu
cd "$(dirname "$0")"
arch=${1:-amd64}
mkdir -p ../bin
CGO_ENABLED=0 GOOS=linux GOARCH=$arch GOFLAGS=-mod=mod \
  go build -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o "../bin/iggyc-$arch" .
sha256sum "../bin/iggyc-$arch" 2>/dev/null || shasum -a 256 "../bin/iggyc-$arch"
