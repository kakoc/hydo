#!/bin/sh
# Build the test client in client/ (`rmq`: the sends, the transactions, the consumers, the drain)
# as a static Linux program for x86_64: bin/rmq-amd64 (hvx and KVM). RocketMQ itself comes from
# its official image (hydo/fetch-images.sh). Needs Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a
# Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
(cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
   go build -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o ../bin/rmq-amd64 .)
ls -l bin/rmq-amd64
