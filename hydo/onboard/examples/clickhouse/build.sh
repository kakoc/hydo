#!/bin/sh
# Build the test client in client/ (`chc`: the schema, the inserts, the reads, the drain and the
# checks) as a static Linux program for x86_64: bin/chc-amd64. ClickHouse and ClickHouse Keeper
# come from their official images (hydo/fetch-images.sh). OUT names another output folder.
# Needs Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
OUT=${OUT:-$(pwd)/bin}
mkdir -p "$OUT"
(cd client && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
   go build -trimpath -buildvcs=false -ldflags "-s -w" -o "$OUT/chc-amd64" .)
ls -l "$OUT/chc-amd64"
