#!/bin/sh
# Build the test programs this example runs, as static Linux programs (CGO_ENABLED=0):
#   bin/cache-buggy-<arch>.test  etcd main at ETCD_COMMIT, the module go.etcd.io/etcd/cache/v3
#   bin/cache-fixed-<arch>.test  the same commit with fix-test.patch applied (a test-only change)
# for <arch> amd64 (hvx and KVM) and arm64 (the Mac engine).
# Needs git, network once and a Go that can fetch the module's toolchain (go1.27.4, through
# GOTOOLCHAIN). Works on a Mac and on Linux; the bytes are checked against the pins below.
#   sh build-test.sh                  # all four
#   ETCD_SRC=<dir> sh build-test.sh   # reuse a clone
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
ETCD_COMMIT=1b969e26a2c65733f1ff696e3537373d56b3ae67   # main, 2026-11-12, after the failing CI run
GO_TOOLCHAIN=go1.27.4                                  # cache/go.mod's toolchain line
SRC=${ETCD_SRC:-$HERE/work/etcd}
if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin https://github.com/etcd-io/etcd.git
fi
git -C "$SRC" fetch -q --depth 1 origin "$ETCD_COMMIT"
mkdir -p bin
build() {   # variant arch
  (cd "$SRC/cache" && GOTOOLCHAIN=$GO_TOOLCHAIN CGO_ENABLED=0 GOOS=linux GOARCH=$2 \
     go test -c -trimpath -o "$HERE/bin/cache-$1-$2.test" .)
}
git -C "$SRC" -c advice.detachedHead=false checkout -q -f "$ETCD_COMMIT"
build buggy amd64
build buggy arm64
git -C "$SRC" apply "$HERE/fix-test.patch"
build fixed amd64
build fixed arm64
git -C "$SRC" checkout -q -f "$ETCD_COMMIT"
check() {   # file sha256
  got=$( (sha256sum "$1" 2>/dev/null || shasum -a 256 "$1") | cut -d' ' -f1)
  if [ "$got" = "$2" ]; then echo "$1 ok"; else echo "$1: sha256 $got, expected $2" >&2; exit 1; fi
}
check bin/cache-buggy-amd64.test 09ae0a670871cef4c433255689b5eccd3c4452c8f2bf02cc99062ab6aef2400e
check bin/cache-buggy-arm64.test 7cc6a546a5cd87169707694736ca006388a4e3f8700e35458059968a91e8f590
check bin/cache-fixed-amd64.test be92ee626a918f85c339a64e49213a05cd9ad9a1b29581156ee024458065ae75
check bin/cache-fixed-arm64.test 1202ab71c9dea0f4dcd2beee3a61b87ca50a96eac5580e11663a35069848cb9e
