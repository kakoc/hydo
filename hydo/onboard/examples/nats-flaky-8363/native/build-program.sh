#!/bin/sh
# Build the test program of nats-server's package server/ats for this machine, at the commit of
# ../build-test.sh (main of 2026-11-06, which still has the race):
#   sh build-program.sh        # -> work/ats.test
# On Linux x86_64, ../build-test.sh gives the pinned static program too (bin/ats-buggy.test).
# Needs git, network once, and Go; GOTOOLCHAIN fetches go1.26.10, the module's toolchain line.
# The clone is in work/nats-src.
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
COMMIT=59bf235aea63881477dd485af3579be2ffc32b0c
SRC=$HERE/work/nats-src
if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin https://github.com/nats-io/nats-server.git
fi
git -C "$SRC" fetch -q --depth 1 origin "$COMMIT"
git -C "$SRC" -c advice.detachedHead=false checkout -q -f "$COMMIT"
(cd "$SRC/server/ats" && GOTOOLCHAIN=go1.26.10 CGO_ENABLED=0 go test -c -trimpath -o "$HERE/work/ats.test" .)
echo "work/ats.test"
