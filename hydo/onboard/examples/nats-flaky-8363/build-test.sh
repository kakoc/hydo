#!/bin/sh
# Build the two test programs this example runs, as static linux/amd64 binaries:
#   bin/ats-buggy.test  nats-server main at NATS_COMMIT (server/ats as issue 8363 describes it)
#   bin/ats-fixed.test  the head of pull request 8794 (Unregister waits for the goroutine)
# Needs git, network once and a Go that can fetch the module's toolchain (go1.26.10, through
# GOTOOLCHAIN). Works on a Mac and on Linux; the bytes are checked against the pins below.
#   sh build-test.sh            # both
#   NATS_SRC=<dir> sh build-test.sh   # reuse a clone
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
NATS_COMMIT=59bf235aea63881477dd485af3579be2ffc32b0c   # main, 2026-11-06
FIX_COMMIT=436addad5f9db3983772fe3b85969e1f51d2b99d    # refs/pull/8794/head
GO_TOOLCHAIN=go1.26.10                                  # the module's toolchain line
SRC=${NATS_SRC:-$HERE/work/nats-server}
if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$(dirname "$SRC")"
  git clone -q --filter=blob:none https://github.com/nats-io/nats-server.git "$SRC"
fi
git -C "$SRC" fetch -q origin "$NATS_COMMIT" "refs/pull/8794/head"
mkdir -p bin
build() {   # commit output
  git -C "$SRC" -c advice.detachedHead=false checkout -q "$1"
  (cd "$SRC/server/ats" && GOTOOLCHAIN=$GO_TOOLCHAIN CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
     go test -c -trimpath -o "$HERE/bin/$2" .)
}
build "$NATS_COMMIT" ats-buggy.test
build "$FIX_COMMIT" ats-fixed.test
check() {   # file sha256
  got=$( (sha256sum "$1" 2>/dev/null || shasum -a 256 "$1") | cut -d' ' -f1)
  if [ "$got" = "$2" ]; then echo "$1 ok"; else echo "$1: sha256 $got, expected $2" >&2; exit 1; fi
}
check bin/ats-buggy.test 521e509e58777fd6108cff3a7c70e85e81c2d3836bc5500603de02e2414c8c8d
check bin/ats-fixed.test eea9f033d28231d049543dfcce6dcc6ecd5c4000e99f183b954e533fc201ec90
