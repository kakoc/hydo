#!/bin/sh
# Build the test client in client/ (`nsqc`: the publishes, the consumers, the drain and the
# read-back) as static Linux programs: bin/nsqc-amd64 (hvx and KVM) and bin/nsqc-arm64 (the Mac
# engine). NSQ itself comes from its official image (hydo/fetch-images.sh). Needs Go 1.27.3
# (GOTOOLCHAIN fetches it). Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o "../bin/nsqc-$arch" .)
done
ls -l bin/nsqc-amd64 bin/nsqc-arm64
