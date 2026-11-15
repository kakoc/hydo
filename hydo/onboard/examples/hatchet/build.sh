#!/bin/sh
# Build hq (app/: a small application on Hatchet's Go SDK v0.120.5 with its own test client) as
# static Linux programs: bin/hq-amd64 (hvx, KVM) and bin/hq-arm64 (the Mac engine, Docker on a Mac).
# Needs Go; GOTOOLCHAIN fetches go1.27.3 once. Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd app && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w" -o "../bin/hq-$arch" .)
done
ls -l bin
