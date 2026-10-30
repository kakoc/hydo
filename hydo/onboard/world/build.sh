#!/usr/bin/env bash
# Build hydo-world (the fake outside services and fault proxies, MANIFEST.md "Fake outside
# services") as one static binary with the pinned Go toolchain (docs/BUILD-PINS.md). Standard
# library only: no module downloads. Byte-reproducible: CGO off, -trimpath, no VCS stamp, empty
# build id. hydo-build stages <build dir>/hydo-world (or $HYDO_WORLDBIN) for manifests with fakes:.
#   hydo/onboard/world/build.sh [out]          (default: build/hydo-world, from the repository root)
# HYDO_WORLD_GOARCH=arm64 builds the arm64 binary for hydo-build --arch arm64, which stages
# <build dir>/hydo-world-arm64 (or $HYDO_WORLDBIN_ARM64); default amd64.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${1:-build/hydo-world}"
GO="${HYDO_GO:-/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go}"
[ -x "$GO" ] || GO=/nix/store/xrgn6m0c5vbz06467q06zdwnk6qqdl7z-go-1.26.5/bin/go
[ -x "$GO" ] || { echo "error: no pinned Go at $GO (set HYDO_GO)" >&2; exit 1; }
GOARCH="${HYDO_WORLD_GOARCH:-amd64}"
mkdir -p "$(dirname "$OUT")"
OUT="$(cd "$(dirname "$OUT")" && pwd)/$(basename "$OUT")"
T="$(dirname "$OUT")"
( cd "$HERE" && GOPATH="$T/world-gopath" GOMODCACHE="$T/world-gomod" GOCACHE="$T/world-gocache" \
  GOFLAGS=-mod=readonly GOTOOLCHAIN=local CGO_ENABLED=0 GOOS=linux GOARCH="$GOARCH" \
  "$GO" build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$OUT" . )
echo "built $OUT ($(stat -c%s "$OUT") bytes, static Go $GOARCH, sha256 $(sha256sum "$OUT" | cut -c1-16))"
