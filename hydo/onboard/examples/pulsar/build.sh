#!/bin/sh
# Build the test client in client/ (`pulsarc`: the topic's setup, the workload's operations, the
# drain and the transaction check) as static Linux programs for the hvx engine, twice:
#
#   bin/pulsarc-release-amd64  with pulsar-client-go v0.21.0, the newest release (client/go.mod)
#   bin/pulsarc-fix-amd64      with the head of pulsar-client-go pull request 1499
#                              (a4d7bf4b, client/go.fix.mod), which puts a transaction's id into
#                              each message's metadata: with v0.21.0 an aborted transaction's
#                              messages are delivered (pulsar-client-go#1498, RESULTS.md finding 1)
#
# The build parameter `client` of hydo.yaml chooses one. Pulsar itself comes from its official
# image (hydo/fetch-images.sh). Needs Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a Mac and on
# Linux; the same Go release gives the same bytes.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
arch=${1:-amd64}
for c in release fix; do
  mod=go.mod
  [ "$c" = release ] || mod=go.$c.mod
  (cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -modfile=$mod -trimpath -buildvcs=false -ldflags "-s -w -buildid=" -o "../bin/pulsarc-$c-$arch" .)
done
ls -l bin/pulsarc-*-"$arch"
