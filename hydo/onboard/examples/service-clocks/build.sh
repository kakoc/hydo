#!/bin/sh
# Builds the example's static Go program clockprobe next to hydo.yaml (Milestone 372). Needs Go
# (any version from 1.22); GO names another go program.
#   hydo/onboard/examples/service-clocks/build.sh
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
GO="${GO:-go}"
cd "$HERE/clockprobe"
CGO_ENABLED=0 GOOS=linux GOARCH=amd64 GOFLAGS=-mod=mod "$GO" build -trimpath \
  -ldflags "-s -w -buildid=" -o "$HERE/clockprobe.bin" .
echo "built $HERE/clockprobe.bin"
