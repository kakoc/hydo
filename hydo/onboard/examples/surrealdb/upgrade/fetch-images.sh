#!/bin/sh
# Fetch the SurrealDB 3.3.5 image by digest into images/, and the 4.0.1 image into ../hydo/images/
# (../hydo/fetch-images.sh). Needs hydo-fetch-image (it comes with hydo) and network access, once.
# x86_64 by default; arm64 or all as the argument.
set -eu
cd "$(dirname "$0")"
A=${1:-x86_64}
case "$A" in x86_64|arm64|all) ;; *) echo "fetch-images.sh: x86_64, arm64 or all (got $A)" >&2; exit 2;; esac
F=${HYDO_FETCH_IMAGE:-$(command -v hydo-fetch-image || true)}
[ -n "$F" ] || { echo "fetch-images.sh: hydo-fetch-image not found (put hydo's bin directory on PATH, or set HYDO_FETCH_IMAGE)" >&2; exit 1; }
sh ../hydo/fetch-images.sh "$A"
mkdir -p images
[ "$A" = arm64 ] || [ -f images/surrealdb-surrealdb-v3.3.5-amd64.docker.tar ] || "$F" docker.io/surrealdb/surrealdb sha256:bbba754d9bea5cf7ef46d6dfb45031b4600e1ee4369985d103131a3006c19678 surrealdb/surrealdb:v3.3.5 images/surrealdb-surrealdb-v3.3.5-amd64.docker.tar
[ "$A" = x86_64 ] || [ -e images/surrealdb-surrealdb-v3.3.5-arm64-oci ] || "$F" docker.io/surrealdb/surrealdb sha256:aa055fca8ce56aa91cf07393dcd7b85cf9b099aff4d713bbfcbebdf49f576570 surrealdb/surrealdb:v3.3.5 images/surrealdb-surrealdb-v3.3.5-arm64-oci
