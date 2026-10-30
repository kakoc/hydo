#!/usr/bin/env bash
# Build the stack example driven by the generated gRPC driver (hydo-grpcgen.yaml).
# FRONT=fix (default) or FRONT=old, as in build.sh. Run from the repository root.
# Output: build/explore-ob-stackgen-$FRONT-disk.img
set -euo pipefail
FRONT="${FRONT:-fix}"
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../../.." && pwd)"
case "$FRONT" in
  fix) BIN="${HYDO_FRONT_BIN:-$HOME/hydo/build/explore-fix-hydostack}";;
  old) BIN="${HYDO_FRONT_BIN:-$HOME/hydo/build/explore-hydostack}";;
  *) echo "FRONT must be fix or old" >&2; exit 2;;
esac
install -m 0755 "$BIN" "$HERE/front-door"
"$TOP/hydo/onboard/hydo-grpc-gen" "$TOP/hydo/stack/proto" --hints "$TOP/hydo/stack/grpc-hints.yaml" --out "$HERE/grpcgen"
echo "*" > "$HERE/grpcgen/.gitignore"   # generated: not tracked
PY3="${HYDO_PY3:-$(command -v python3 || ls /nix/store/*python3-3.11*/bin/python3 | head -1)}"
"$PY3" "$HERE/../../hydo-build" "$HERE/hydo-grpcgen.yaml" --out "explore-ob-stackgen-$FRONT" "$@"
