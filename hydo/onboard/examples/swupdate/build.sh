#!/bin/sh
# Build what the device image needs into out/ (git-ignored), with Docker:
#   out/bin-aarch64/, out/bin-x86_64/   swupdate 2026.05.1, fw_printenv and fw_setenv
#                                       (libubootenv 0.3.8) and loopdio, static
#                                       (build/Dockerfile, build/swupdate.config)
#   out/images/, out/bundles/           the OS images v1-v5 and the SWUpdate images v2-v5
#                                       (build/make-swu.sh)
#   out/modules/                        empty here; fetch-modules.sh fills it on Linux with the
#                                       hvx guest kernel's loop module
# Works on a Mac with Docker (the amd64 tools are built under emulation) and on Linux with
# Docker. The sources are pinned by sha256 in build/Dockerfile, the base image by digest.
set -eu
cd "$(dirname "$0")"
mkdir -p out/modules
for arch in arm64 amd64; do
  case $arch in arm64) m=aarch64 ;; amd64) m=x86_64 ;; esac
  docker build -q --platform "linux/$arch" -t "hydo-swupdate-tools:$arch" build/ > /dev/null
  id=$(docker create --platform "linux/$arch" "hydo-swupdate-tools:$arch")
  rm -rf "out/bin-$m" && docker cp "$id:/out" "out/bin-$m" && docker rm "$id" > /dev/null
done
# the images do not depend on the architecture: build them once, with the native image
native=arm64; [ "$(uname -m)" = x86_64 ] && native=amd64
docker run --rm --platform "linux/$native" -v "$PWD/out:/work" -v "$PWD/build:/build:ro" \
  "hydo-swupdate-tools:$native" sh /build/make-swu.sh /work
ls -l out/bin-aarch64 out/bin-x86_64
