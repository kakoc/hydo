#!/bin/sh
# Fetch the two images of Blnk's own docker-compose.yaml by digest into images/: Redis exactly as
# that file names it (redis:7.2.4), and PostgreSQL 16 (the file's postgres:16, pinned as
# 16.16-alpine3.24). hydo build never pulls. Needs hydo-fetch-image (it comes with hydo) and
# network access, once.
set -eu
cd "$(dirname "$0")"
F=${HYDO_FETCH_IMAGE:-$(command -v hydo-fetch-image || true)}
[ -n "$F" ] || { echo "fetch-images.sh: hydo-fetch-image not found (put hydo's bin directory on PATH, or set HYDO_FETCH_IMAGE)" >&2; exit 1; }
mkdir -p images
[ -e images/postgres-16-alpine-arm64-oci ] || "$F" docker.io/library/postgres sha256:40e58641410e030f5769096bca8372f68f243e1859c8ecdf11f227da390d514d postgres:16.16-alpine3.24 images/postgres-16-alpine-arm64-oci
[ -f images/postgres-16-alpine-amd64.docker.tar ] || "$F" docker.io/library/postgres sha256:a2f7e20374cd8fd8ed4a588b79549e10589bea7b1686e77922f5ac741b80b8a1 postgres:16.16-alpine3.24 images/postgres-16-alpine-amd64.docker.tar
[ -e images/redis-7.2.4-arm64-oci ] || "$F" docker.io/library/redis sha256:4da694ba727afad9a027c075a3e32356affea4db13482d489822107dcf09d4ed redis:7.2.4 images/redis-7.2.4-arm64-oci
[ -f images/redis-7.2.4-amd64.docker.tar ] || "$F" docker.io/library/redis sha256:9341b6548cc35b64a6de0085555264336e2f570e17ecff20190bf62222f2bd64 redis:7.2.4 images/redis-7.2.4-amd64.docker.tar
ls -ld images/postgres-16-alpine-arm64-oci images/postgres-16-alpine-amd64.docker.tar images/redis-7.2.4-arm64-oci images/redis-7.2.4-amd64.docker.tar
