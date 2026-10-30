#!/usr/bin/env bash
# Build the stack example from its manifest. FRONT=fix (default: the fixed front door, M39)
# or FRONT=old (the pre-fix front door that has the M38 durable-defaults bug). The front-door
# binaries are the prebuilt ones from build-stack.sh's explorer images.
# Output: build/explore-ob-stack-$FRONT-disk.img (run from the repository root).
set -euo pipefail
FRONT="${FRONT:-fix}"
HERE="$(cd "$(dirname "$0")" && pwd)"
case "$FRONT" in
  fix) BIN="${HYDO_FRONT_BIN:-$HOME/hydo/build/explore-fix-hydostack}";;
  old) BIN="${HYDO_FRONT_BIN:-$HOME/hydo/build/explore-hydostack}";;
  *) echo "FRONT must be fix or old" >&2; exit 2;;
esac
install -m 0755 "$BIN" "$HERE/front-door"
HB="${HYDO_BUILD_BIN:-$(command -v hydo-build || echo "$HERE/../../../../target/release/hydo-build")}"   # the Rust image builder
[ -x "$HB" ] || { echo "error: no hydo-build binary: cargo build --release -p hydo-build (or set HYDO_BUILD_BIN)" >&2; exit 1; }
HYDO_ONBOARD="$HERE/../.." "$HB" "$HERE/hydo.yaml" --out "explore-ob-stack-$FRONT" "$@"
