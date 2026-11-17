#!/bin/sh
# Build the static test client for the guest: client/build.sh [arm64|amd64] -> bin/rpclient-<arch>
# (CGO off, trimmed paths, no build id: the same Go release gives the same bytes on any machine).
set -eu
cd "$(dirname "$0")"
arch=${1:-amd64}
mkdir -p ../bin
CGO_ENABLED=0 GOOS=linux GOARCH=$arch GOFLAGS=-mod=mod \
  go build -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o "../bin/rpclient-$arch" .
shasum -a 256 "../bin/rpclient-$arch"
