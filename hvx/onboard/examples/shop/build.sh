#!/usr/bin/env bash
# Build the shop demo (hydo.yaml in this directory): hydo-world, the two shop binaries (the
# planted bug and the fix: src/charge_buggy.go / src/charge_fixed.go, chosen by the Go build tag
# "fixed"), and the two images build/explore-ob-shop-buggy-disk.img and -fixed-disk.img.
# Run from the repository root on the build box; needs build/hvx_seedrng built with the pinned
# musl gcc (hvx/BUILD-PINS.md) or HYDO_SEEDRNG. Pinned Go toolchain, standard library only.
#   hvx/onboard/examples/shop/build.sh [binaries]     (binaries: stop after the three programs)
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../../.." && pwd)"
GO="${HYDO_GO:-/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go}"
[ -x "$GO" ] || GO=/nix/store/xrgn6m0c5vbz06467q06zdwnk6qqdl7z-go-1.26.5/bin/go
[ -x "$GO" ] || { echo "error: no pinned Go at $GO (set HYDO_GO)" >&2; exit 1; }
mkdir -p "$TOP/build"
HYDO_GO="$GO" "$TOP/hvx/onboard/world/build.sh" "$TOP/build/hydo-world"
for v in buggy fixed; do
  tags=""; [ $v = fixed ] && tags="-tags=fixed"
  ( cd "$HERE/src" && GOPATH="$TOP/build/world-gopath" GOMODCACHE="$TOP/build/world-gomod" GOCACHE="$TOP/build/world-gocache" \
    GOFLAGS=-mod=readonly GOTOOLCHAIN=local CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
    "$GO" build $tags -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$TOP/build/shop-$v" . )
  echo "built build/shop-$v (sha256 $(sha256sum "$TOP/build/shop-$v" | cut -c1-16))"
done
[ "${1:-}" = binaries ] && exit 0
PY3="${HYDO_PY3:-$(command -v python3 || ls /nix/store/*python3-3.11*/bin/python3 | head -1)}"
cd "$TOP"
for v in buggy fixed; do
  "$PY3" hvx/onboard/hydo-build "$HERE/hydo.yaml" --set variant=$v --out "explore-ob-shop-$v"
done
