#!/bin/sh
# Build the programs of this example as static Linux programs (CGO_ENABLED=0):
#   bin/blnk-<arch>        Blnk at BLNK_COMMIT (main of 2026-11-11, after the release v0.15.8),
#                          the upstream ./cmd, as its Dockerfile builds it
#   bin/blnk-fixed-<arch>  the same with fix-overdraft-limit.patch and fix-atomic-bulk.patch
#                          (`hydo build --set blnk=blnk-fixed`)
#   bin/blnkc-<arch>       the test client in client/ (the workload's commands and the checks)
# for <arch> amd64 (hvx and KVM) and arm64 (the Mac engine).
# Needs git, network once, and Go 1.27.3 (GOTOOLCHAIN fetches it). Works on a Mac and on Linux;
# the bytes are checked against the pins below.
#   sh build.sh                    # all six
#   BLNK_SRC=<dir> sh build.sh     # reuse a clone (its work tree is reset to BLNK_COMMIT)
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
# main after v0.15.8 (2026-11-02): it has the fixes of the upstream issues 413, 418, 421 and 422,
# which v0.15.8 lacks (RESULTS.md, "The version")
BLNK_COMMIT=67d8833307798320caa21dc0746a0667663ecb5c
GO_TOOLCHAIN=go1.27.3
SRC=${BLNK_SRC:-$HERE/work/blnk}
if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin https://github.com/blnkfinance/blnk.git
fi
git -C "$SRC" fetch -q --depth 1 origin "$BLNK_COMMIT"
git -C "$SRC" -c advice.detachedHead=false checkout -q -f "$BLNK_COMMIT"
mkdir -p bin
build() {   # output name
  for arch in amd64 arm64; do
    (cd "$SRC" && GOTOOLCHAIN=$GO_TOOLCHAIN CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
       go build -trimpath -buildvcs=false -ldflags "-s -w" -o "$HERE/bin/$1-$arch" ./cmd)
  done
}
build blnk
git -C "$SRC" apply "$HERE/fix-overdraft-limit.patch" "$HERE/fix-atomic-bulk.patch"
build blnk-fixed
git -C "$SRC" checkout -q -f "$BLNK_COMMIT"
for arch in amd64 arm64; do
  (cd client && GOTOOLCHAIN=$GO_TOOLCHAIN CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w" -o "$HERE/bin/blnkc-$arch" .)
done
check() {   # file sha256
  got=$( (sha256sum "$1" 2>/dev/null || shasum -a 256 "$1") | cut -d' ' -f1)
  if [ "$got" = "$2" ]; then echo "$1 ok"; else echo "$1: sha256 $got, expected $2" >&2; exit 1; fi
}
check bin/blnk-amd64 bd33333a2da2e9ca873f22c0b272cb84b222d123132eceffcdb3a04a2c780023
check bin/blnk-arm64 4ad13e51cb08b7a027fa48f2aedfcf3524e188fef0b0a88aeec02ba819dbfa9c
check bin/blnk-fixed-amd64 fc2523dd8058205e6a69ecebb97ece7165ecf3a26d65d48a679763f30c781679
check bin/blnk-fixed-arm64 f1785b46a60c30ea3262a2d5341eb3286ab571c0f32a8312f1be1c8b2e762d8d
check bin/blnkc-amd64 6c45211eda7cc90283e3539aa0fd5142a09a2283eeb5694cfcca10117d73dc6d
check bin/blnkc-arm64 0eba5b43e9df583df63c650b959df539a9d4df6d2aa6cc04b998bbdc305135f8
