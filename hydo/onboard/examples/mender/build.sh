#!/bin/sh
# Build what the device image needs into out/ (git-ignored), with Docker:
#   out/<arch>/bin/          mender-update 5.2.0, mender-flash 1.1.0, fw_printenv and fw_setenv
#                            (libubootenv 0.3.8), busybox, loopdio and mdb_dump, static, and
#                            Mender's rootfs-image Update Module (build/Dockerfile)
#   out/<arch>/images/       the slot images v1-v5, their hashes, and the data partition
#   out/<arch>/artifacts/    the Mender Artifacts v2-v5 (build/make-images.sh)
#   out/common/              U-Boot's default environment
#   out/modules/             empty here; fetch-modules.sh fills it on Linux with the hvx guest
#                            kernel's loop module
# <arch> is aarch64 and x86_64. Works on a Mac with Docker (the amd64 tools are built under
# emulation) and on Linux with Docker. The sources are pinned by sha256 in build/Dockerfile, the
# base image by digest; every file time in the images and Artifacts is fixed, so two builds give
# the same bytes (sha256sum out/*/images/* out/*/artifacts/*). Under emulation they need not:
# a compiler that crashes in one of CMake's checks turns a feature of libarchive off
# (RESULTS.md, "Building the same bytes").
#   sh build.sh            both architectures
#   sh build.sh arm64      one: arm64 (the Mac engine) or amd64 (hvx and KVM)
set -eu
cd "$(dirname "$0")"
mkdir -p out/modules
native=arm64; [ "$(uname -m)" = x86_64 ] && native=amd64
for arch in ${1:-arm64 amd64}; do
  case $arch in arm64) m=aarch64 ;; amd64) m=x86_64 ;; esac
  # the other architecture's image has no mender-artifact (the stage tools, build/Dockerfile)
  target=; [ "$arch" = "$native" ] || target="--target tools"
  docker build -q --platform "linux/$arch" $target -t "hydo-mender-tools:$arch" build/ > /dev/null
  id=$(docker create --platform "linux/$arch" "hydo-mender-tools:$arch")
  rm -rf "out/$m" && mkdir -p "out/$m" && docker cp "$id:/out" "out/$m/bin" && docker rm "$id" > /dev/null
done
# the images and Artifacts carry each architecture's programs; mender-artifact runs natively
docker build -q --platform "linux/$native" -t "hydo-mender-tools:$native" build/ > /dev/null
docker run --rm --platform "linux/$native" -v "$PWD/out:/work" -v "$PWD/build:/build:ro" \
  "hydo-mender-tools:$native" sh /build/make-images.sh /work
