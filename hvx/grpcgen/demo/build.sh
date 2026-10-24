#!/usr/bin/env bash
# Build the gRPC demo's images on the build box (pinned Go, hvx/BUILD-PINS.md):
#   hvx/grpcgen/demo/build.sh [buggy] [fixed]          (default: both)
# 1. orderd, static (the service under test);
# 2. hydo-grpc-gen: proto/ + hints.yaml -> build/gen (schema, plan, static hydo-grpc client);
# 3. hydo-build hydo.yaml --set mode=<m> --out explore-ob-grpcorders-<m> (images in $HYDO_BUILD_DIR,
#    default <repo>/build).
# HYDO_SEEDRNG must name an hvx_seedrng built with the pinned static musl gcc.
set -euo pipefail
D="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOP="$(cd "$D/../../.." && pwd)"
GO="${HYDO_GO:-/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go}"
[ -x "$GO" ] || GO=/nix/store/xrgn6m0c5vbz06467q06zdwnk6qqdl7z-go-1.26.5/bin/go
[ -x "$GO" ] || GO=go
export HYDO_GO="$GO"
mkdir -p "$D/build"
( cd "$TOP/hvx/grpcgen" && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 GOFLAGS=-mod=readonly GOTOOLCHAIN=local \
  "$GO" build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$D/build/orderd" ./demo/orderd )
"$TOP/hvx/onboard/hydo-grpc-gen" "$D/proto" --hints "$D/hints.yaml" --out "$D/build/gen"
sha256sum "$D/build/orderd" "$D/build/gen/hydo-grpc" "$D/build/gen/grpc/desc.pb" "$D/build/gen/grpc/plan.json" 2>/dev/null || true
for m in ${@:-buggy fixed}; do
  "$TOP/hvx/onboard/hydo-build" "$D/hydo.yaml" --set mode=$m --out explore-ob-grpcorders-$m --build-dir "${HYDO_BUILD_DIR:-$TOP/build}"
done
