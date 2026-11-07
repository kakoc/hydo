#!/bin/sh
# Copy the registry images by digest (linux/amd64) into images/, with hydo-fetch-image (comes with
# hydo). The shop's own image is built by ../app/build-app.sh. Run from this directory; needs the
# network once. (Written by hand: `hydo compose` stopped at the shop's local image, and with
# --no-resolve it pinned nothing; the digests are those `hydo compose` resolves without the shop.)
set -e
cd "$(dirname "$0")"
F=${HYDO_FETCH_IMAGE:-$(command -v hydo-fetch-image || true)}
[ -n "$F" ] || { echo "fetch-images.sh: hydo-fetch-image not found (put hydo's bin directory on PATH, or set HYDO_FETCH_IMAGE)" >&2; exit 1; }
mkdir -p images
# postgres:15-alpine = docker.io/library/postgres@sha256:aae6554de426ae1990f1a939ff183a0299146cf7142a6f4c379c3357af9cc2d8
[ -f images/postgres-15-alpine-amd64.docker.tar ] || "$F" docker.io/library/postgres sha256:d3ab4e7f0ca6e34e8135c95defd881673f7edddfacf63231eeb89caf6db2c1c9 postgres:15-alpine images/postgres-15-alpine-amd64.docker.tar
# redis:7-alpine = docker.io/library/redis@sha256:c5d34a98396a4775e223c74e929162c35c5a2182901ba26b50ade939941ce14b
[ -f images/redis-7-alpine-amd64.docker.tar ] || "$F" docker.io/library/redis sha256:ec1c2d3603c3d7d65b153bedda0248b641aa3bd7684503406b27b9e870ac0796 redis:7-alpine images/redis-7-alpine-amd64.docker.tar
ls -l images/
