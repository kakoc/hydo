#!/usr/bin/env bash
# hydo/bench/build-order-services.sh -- every order service of the benchmark as a static binary
# (Milestone 135), for build-bench-manifest.sh (ORDER_BIN=<out dir>). The sources are assembled
# exactly as build-bench.sh, build-bench-m59.sh and build-bench-bg.sh assemble them (a copy of
# hydo/stack with the stored base main.go, the defect's patch, BUGGIFY wrappers for bg-*), and
# built as build-stack.sh 6.1 builds them (pinned Go 1.26.5, CGO_ENABLED=0, -trimpath,
# -buildvcs=false, -ldflags "-s -w -buildid=", GOFLAGS=-mod=readonly, GOTOOLCHAIN=local).
#
#   build-order-services.sh <out dir> [<stem> ...]      -> <out dir>/<stem>-hydostack
#
# Stems: control, d8-no-order-lock .. d12-shared-key-buffer (base main-m59-control.go), nf and
# d1-kafka-first .. d7-skip-last-record (main-0112b48.go, no buggify.go), d0-old-front-door
# (main-7e32c4a.go), fix (= nf: the M39 fix, the binary explore-fix and explore-nf share), and
# bg-control, bg-d0-old-front-door, bg-d1-... .. bg-d7-... (hydo-bench addbuggify over the same
# bases). Default: every stem except bg-* (BG=1 adds them).
# The module files are hydo/stack/go.mod and go.sum (franz-go v1.22.2 since M135);
# STACK_GOMOD=baseline uses go.mod.baseline/go.sum.baseline (franz-go v1.17.1, the stack images'
# versions), which reproduces the stack images' binaries. HYDO_GO is the pinned go binary,
# HYDO_GOMODCACHE the module cache (default <out dir>/.gomod), HYDO_BENCH_BIN hydo-bench.
set -euo pipefail
cd "$(dirname "$0")/../.."
TOP=$PWD
out=$1; shift
mkdir -p "$out"; out=$(cd "$out" && pwd)
GO="${HYDO_GO:-/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go}"
[ -x "$GO" ] || GO=/nix/store/xrgn6m0c5vbz06467q06zdwnk6qqdl7z-go-1.26.5/bin/go
[ -x "$GO" ] || { echo "error: no pinned Go 1.26.5 (set HYDO_GO)" >&2; exit 1; }
"$GO" version | grep -q 'go1.26.5 ' || { echo "error: $GO is not go1.26.5" >&2; exit 1; }
BENCH=${HYDO_BENCH_BIN:-$(command -v hydo-bench || echo "$TOP/target/release/hydo-bench")}
M59="d8-no-order-lock d9-redis-unlocked d10-outcome-in-memory d11-ack-unflushed-flag d12-shared-key-buffer"
OLD="d1-kafka-first d2-batched-publish-flag d3-blind-retry d4-produce-timeout-definite d5-myisam d6-client-deadline-definite d7-skip-last-record"
if [ $# = 0 ]; then
  set -- control $M59 nf $OLD d0-old-front-door fix
  if [ "${BG:-0}" = 1 ]; then
    set -- "$@" bg-control bg-d0-old-front-door
    for d in $OLD; do set -- "$@" "bg-$d"; done
  fi
fi
src_of() {   # <stem> <dir>: the stem's sources
  local s=$1 d=$2 base p bg=0
  case "$s" in bg-*) bg=1; s=${s#bg-};; esac
  rm -rf "$d"; mkdir -p "$d"; cp -a hydo/stack/. "$d/"
  rm -f "$d/go.mod.baseline" "$d/go.sum.baseline"
  if [ "${STACK_GOMOD:-current}" = baseline ]; then
    cp hydo/stack/go.mod.baseline "$d/go.mod"; cp hydo/stack/go.sum.baseline "$d/go.sum"
  fi
  case "$bg $s" in
    "0 control"|"0 d8-"*|"0 d9-"*|"0 d10-"*|"0 d11-"*|"0 d12-"*)
      cp hydo/bench/base/main-m59-control.go "$d/main.go"
      if [ "$s" != control ]; then
        [ -f "hydo/bench/patches-m59/$s.patch" ] || { echo "error: no such order service: $1" >&2; exit 1; }
        ( cd "$d" && patch -s -p1 < "$TOP/hydo/bench/patches-m59/$s.patch" )
      fi
      return;;
  esac
  case "$s" in
    d0-old-front-door) base=main-7e32c4a.go;;
    nf|fix|control) base=main-0112b48.go;;   # bg-control: the M46 order service with BUGGIFY
    *) base=main-0112b48.go; p=hydo/bench/patches/$s.patch
       [ -f "$p" ] || { echo "error: no such order service: $1" >&2; exit 1; };;
  esac
  cp "hydo/bench/base/$base" "$d/main.go"
  [ -n "${p:-}" ] && ( cd "$d" && patch -s -p1 < "$TOP/$p" )
  if [ $bg = 1 ]; then
    "$BENCH" addbuggify "$d/main.go" > /dev/null
  else
    rm -f "$d/buggify.go"
  fi
}
for s in "$@"; do
  d=$out/.src/$s
  src_of "$s" "$d"
  ( cd "$d" && GOPATH="$out/.gopath" GOMODCACHE="${HYDO_GOMODCACHE:-$out/.gomod}" \
      GOCACHE="$out/.gocache" GOFLAGS=-mod=readonly GOTOOLCHAIN=local \
      CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
      "$GO" build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$out/$s-hydostack" . )
  echo "$s $(sha256sum < "$out/$s-hydostack" | cut -c1-16) $(stat -c%s "$out/$s-hydostack")"
done
