#!/usr/bin/env bash
# hvx/bench/build-bench-bg.sh -- the bug benchmark rebuilt with BUGGIFY fault points (M56).
#
# For each defect (and "control", the unpatched front door): copy hvx/stack (the SDK with
# hydoassert.Buggify and the wrappers in buggify.go), put the defect's base front door in as
# main.go -- hvx/bench/base/main-0112b48.go (the fixed front door the patches were written
# against) for d1..d7 and the control, main-7e32c4a.go (the pre-M39 front door) for d0 --
# apply the defect patch, route the store calls through the wrappers (addbuggify.py), and
# build build/explore-bench-bg-<name>-disk.img with build-stack.sh. Every image is in the
# CURRENT guest (stack-explore.sh with the B= token), d0 included, so all share one control.
# Without a B= token the fault points are off and a run behaves as in the plain image.
#
#   bash hvx/bench/build-bench-bg.sh                  # control + d0..d7
#   bash hvx/bench/build-bench-bg.sh control d7-skip-last-record
#
# Run from the repo root on the build box (same environment as build-bench.sh).
set -euo pipefail
cd "$(dirname "$0")/../.."
# python3 is not on the build box's PATH (NixOS): the same lookup as build-stack.sh
PY3="${HYDO_PY3:-}"
[ -x "$PY3" ] || PY3=$(command -v python3 || true)
[ -x "$PY3" ] || PY3=/nix/store/14bkh67gwzg2hvqkadi03dyinrd7iybd-python3-3.11.9-env/bin/python3
[ -x "$PY3" ] || PY3=$(ls /nix/store/*python3*/bin/python3 2>/dev/null | head -1 || true)
[ -x "$PY3" ] || { echo "error: python3 not found (set HYDO_PY3)" >&2; exit 1; }
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  names=(control d0-old-front-door)
  for p in hvx/bench/patches/*.patch; do names+=("$(basename "$p" .patch)"); done
fi
for n in "${names[@]}"; do
  src=build/bench-bg-src/$n
  rm -rf "$src"; mkdir -p "$src"
  cp -a hvx/stack/. "$src/"
  case "$n" in
    control) cp hvx/bench/base/main-0112b48.go "$src/main.go";;
    d0-old-front-door) cp hvx/bench/base/main-7e32c4a.go "$src/main.go";;
    *)
      p=hvx/bench/patches/$n.patch
      [ -f "$p" ] || { echo "no such defect: $n" >&2; exit 1; }
      cp hvx/bench/base/main-0112b48.go "$src/main.go"
      ( cd "$src" && patch -s -p1 < "$OLDPWD/$p" )
      ;;
  esac
  "$PY3" hvx/bench/addbuggify.py "$src/main.go"
  echo "== $n: building build/explore-bench-bg-$n-disk.img"
  STACK_OUT=explore-bench-bg-$n STACK_GO_SRC="$src" bash ./build-stack.sh
  if [ "${BENCH_KEEP:-0}" != 1 ]; then
    chmod -R u+w build/rootfs-explore-bench-bg-$n-root 2>/dev/null || true
    rm -rf build/rootfs-explore-bench-bg-$n-root build/explore-bench-bg-$n-ext4.img build/explore-bench-bg-$n-data.img
  fi
done
