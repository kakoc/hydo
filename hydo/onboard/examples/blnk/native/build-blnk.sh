#!/bin/sh
# Build Blnk at the commit of this example for this machine (the host's OS and architecture):
#   sh build-blnk.sh            # -> work/blnk
# The commit is main of 2026-11-11 (67d88333), after the release v0.15.8, as in ../build.sh.
# Needs git, network once, and Go; GOTOOLCHAIN fetches go1.27.3. The clone is in work/blnk-src.
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
COMMIT=67d8833307798320caa21dc0746a0667663ecb5c
SRC=$HERE/work/blnk-src
if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin https://github.com/blnkfinance/blnk.git
fi
git -C "$SRC" fetch -q --depth 1 origin "$COMMIT"
git -C "$SRC" -c advice.detachedHead=false checkout -q -f "$COMMIT"
(cd "$SRC" && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 \
   go build -trimpath -buildvcs=false -ldflags "-s -w" -o "$HERE/work/blnk" ./cmd)
echo "work/blnk"
