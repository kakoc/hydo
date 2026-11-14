#!/bin/sh
# Build the test program of etcd's module go.etcd.io/etcd/cache/v3 for this machine, at the
# commit of ../build-test.sh:
#   sh build-program.sh        # -> work/cache.test
# On Linux, ../build-test.sh gives the pinned static programs too (bin/cache-buggy-amd64.test).
# Needs git, network once, and Go; GOTOOLCHAIN fetches go1.27.4, the toolchain line of
# cache/go.mod. The clone is in work/etcd-src.
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
COMMIT=1b969e26a2c65733f1ff696e3537373d56b3ae67   # main, 2026-11-12, after the failing CI run
SRC=$HERE/work/etcd-src
if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin https://github.com/etcd-io/etcd.git
fi
git -C "$SRC" fetch -q --depth 1 origin "$COMMIT"
git -C "$SRC" -c advice.detachedHead=false checkout -q -f "$COMMIT"
(cd "$SRC/cache" && GOTOOLCHAIN=go1.27.4 CGO_ENABLED=0 go test -c -trimpath -o "$HERE/work/cache.test" .)
echo "work/cache.test"
