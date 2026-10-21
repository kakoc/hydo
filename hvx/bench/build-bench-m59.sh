#!/usr/bin/env bash
# hvx/bench/build-bench-m59.sh -- the harder benchmark defects d8..d12 (Milestone 59).
#
# Base front door: hvx/bench/base/main-m59-control.go = the order service of commit 3773607
# (hvx/bench/base/main-3773607.go: the Milestone 50 fixes and the M56 BUGGIFY wrappers) plus a
# fix for the Milestone 58 flush-cut bug (a request that READ published=1 commits a real write
# before it ACKs). For "control" that file is built as is; for a defect, the patch
# hvx/bench/patches-m59/<name>.patch is applied to it first. Each image is
# build/explore-bench-m59-<name>-disk.img (STACK_OUT=explore* gives the explorer's serve mode,
# STACK_GO_SRC points build-stack.sh at the copy). The rest of the guest is the current
# stack-explore guest, so every image accepts the Q=, X=, B= and cf= tokens.
#
#   bash hvx/bench/build-bench-m59.sh                 # control + every m59 defect
#   bash hvx/bench/build-bench-m59.sh control d8-no-order-lock
#
# Run from the repo root on the build box, same environment as build-bench-bg.sh
# (HYDO_GCC_MUSL, HYDO_MUSL_ROOT, HYDO_REDIS_OUT when the pinned store paths are gone).
set -euo pipefail
cd "$(dirname "$0")/../.."
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  names=(control)
  for p in hvx/bench/patches-m59/*.patch; do names+=("$(basename "$p" .patch)"); done
fi
for n in "${names[@]}"; do
  src=build/bench-m59-src/$n
  rm -rf "$src"; mkdir -p "$src"
  cp -a hvx/stack/. "$src/"
  # the stored base, not hvx/stack/main.go (another line of work may change that file)
  cp hvx/bench/base/main-m59-control.go "$src/main.go"
  if [ "$n" != control ]; then
    p=hvx/bench/patches-m59/$n.patch
    [ -f "$p" ] || { echo "no such defect: $n" >&2; exit 1; }
    ( cd "$src" && patch -s -p1 < "$OLDPWD/$p" )
  fi
  echo "== $n: building build/explore-bench-m59-$n-disk.img"
  STACK_OUT=explore-bench-m59-$n STACK_GO_SRC="$src" bash ./build-stack.sh
  if [ "${BENCH_KEEP:-0}" != 1 ]; then
    chmod -R u+w build/rootfs-explore-bench-m59-$n-root 2>/dev/null || true
    rm -rf build/rootfs-explore-bench-m59-$n-root build/explore-bench-m59-$n-ext4.img build/explore-bench-m59-$n-data.img
  fi
done
