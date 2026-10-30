#!/usr/bin/env bash
# Build hydo-read (also hydo-check, via a symlink) as one static, byte-reproducible binary:
#   hvx/readers/build.sh [OUT]          # from the repository root; OUT default build/hydo-read
# Pinned Go toolchain (the one build-stack.sh uses), dependency versions pinned by go.sum,
# CGO off, -trimpath, no build id. Modules are cached under build/readers-gomod.
set -euo pipefail
TOP="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${1:-$TOP/build/hydo-read}"
GO="${HYDO_GO:-/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go}"
[ -x "$GO" ] || GO=/nix/store/xrgn6m0c5vbz06467q06zdwnk6qqdl7z-go-1.26.5/bin/go
GOARCH="${HYDO_READ_GOARCH:-amd64}"
mkdir -p "$TOP/build/readers-gomod" "$TOP/build/readers-gocache" "$(dirname "$OUT")"
( cd "$TOP/hvx/readers" && \
  GOPATH="$TOP/build/readers-gopath" GOMODCACHE="$TOP/build/readers-gomod" GOCACHE="$TOP/build/readers-gocache" \
  GOFLAGS=-mod=readonly GOTOOLCHAIN=local CGO_ENABLED=0 GOOS=linux GOARCH="$GOARCH" \
  "$GO" build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$OUT" . )
echo "built $OUT ($(stat -c%s "$OUT") bytes, static Go $GOARCH, sha256 $(sha256sum "$OUT" | cut -c1-16))"
