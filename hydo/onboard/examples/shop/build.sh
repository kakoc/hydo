#!/usr/bin/env bash
# Build the shop demo (hydo.yaml in this directory): hydo-world (Rust, hydo/crates/hydo-world), the two shop binaries (the
# planted bug and the fix: src/charge_buggy.go / src/charge_fixed.go, chosen by the Go build tag
# "fixed"), and the two images build/explore-ob-shop-buggy-disk.img and -fixed-disk.img.
# Run from the repository root on the build box; needs build/hvx_seedrng built with the pinned
# musl gcc (docs/BUILD-PINS.md) or HYDO_SEEDRNG. Pinned Go toolchain, standard library only.
#   hydo/onboard/examples/shop/build.sh [binaries]     (binaries: stop after the three programs)
#   SHOP_IMG=<prefix> (default explore-ob-shop) names the images <prefix>-buggy / <prefix>-fixed
#   SHOP_GOARCH=arm64 builds the arm64 programs instead -- build/arm64/shop-{buggy,fixed} and
#   build/hydo-world-arm64 -- and stops there: the arm64 images are built with the Mac release
#   (`hydo build hydo.yaml --set variant=buggy --set bin=<dir>`, or hydo-build --arch arm64).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../../.." && pwd)"
GO="${HYDO_GO:-/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go}"
[ -x "$GO" ] || GO=/nix/store/xrgn6m0c5vbz06467q06zdwnk6qqdl7z-go-1.26.5/bin/go
[ -x "$GO" ] || { echo "error: no pinned Go at $GO (set HYDO_GO)" >&2; exit 1; }
mkdir -p "$TOP/build"
ARCH="${SHOP_GOARCH:-amd64}"
if [ "$ARCH" = amd64 ]; then BIN="$TOP/build"; WORLD="$TOP/build/hydo-world"
else BIN="$TOP/build/$ARCH"; WORLD="$TOP/build/hydo-world-$ARCH"; mkdir -p "$BIN"; fi
HYDO_WORLD_ARCH="$ARCH" "$TOP/hydo/crates/hydo-world/build.sh" "$WORLD"   # Rust (M111)
for v in buggy fixed; do
  tags=""; [ $v = fixed ] && tags="-tags=fixed"
  ( cd "$HERE/src" && GOPATH="$TOP/build/world-gopath" GOMODCACHE="$TOP/build/world-gomod" GOCACHE="$TOP/build/world-gocache" \
    GOFLAGS=-mod=readonly GOTOOLCHAIN=local CGO_ENABLED=0 GOOS=linux GOARCH="$ARCH" \
    "$GO" build $tags -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$BIN/shop-$v" . )
  echo "built $BIN/shop-$v (sha256 $(sha256sum "$BIN/shop-$v" | cut -c1-16))"
done
[ "${1:-}" = binaries ] || [ "$ARCH" != amd64 ] && exit 0
HB="${HYDO_BUILD_BIN:-$(command -v hydo-build || echo "$TOP/target/release/hydo-build")}"   # the Rust image builder
[ -x "$HB" ] || { echo "error: no hydo-build binary: cargo build --release -p hydo-build (or set HYDO_BUILD_BIN)" >&2; exit 1; }
cd "$TOP"
for v in buggy fixed; do
  HYDO_ONBOARD="$TOP/hydo/onboard" "$HB" "$HERE/hydo.yaml" --set variant=$v --out "${SHOP_IMG:-explore-ob-shop}-$v"
done
