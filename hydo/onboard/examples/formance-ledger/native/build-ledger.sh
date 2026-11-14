#!/bin/sh
# Build Formance Ledger from a release tag for this machine (the host's OS and architecture):
#   sh build-ledger.sh v2.4.18     # -> work/ledger-v2.4.18, the release with the bug
#   sh build-ledger.sh v2.3.24     # -> work/ledger-v2.3.24, the release before it
# Needs git, network once, and Go; GOTOOLCHAIN fetches go1.27.3, the toolchain line of
# v2.4.18's go.mod. The clone is in work/ledger-src (git-ignored).
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
TAG=${1:?usage: sh build-ledger.sh <v2.4.18|v2.3.24>}
case $TAG in
  v2.4.18) COMMIT=83cd8dc7a66ba02a0418a36ec45663b19e26dcb3 ;;
  v2.3.24) COMMIT=b6717d8af8111ee16a0ed188fde5524ee9ae27ef ;;
  *) echo "build-ledger.sh: unknown tag $TAG (v2.4.18 or v2.3.24)" >&2; exit 1 ;;
esac
SRC=$HERE/work/ledger-src
if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin https://github.com/formancehq/ledger.git
fi
git -C "$SRC" fetch -q --depth 1 origin "$COMMIT"
git -C "$SRC" -c advice.detachedHead=false checkout -q -f "$COMMIT"
(cd "$SRC" && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 \
   go build -trimpath -buildvcs=false -ldflags "-s -w" -o "$HERE/work/ledger-$TAG" .)
echo "work/ledger-$TAG"
