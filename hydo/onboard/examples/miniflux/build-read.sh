#!/usr/bin/env bash
# Build the miniflux example with the store-reader invariants (hydo-read.yaml); the miniflux
# binary as in build.sh (HYDO_MINIFLUX_BIN), hydo-read from build/hydo-read
# (hydo/crates/hydo-read/build.sh) or HYDO_READ_BIN.
# Output: build/explore-ob-miniflux-read-disk.img (run from the repository root).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="${HYDO_MINIFLUX_BIN:-$HOME/hydo/build/miniflux}"
install -m 0755 "$BIN" "$HERE/miniflux"
HB="${HYDO_BUILD_BIN:-$(command -v hydo-build || echo "$HERE/../../../../target/release/hydo-build")}"   # the Rust image builder
[ -x "$HB" ] || { echo "error: no hydo-build binary: cargo build --release -p hydo-build (or set HYDO_BUILD_BIN)" >&2; exit 1; }
HYDO_ONBOARD="$HERE/../.." "$HB" "$HERE/hydo-read.yaml" --out "explore-ob-miniflux-read" "$@"
