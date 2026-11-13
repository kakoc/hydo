#!/bin/sh
# Build the two programs of this example as static Linux programs (CGO_ENABLED=0):
#   bin/ledger-<arch>   Formance Ledger at LEDGER_COMMIT (tag v2.4.18), the upstream `main.go`
#   bin/ledgerc-<arch>  the test client in client/ (the workload's commands and the checks)
# for <arch> amd64 (hvx and KVM) and arm64 (the Mac engine).
# Needs git, network once, and Go 1.27.3 (the ledger's go.mod toolchain line; GOTOOLCHAIN fetches
# it). Works on a Mac and on Linux; the bytes are checked against the pins below.
#   sh build.sh                     # all four
#   LEDGER_SRC=<dir> sh build.sh    # reuse a clone
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
LEDGER_COMMIT=83cd8dc7a66ba02a0418a36ec45663b19e26dcb3   # tag v2.4.18, 2026-11-05
GO_TOOLCHAIN=go1.27.3
SRC=${LEDGER_SRC:-$HERE/work/ledger}
if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin https://github.com/formancehq/ledger.git
fi
git -C "$SRC" fetch -q --depth 1 origin "$LEDGER_COMMIT"
git -C "$SRC" -c advice.detachedHead=false checkout -q -f "$LEDGER_COMMIT"
mkdir -p bin
# The release's version strings (.goreleaser.yml), with the commit's date as the build date, so
# that the bytes do not depend on when the build ran.
LDFLAGS="-s -w -X github.com/formancehq/ledger/cmd.Version=v2.4.18 -X github.com/formancehq/ledger/cmd.BuildDate=2026-11-05T17:03:57+01:00 -X github.com/formancehq/ledger/cmd.Commit=83cd8dc"
for arch in amd64 arm64; do
  (cd "$SRC" && GOTOOLCHAIN=$GO_TOOLCHAIN CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "$LDFLAGS" -o "$HERE/bin/ledger-$arch" .)
  (cd client && GOTOOLCHAIN=$GO_TOOLCHAIN CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
     go build -trimpath -buildvcs=false -ldflags "-s -w" -o "$HERE/bin/ledgerc-$arch" .)
done
check() {   # file sha256
  got=$( (sha256sum "$1" 2>/dev/null || shasum -a 256 "$1") | cut -d' ' -f1)
  if [ "$got" = "$2" ]; then echo "$1 ok"; else echo "$1: sha256 $got, expected $2" >&2; exit 1; fi
}
[ "${PIN_CHECK:-1}" = 0 ] && exit 0
check bin/ledger-amd64 9a9e7267d0b07919366f61ada33d243ccd641bc7d7087daab7b7ce5a04eb2ef0
check bin/ledger-arm64 573b6d23c18505ff0d74cb0f8a32f3a44c7af11ba47d89c2729325997a5081b6
check bin/ledgerc-amd64 98de851e9527c43e3da730fb23e886aa39be09f017dfba65183e2f1dc2182ada
check bin/ledgerc-arm64 dbdcfb5b3ec783df324a798a85f4919540a82e7eb70cd7814fa4cd03ce4ec7ff
