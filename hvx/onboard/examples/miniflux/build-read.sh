#!/usr/bin/env bash
# Build the miniflux example with the store-reader invariants (hydo-read.yaml); the miniflux
# binary as in build.sh (HYDO_MINIFLUX_BIN), hydo-read from build/hydo-read or HYDO_READ_BIN.
# Output: build/explore-ob-miniflux-read-disk.img (run from the repository root).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="${HYDO_MINIFLUX_BIN:-$HOME/hydo/build/miniflux}"
install -m 0755 "$BIN" "$HERE/miniflux"
PY3="${HYDO_PY3:-$(command -v python3 || ls /nix/store/*python3-3.11*/bin/python3 | head -1)}"
"$PY3" "$HERE/../../hydo-build" "$HERE/hydo-read.yaml" --out "explore-ob-miniflux-read" "$@"
