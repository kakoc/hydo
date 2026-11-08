#!/bin/sh
# Fetch the PostgreSQL image (the voting app's database, ../../compose-voting) by digest into
# images/ (hydo-build never pulls). Needs hydo-fetch-image (it comes with hydo) and network, once.
set -eu
cd "$(dirname "$0")"
F=${HYDO_FETCH_IMAGE:-$(command -v hydo-fetch-image || true)}
[ -n "$F" ] || { echo "fetch-images.sh: hydo-fetch-image not found (put hydo's bin directory on PATH, or set HYDO_FETCH_IMAGE)" >&2; exit 1; }
mkdir -p images
case "$(uname -m)" in
  arm64|aarch64) [ -e images/postgres-15-alpine-arm64-oci ] || "$F" docker.io/library/postgres sha256:188e422ad801eaf156d9b9e276064d931553a81f4281a18db98752178dbc061d postgres:15-alpine images/postgres-15-alpine-arm64-oci ;;
  *) [ -f images/postgres-15-alpine-amd64.docker.tar ] || "$F" docker.io/library/postgres sha256:1d4031d6ed0a9b774b365f99a3c16127217eb19f699d8d43fc43b732748601ef postgres:15-alpine images/postgres-15-alpine-amd64.docker.tar ;;
esac
ls -ld images/*
