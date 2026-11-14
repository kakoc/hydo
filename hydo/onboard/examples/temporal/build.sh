#!/bin/sh
# Build tw (the worker, the client and the checks) for the guest of this machine's hydo:
# linux/arm64 on a Mac, linux/amd64 on Linux. It writes hydo/bin/tw, which hydo/hydo.yaml copies
# into the image and docker-compose.yaml mounts into the worker. Needs Go 1.26 or later.
#     sh build.sh            then: hydo build hydo/hydo.yaml
# The program is static (CGO_ENABLED=0) and built without paths or a build id, so one Go version
# gives the same bytes on every machine.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
case $(uname -s) in
  Darwin) ARCH=arm64 ;;
  *) ARCH=amd64 ;;
esac
ARCH=${GOARCH:-$ARCH}
mkdir -p "$HERE/hydo/bin"
cd "$HERE/tw"
CGO_ENABLED=0 GOOS=linux GOARCH=$ARCH \
  go build -trimpath -buildvcs=false -ldflags='-s -w -buildid=' -o "$HERE/hydo/bin/tw" .
echo "built hydo/bin/tw for linux/$ARCH with $(go version | cut -d' ' -f3)"
