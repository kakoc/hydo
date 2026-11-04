#!/usr/bin/env bash
# Build the miniflux example from its manifest (run from the repository root on the build machine).
# 1. miniflux v2.3.3, static and byte-reproducible (sha256 dd8e2456…):
#      git clone --depth 1 --branch v2.3.3 https://github.com/miniflux/v2.git && cd v2
#      CGO_ENABLED=0 GOTOOLCHAIN=local GOFLAGS=-mod=readonly go build -trimpath -buildvcs=false \
#        -ldflags "-s -w -buildid= -X miniflux.app/v2/internal/version.Version=2.3.3" -o miniflux .
#    (go 1.26.5: /nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5)
# 2. this script: HYDO_MINIFLUX_BIN=<that binary> hydo/onboard/examples/miniflux/build.sh
# Output: build/explore-ob-miniflux-disk.img
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="${HYDO_MINIFLUX_BIN:-$HOME/hydo/build/miniflux}"
install -m 0755 "$BIN" "$HERE/miniflux"
HB="${HYDO_BUILD_BIN:-$(command -v hydo-build || echo "$HERE/../../../../target/release/hydo-build")}"   # the Rust image builder
[ -x "$HB" ] || { echo "error: no hydo-build binary: cargo build --release -p hydo-build (or set HYDO_BUILD_BIN)" >&2; exit 1; }
HYDO_ONBOARD="$HERE/../.." "$HB" "$HERE/hydo.yaml" --out "explore-ob-miniflux" "$@"
