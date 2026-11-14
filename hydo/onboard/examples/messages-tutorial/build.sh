#!/bin/sh
# Build tinyq for the guest of this machine's hydo: linux/arm64 on a Mac, linux/amd64 on Linux.
# It writes hydo/bin/tinyq, the file hydo/hydo.yaml copies into the image. Needs Go 1.22 or later.
#     sh build.sh            then: hydo build hydo/hydo.yaml
# The program is static (CGO_ENABLED=0) and built without paths or a build id, so one Go version
# gives the same bytes on every machine.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
case $(uname -s) in
  Darwin) ARCH=arm64 ;;
  *) ARCH=amd64 ;;
esac
mkdir -p "$HERE/hydo/bin"
cd "$HERE/tinyq"
CGO_ENABLED=0 GOOS=linux GOARCH=$ARCH GOFLAGS=-mod=mod \
  go build -trimpath -buildvcs=false -ldflags='-s -w -buildid=' -o "$HERE/hydo/bin/tinyq" .
echo "built hydo/bin/tinyq for linux/$ARCH with $(go version | cut -d' ' -f3)"
