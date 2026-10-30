#!/usr/bin/env bash
# Build the stack example with error logs as findings (hydo-errors.yaml). FRONT=fix
# (default) or FRONT=old as in build.sh.
# Output: build/explore-ob-stack-errors-$FRONT-disk.img (run from the repository root).
set -euo pipefail
FRONT="${FRONT:-fix}"
HERE="$(cd "$(dirname "$0")" && pwd)"
case "$FRONT" in
  fix) BIN="${HYDO_FRONT_BIN:-$HOME/hydo/build/explore-fix-hydostack}";;
  old) BIN="${HYDO_FRONT_BIN:-$HOME/hydo/build/explore-hydostack}";;
  *) echo "FRONT must be fix or old" >&2; exit 2;;
esac
install -m 0755 "$BIN" "$HERE/front-door"
PY3="${HYDO_PY3:-$(command -v python3 || ls /nix/store/*python3-3.11*/bin/python3 | head -1)}"
"$PY3" "$HERE/../../hydo-build" "$HERE/hydo-errors.yaml" --out "explore-ob-stack-errors-$FRONT" "$@"
