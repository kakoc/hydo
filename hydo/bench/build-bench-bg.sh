#!/usr/bin/env bash
# hydo/bench/build-bench-bg.sh -- the bug benchmark rebuilt with BUGGIFY fault points (M56).
#
# For each defect (and "control", the unpatched front door): copy hydo/stack (the SDK with
# hydoassert.Buggify and the wrappers in buggify.go), put the defect's base front door in as
# main.go -- hydo/bench/base/main-0112b48.go (the fixed front door the patches were written
# against) for d1..d7 and the control, main-7e32c4a.go (the pre-M39 front door) for d0 --
# apply the defect patch, route the store calls through the wrappers (hydo-bench addbuggify), and
# build build/explore-bench-bg-<name>-disk.img with build-stack.sh. Every image is in the
# CURRENT guest (stack-explore.sh with the B= token), d0 included, so all share one control.
# Without a B= token the fault points are off and a run behaves as in the plain image.
#
#   bash hydo/bench/build-bench-bg.sh                  # control + d0..d7
#   bash hydo/bench/build-bench-bg.sh control d7-skip-last-record
#
# Run from the repo root on the build box (same environment as build-bench.sh).
set -euo pipefail
cd "$(dirname "$0")/../.."
# addbuggify (hydo-bench addbuggify; addbuggify.py until M108, same output): $HYDO_BENCH_BIN, else
# hydo-bench on PATH, else this tree's target/release/hydo-bench (cargo build --release -p hydo-bench)
BENCH=${HYDO_BENCH_BIN:-$(command -v hydo-bench || echo target/release/hydo-bench)}
[ -x "$BENCH" ] || { echo "error: hydo-bench not found (set HYDO_BENCH_BIN)" >&2; exit 1; }
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  names=(control d0-old-front-door)
  for p in hydo/bench/patches/*.patch; do names+=("$(basename "$p" .patch)"); done
fi
for n in "${names[@]}"; do
  src=build/bench-bg-src/$n
  rm -rf "$src"; mkdir -p "$src"
  cp -a hydo/stack/. "$src/"
  case "$n" in
    control) cp hydo/bench/base/main-0112b48.go "$src/main.go";;
    d0-old-front-door) cp hydo/bench/base/main-7e32c4a.go "$src/main.go";;
    *)
      p=hydo/bench/patches/$n.patch
      [ -f "$p" ] || { echo "no such defect: $n" >&2; exit 1; }
      cp hydo/bench/base/main-0112b48.go "$src/main.go"
      ( cd "$src" && patch -s -p1 < "$OLDPWD/$p" )
      ;;
  esac
  "$BENCH" addbuggify "$src/main.go"
  echo "== $n: building build/explore-bench-bg-$n-disk.img"
  STACK_OUT=explore-bench-bg-$n STACK_GO_SRC="$src" bash hv/hvx/images/build-stack.sh
  if [ "${BENCH_KEEP:-0}" != 1 ]; then
    chmod -R u+w build/rootfs-explore-bench-bg-$n-root 2>/dev/null || true
    rm -rf build/rootfs-explore-bench-bg-$n-root build/explore-bench-bg-$n-ext4.img build/explore-bench-bg-$n-data.img
  fi
done
