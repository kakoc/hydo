#!/usr/bin/env bash
# Build the stack-grpc example (hydo.yaml here): the four-service stack with the gRPC fault proxy
# "orders" between the client and the order service. One image per order service:
#   VARIANT=cur      hydo/stack/main.go (retry-safe: M37 published flag, M50 per-order lock)
#   VARIANT=old      hydo/bench/base/main-0112b48.go (the M39 order service)
#   VARIANT=planted  cur with a planted defect (Place publishes an already-published order again)
# The client in every image is the cur client, built with one added file (target.go: dial
# $HYDO_GRPC_TARGET when it is set), so it reaches the order service through the proxy.
# Output: build/explore-ob-stack-grpc-$VARIANT-disk.img. Run from anywhere on the build box;
# tool pins as in build-stack.sh (Go) and hydo-build (HYDO_SEEDRNG, HYDO_WORLDBIN, ...).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../../.." && pwd)"
cd "$TOP"
VARIANT="${VARIANT:-cur}"
case "$VARIANT" in cur|old|planted) ;; *) echo "VARIANT must be cur, old or planted" >&2; exit 2;; esac
GO="${HYDO_GO:-/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go}"
[ -x "$GO" ] || GO=/nix/store/xrgn6m0c5vbz06467q06zdwnk6qqdl7z-go-1.26.5/bin/go
[ -x "$GO" ] || { echo "error: no pinned Go (set HYDO_GO)" >&2; exit 1; }

gobuild() {   # <src dir> <out>: as build-stack.sh 6.1 (byte-reproducible)
  mkdir -p build/stack-gomod build/stack-gocache
  ( cd "$1" && GOPATH="$TOP/build/stack-gopath" GOMODCACHE="${HYDO_GOMODCACHE:-$TOP/build/stack-gomod}" \
    GOCACHE="$TOP/build/stack-gocache" GOFLAGS=-mod=readonly GOTOOLCHAIN=local \
    CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
    "$GO" build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$2" . )
  echo "built $2 ($(stat -c%s "$2") bytes, sha256 $(sha256sum "$2" | cut -c1-16))"
}
src() {   # <variant>: build/stack-grpc-src/<variant>, the order service + client sources
  local d=build/stack-grpc-src/$1
  rm -rf "$d"; mkdir -p "$d"; cp -a hydo/stack/. "$d/"
  case "$1" in
    old) cp hydo/bench/base/main-0112b48.go "$d/main.go"; rm -f "$d/buggify.go";;   # as hydo/bench/build-bench.sh
    *)
      cat > "$d/target.go" <<'GO'
package main

import "os"

// clientTarget: where the client roles dial (the stack-grpc example points them at its proxy)
func clientTarget() string {
	if a := os.Getenv("HYDO_GRPC_TARGET"); a != "" {
		return a
	}
	return grpcAddr
}
GO
      [ "$(grep -c 'grpc.NewClient(grpcAddr,' "$d/main.go")" = 2 ] || { echo "error: main.go: expected 2 grpc.NewClient(grpcAddr, ...)" >&2; exit 1; }
      sed -i 's/grpc\.NewClient(grpcAddr,/grpc.NewClient(clientTarget(),/' "$d/main.go";;
  esac
  if [ "$1" = planted ]; then
    [ "$(grep -c '^	if published != 1 {$' "$d/main.go")" = 1 ] || { echo "error: main.go: expected one 'if published != 1 {'" >&2; exit 1; }
    sed -i 's|^	if published != 1 {$|	if true { // PLANTED DEFECT (stack-grpc): an order already published (published == 1) is sent to Kafka again|' "$d/main.go"
  fi
}
src cur; gobuild build/stack-grpc-src/cur "$TOP/build/stack-grpc-cur-hydostack"
if [ "$VARIANT" != cur ]; then src "$VARIANT"; gobuild "build/stack-grpc-src/$VARIANT" "$TOP/build/stack-grpc-$VARIANT-hydostack"; fi
install -m 0755 "build/stack-grpc-$VARIANT-hydostack" "$HERE/front-door"
install -m 0755 build/stack-grpc-cur-hydostack "$HERE/client"
HB="${HYDO_BUILD_BIN:-$(command -v hydo-build || echo "$HERE/../../../../target/release/hydo-build")}"   # the Rust image builder
[ -x "$HB" ] || { echo "error: no hydo-build binary: cargo build --release -p hydo-build (or set HYDO_BUILD_BIN)" >&2; exit 1; }
HYDO_ONBOARD="$HERE/../.." "$HB" "$HERE/hydo.yaml" --out "explore-ob-stack-grpc-$VARIANT" "$@"
