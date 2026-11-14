#!/usr/bin/env bash
# Build the two market programs of the OpenAPI fakes example (hydo.yaml in this directory): the
# planted bug (src/retry_buggy.go) and the fix (src/retry_fixed.go, the Go build tag "fixed"), as
# static Linux programs, standard library only.
#   hydo/onboard/examples/openapi-payouts/build.sh [out dir]   (default: build/ at the repository root)
#   PAYOUTS_GOARCH=arm64 (for the Mac engine) or amd64 (the default); HYDO_GO names the Go.
# Then, with a hydo release:
#   hydo build hydo/onboard/examples/openapi-payouts/hydo.yaml --set bin=<out dir> --set variant=buggy
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../../.." && pwd)"
GO="${HYDO_GO:-$(command -v go)}"
OUT="${1:-$TOP/build}"
ARCH="${PAYOUTS_GOARCH:-amd64}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
for v in buggy fixed; do
  tags=""; [ $v = fixed ] && tags="-tags=fixed"
  ( cd "$HERE/src" && GOFLAGS=-mod=readonly GOTOOLCHAIN=local CGO_ENABLED=0 GOOS=linux GOARCH="$ARCH" \
    "$GO" build $tags -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$OUT/market-$v" . )
  if command -v sha256sum > /dev/null; then SHA="$(sha256sum "$OUT/market-$v" | cut -c1-16)"
  else SHA="$(shasum -a 256 "$OUT/market-$v" | cut -c1-16)"; fi
  echo "built $OUT/market-$v ($ARCH, sha256 $SHA)"
done
