#!/bin/sh
# Build riverq (app/: a small application on River v0.51.2 with its own test client) as static
# Linux programs: bin/riverq-amd64 (hvx, KVM) and bin/riverq-arm64 (the Mac engine).
# Needs Go; GOTOOLCHAIN fetches go1.27.3 once. Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
mkdir -p bin
for arch in amd64 arm64; do
  (cd app && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w" -o "../bin/riverq-$arch" .)
done
ls -l bin
