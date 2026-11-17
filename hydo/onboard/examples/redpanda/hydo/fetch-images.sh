#!/bin/sh
# Copy the Redpanda images by digest into hydo/images/ (network, once). The digests are the
# platform manifests of redpandadata/redpanda:<version> (docker buildx imagetools inspect). The
# manifest picks one with `--set redpanda=<version>`.
set -eu
cd "$(dirname "$0")"
F=${HYDO_FETCH_IMAGE:-$(command -v hydo-fetch-image || true)}
[ -n "$F" ] || { echo "fetch-images.sh: hydo-fetch-image not found (put hydo's bin directory on PATH, or set HYDO_FETCH_IMAGE)" >&2; exit 1; }
mkdir -p images
get() {   # get <version> <arm64 manifest digest> <amd64 manifest digest>
  [ -f "images/redpanda-$1-arm64.docker.tar" ] || "$F" docker.io/redpandadata/redpanda "sha256:$2" "redpandadata/redpanda:$1" "images/redpanda-$1-arm64.docker.tar"
  [ -f "images/redpanda-$1-amd64.docker.tar" ] || "$F" docker.io/redpandadata/redpanda "sha256:$3" "redpandadata/redpanda:$1" "images/redpanda-$1-amd64.docker.tar"
}
# the latest release (2026-11-12)
get v26.2.7 7f28a0155a74329279d1c21feff0ce6d0527b0436216559bcb34f68cc66a3f3b d5cc5fa94b1caf1cee182f4ab6305d73e7ba89ad58ed806bb31f7ae0ae038442
ls -ld images/*
