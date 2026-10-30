#!/bin/sh
# Fetch the arm64 images of docker-compose.yml for hydo-mac.yaml (Milestone 99) into images/, by the
# digests of hydo-compose.lock.json: hydo-fetch-image (it comes with hydo; before M107
# hydo/onboard/fetch_image.py) checks every blob against its digest and writes a `docker save`
# archive (hydo-build reads it like the copies of fetch-images.sh; the image root depends only on
# the content). Needs network, once.
set -eu
cd "$(dirname "$0")"
F=${HYDO_FETCH_IMAGE:-$(command -v hydo-fetch-image || true)}
[ -n "$F" ] || { echo "fetch-images-mac.sh: hydo-fetch-image not found (put hydo's bin directory on PATH, or set HYDO_FETCH_IMAGE)" >&2; exit 1; }
mkdir -p images
f() { [ -f "images/$3-arm64.docker.tar" ] || "$F" "$1" "$2" "$4" "images/$3-arm64.docker.tar"; }
f docker.io/dockersamples/examplevotingapp_vote sha256:0ab82675962403d98735aa431ca116e8014051d5a8f145d302e04f21346f9b58 dockersamples-examplevotingapp_vote-latest dockersamples/examplevotingapp_vote:latest
f docker.io/dockersamples/examplevotingapp_result sha256:c87f44054a409c8a457b19089efbb9bb039b4539af62b2f4906bf6f05d380c31 dockersamples-examplevotingapp_result-latest dockersamples/examplevotingapp_result:latest
f docker.io/dockersamples/examplevotingapp_worker sha256:374f13009def417269e568e3796746605a4efc4e166e7ccda69312face04b963 dockersamples-examplevotingapp_worker-latest dockersamples/examplevotingapp_worker:latest
f docker.io/library/redis sha256:0819246a2bb1979f7745a8bae2559a2ffa49071ef7a5e7213b3e347e162aeb26 redis-alpine redis:alpine
f docker.io/library/postgres sha256:188e422ad801eaf156d9b9e276064d931553a81f4281a18db98752178dbc061d postgres-15-alpine postgres:15-alpine
ls -l images/*-arm64.docker.tar
