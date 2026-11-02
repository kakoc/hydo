#!/bin/sh
# Build the static test client for the guest: client/build.sh [arm64|amd64] -> bin/natsjs-<arch>
# (CGO off, trimmed paths, no build id: the same Go release gives the same bytes on any machine).
set -eu
cd "$(dirname "$0")"
arch=${1:-arm64}
mkdir -p ../bin
CGO_ENABLED=0 GOOS=linux GOARCH=$arch GOFLAGS=-mod=mod \
  go build -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o "../bin/natsjs-$arch" .
shasum -a 256 "../bin/natsjs-$arch"
