#!/usr/bin/env bash
# Build the miniflux example from its manifest (run from the repository root on the build box).
# 1. miniflux v2.3.3, static and byte-reproducible (sha256 dd8e2456…):
#      git clone --depth 1 --branch v2.3.3 https://github.com/miniflux/v2.git && cd v2
#      CGO_ENABLED=0 GOTOOLCHAIN=local GOFLAGS=-mod=readonly go build -trimpath -buildvcs=false \
#        -ldflags "-s -w -buildid= -X miniflux.app/v2/internal/version.Version=2.3.3" -o miniflux .
#    (go 1.26.5: /nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5)
# 2. this script: HYDO_MINIFLUX_BIN=<that binary> hvx/onboard/examples/miniflux/build.sh
# Output: build/explore-ob-miniflux-disk.img
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="${HYDO_MINIFLUX_BIN:-$HOME/hydo/build/miniflux}"
install -m 0755 "$BIN" "$HERE/miniflux"
PY3="${HYDO_PY3:-$(command -v python3 || ls /nix/store/*python3-3.11*/bin/python3 | head -1)}"
"$PY3" "$HERE/../../hydo-build" "$HERE/hydo.yaml" --out "explore-ob-miniflux" "$@"
