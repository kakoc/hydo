#!/usr/bin/env bash
# hydo/bench/build-bench.sh -- build the bug-benchmark guest images (Milestone 46b).
#
# For each defect patch in hydo/bench/patches/<name>.patch: copy the fixed front door
# (hydo/stack), apply the patch, and build the explorer image build/explore-bench-<name>-disk.img
# with build-stack.sh (the STACK_OUT=explore* prefix gives it the explorer's serve mode).
# Everything except the front-door binary is the same as explore-nf-disk.img (the control).
#
#   bash hydo/bench/build-bench.sh              # every defect
#   bash hydo/bench/build-bench.sh d3-blind-retry d5-myisam
#
# Run from the repo root on the build box. Extra environment (e.g. HYDO_GCC_MUSL,
# HYDO_MUSL_ROOT, HYDO_REDIS_OUT when the pinned store paths are gone) is passed through to
# build-stack.sh. BENCH_KEEP=1 keeps the intermediate ext4/data images and the root tree.
set -euo pipefail
cd "$(dirname "$0")/../.."
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  for p in hydo/bench/patches/*.patch; do names+=("$(basename "$p" .patch)"); done
fi
for n in "${names[@]}"; do
  p=hydo/bench/patches/$n.patch
  [ -f "$p" ] || { echo "no such defect: $n" >&2; exit 1; }
  src=build/bench-src/$n
  rm -rf "$src"; mkdir -p "$src"
  cp -a hydo/stack/. "$src/"
  # the patches are written against the fixed front door of M39-M46 (commit 0112b48), not the
  # current hydo/stack/main.go (M50 changed the code they patch), and without the M56 BUGGIFY
  # wrappers (buggify.go is not built here)
  cp hydo/bench/base/main-0112b48.go "$src/main.go"; rm -f "$src/buggify.go"
  ( cd "$src" && patch -s -p1 < "$OLDPWD/$p" )
  echo "== $n: building build/explore-bench-$n-disk.img"
  STACK_OUT=explore-bench-$n STACK_GO_SRC="$src" bash hv/hvx/images/build-stack.sh
  if [ "${BENCH_KEEP:-0}" != 1 ]; then
    chmod -R u+w build/rootfs-explore-bench-$n-root 2>/dev/null || true
    rm -rf build/rootfs-explore-bench-$n-root build/explore-bench-$n-ext4.img build/explore-bench-$n-data.img
  fi
done
