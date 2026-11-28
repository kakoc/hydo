#!/bin/sh
# Build nsqd v1.3.0 from NSQ's source with its debug information, for the live map
# (hydo/hydo-map.yaml): bin/nsqd-map-arm64, a static Linux program for the Mac engine. It is built
# as the official image nsqio/nsq:v1.3.0 builds it (Go 1.21.5, CGO_ENABLED=0), but without
# -ldflags "-s -w", which drop the debug information that the live map reads, and with -trimpath,
# so that the debug information names the files by their module paths. The sources go to
# work/nsq (NSQ at v1.3.0) and work/go-diskqueue (go-diskqueue v1.1.0, the version that v1.3.0
# requires), which `sources:` of hydo-map.yaml names.
# Needs git, network once, and Go (GOTOOLCHAIN fetches Go 1.21.5). Works on a Mac and on Linux.
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
NSQ_COMMIT=f580340fd5be61a94d0fef388458f027925c7fc0   # the tag v1.3.0
DQ_COMMIT=v1.1.0
GO_TOOLCHAIN=go1.21.5   # the Go of the official image's nsqd (`go version -m`)
fetch() {   # dir url commit
  if [ ! -d "$1/.git" ]; then
    mkdir -p "$1"
    git -C "$1" init -q
    git -C "$1" remote add origin "$2"
  fi
  git -C "$1" fetch -q --depth 1 origin "$3"
  git -C "$1" -c advice.detachedHead=false checkout -q -f FETCH_HEAD
}
fetch work/nsq https://github.com/nsqio/nsq.git "$NSQ_COMMIT"
fetch work/go-diskqueue https://github.com/nsqio/go-diskqueue.git "$DQ_COMMIT"
mkdir -p bin
(cd work/nsq && GOTOOLCHAIN=$GO_TOOLCHAIN CGO_ENABLED=0 GOOS=linux GOARCH=arm64 \
   go build -trimpath -buildvcs=false -o "$HERE/bin/nsqd-map-arm64" ./apps/nsqd)
ls -l bin/nsqd-map-arm64
