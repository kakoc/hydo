#!/usr/bin/env bash
# Build the two checkout programs of the emulators example (hydo.yaml in this directory): the
# planted bug (src/key_buggy.go) and the fix (src/key_fixed.go, the Go build tag "fixed"), as
# static Linux programs, standard library only. It also builds s3probe (s3probe/) for amd64 and
# arm64, the client of hydo-s3.yaml.
#   hydo/onboard/examples/checkout/build.sh [out dir]    (default: build/ at the repository root)
#   CHECKOUT_GOARCH=arm64 (for the Mac engine) or amd64 (the default); HYDO_GO names the Go.
# Then, with a hydo release:
#   hydo build hydo/onboard/examples/checkout/hydo.yaml --set bin=<out dir> --set variant=buggy
# `hydo build` fetches the three emulators' images into emulators/ here, by digest.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../../.." && pwd)"
GO="${HYDO_GO:-$(command -v go)}"
OUT="${1:-$TOP/build}"
ARCH="${CHECKOUT_GOARCH:-amd64}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
for v in buggy fixed; do
  tags=""; [ $v = fixed ] && tags="-tags=fixed"
  ( cd "$HERE/src" && GOFLAGS=-mod=readonly GOTOOLCHAIN=local CGO_ENABLED=0 GOOS=linux GOARCH="$ARCH" \
    "$GO" build $tags -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$OUT/checkout-$v" . )
  if command -v sha256sum > /dev/null; then SHA="$(sha256sum "$OUT/checkout-$v" | cut -c1-16)"
  else SHA="$(shasum -a 256 "$OUT/checkout-$v" | cut -c1-16)"; fi
  echo "built $OUT/checkout-$v ($ARCH, sha256 $SHA)"
done
# s3probe of hydo-s3.yaml (Milestone 341), for both architectures, so one folder serves every engine
for a in amd64 arm64; do
  ( cd "$HERE/s3probe" && GOFLAGS=-mod=readonly GOTOOLCHAIN=local CGO_ENABLED=0 GOOS=linux GOARCH="$a" \
    "$GO" build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$OUT/s3probe-$a" . )
  if command -v sha256sum > /dev/null; then SHA="$(sha256sum "$OUT/s3probe-$a" | cut -c1-16)"
  else SHA="$(shasum -a 256 "$OUT/s3probe-$a" | cut -c1-16)"; fi
  echo "built $OUT/s3probe-$a ($a, sha256 $SHA)"
done
