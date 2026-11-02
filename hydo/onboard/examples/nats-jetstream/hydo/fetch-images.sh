#!/bin/sh
# Copy the NATS images by digest into hydo/images/ (network, once). The digests were resolved by
# `hydo compose` (hydo-compose.lock.json for 2.10.22; 2.10.23 and 2.12.1 resolved the same way
# with NATS_VERSION set in an --env-file). The manifest picks one with `--set nats=<version>`.
set -eu
cd "$(dirname "$0")"
F=${HYDO_FETCH_IMAGE:-$(command -v hydo-fetch-image || true)}
[ -n "$F" ] || { echo "fetch-images.sh: hydo-fetch-image not found (put hydo's bin directory on PATH, or set HYDO_FETCH_IMAGE)" >&2; exit 1; }
mkdir -p images
get() {   # get <version> <arm64 manifest digest> <amd64 manifest digest>
  [ -e "images/nats-$1-alpine-arm64-oci" ] || "$F" docker.io/library/nats "sha256:$2" "nats:$1-alpine" "images/nats-$1-alpine-arm64-oci"
  [ -f "images/nats-$1-alpine-amd64.docker.tar" ] || "$F" docker.io/library/nats "sha256:$3" "nats:$1-alpine" "images/nats-$1-alpine-amd64.docker.tar"
}
# Jepsen's crash finding (nats-server issue 6888): lost on 2.10.22, fixed in 2.10.23
get 2.10.22 a7f055eca1521c7f5a89ba5f3dcbff015d57b7a929f8653829c4e6f84164ff2b aa536352f09b109b909e8bfbf9859a40601481bb3742ebc7a09cfaf638622407
get 2.10.23 44905153cfe19be67e2baf248d1fe4a687aa2542e28efbff0f08f466e8749d96 1fd834581735b304465d440d706f668b245fa15c90864a8110d0069628504799
# the release Jepsen analysed (the lazy-fsync finding, issue 7564)
get 2.12.1 d373f17e7959b06cc3f258b3ae749b62e9ed66ac9202a34b790976b80ab25931 dd116e748f85fa29d488724b478936b78ffcf01968ad819662d9d994fd6ee8f8
ls -ld images/*
