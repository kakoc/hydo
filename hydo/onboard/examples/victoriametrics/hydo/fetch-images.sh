#!/bin/sh
# Fetch the images of the VictoriaMetrics cluster by digest into images/ (hydo-build never pulls).
# Started by hydo compose from ../docker-compose.yaml; the versions before v1.156.0 are for
# `hydo versions` (RESULTS.md, finding 1). Needs hydo-fetch-image (it comes with hydo) and network
# access, once. It fetches the images this machine builds: x86_64 on Linux, arm64 on a Mac. Name
# x86_64, arm64 or all as the first argument to choose (sh fetch-images.sh all), and a version as
# the second to fetch only that one. The digests are the platform manifests of
# victoriametrics/<component>:<version>-cluster on Docker Hub.
set -eu
cd "$(dirname "$0")"
A=${1:-}
[ -n "$A" ] || case "$(uname -s)" in Darwin) A=arm64;; *) A=x86_64;; esac
case "$A" in x86_64|arm64|all) ;; *) echo "fetch-images.sh: x86_64, arm64 or all (got $A)" >&2; exit 2;; esac
ONLY=${2:-}
F=${HYDO_FETCH_IMAGE:-$(command -v hydo-fetch-image || true)}
[ -n "$F" ] || { echo "fetch-images.sh: hydo-fetch-image not found (put hydo's bin directory on PATH, or set HYDO_FETCH_IMAGE)" >&2; exit 1; }
mkdir -p images
one() {   # one <component> <version> <amd64 digest> <arm64 digest>
  n=victoriametrics-$1-$2-cluster
  [ "$A" = x86_64 ] || [ -e "images/$n-arm64-oci" ] || "$F" "docker.io/victoriametrics/$1" "sha256:$4" "victoriametrics/$1:$2-cluster" "images/$n-arm64-oci"
  [ "$A" = arm64 ] || [ -f "images/$n-amd64.docker.tar" ] || "$F" "docker.io/victoriametrics/$1" "sha256:$3" "victoriametrics/$1:$2-cluster" "images/$n-amd64.docker.tar"
}
want() { [ -z "$ONLY" ] || [ "$ONLY" = "$1" ]; }
# the newest release (2026-11-09), the one hydo.yaml runs
if want v1.156.0; then
  one vmstorage v1.156.0 0d20231d5a0ddad0eef081bfb7b1ecea21a5b0a5e9c256a52e8c3c66e6fb4132 ad337ea96e9e7fbc8097ba380525bce0d5107ba4db921e0a4df01fe90bee4a1e
  one vminsert  v1.156.0 81989baab297c69d39c9f580683940747a2fb079fa9098749bfebd358edc6bdb 222063ac2c0493597567c41c586167177b8ccd57f112c8711e6bb72b7a7faa21
  one vmselect  v1.156.0 8c7b1015b72b90328cf24cac262e5644760029f0214ac7e34898f9033f5fd0b7 d2df9d90443fbdc66f4011e46dda9f3c2476b345fc52431f7a4d0ecf40079fad
fi
# the first release with -futureRetention
if want v1.141.0; then
  one vmstorage v1.141.0 a744e84280c293d31c88550a875af190c1d4334014b7310c540b129aff3b6bde 2ffaf55c61a604d7a96b7139baea7007f243e9aa81633b93ab8ec192a996001f
  one vminsert  v1.141.0 fbc3cd53386e5f50af45470771c119df759b581377dedc7989c6e8a763081a5d 64c701b780d5e2ce011239850e5495702c6bd2e46b7cfbe890c3fa9a70ef449f
  one vmselect  v1.141.0 67c0104c0d137f51d83fccb4c63e752bef19c8b0501718908db3a5a02b0a9556 d96384e2b85fe66d0170e7131dedc60433bce1ed43007dd84487076618eeebfe
fi
# the release before it
if want v1.140.0; then
  one vmstorage v1.140.0 9ff4aa910857e391d5b4180726fbaf8993e5de59afbc285460f6d05d3415fd1a 15e19a8d56c6fb933af4d166f2b82161e544720fe28c8d78acecd88a7078d198
  one vminsert  v1.140.0 ed512602b91270daeb4d0c1d890f11203f9199dafb74ada38cc2a36ab4ffb351 1cf460eff9fcb79ad702ea5b72debdddf8ccd8ca53ee21997ec3d91172f42930
  one vmselect  v1.140.0 3814e6b4735c7fa26dd8b29994f49d323a5d60c2f8f50d3eab53a1179e89d58c 3fad3340e49a6449dca6142241e6906a837064aaf02a2fe1b5880e14dd8b6556
fi
ls -ld images/*
