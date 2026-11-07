#!/bin/sh
# Fetch every input of compose-voting-browser that is not in the repository (Milestone 166), each
# pinned by sha256, into images/ and cdn/ (git-ignored). Needs network, once.
#   sh fetch-inputs.sh [amd64|arm64]     (default amd64)
# - the five images of Docker's voting app, by the digests of ../compose-voting (hydo-compose.lock.json);
# - headless Chromium: chromedp/headless-shell 156.0.8078.48 (index sha256:3da74b09…; Debian 12
#   slim with Chromium's headless shell and the DejaVu fonts), by its per-platform manifest digest;
# - the two scripts the vote page loads from public CDNs (the guest has no internet; the cdn
#   service serves them under the CDNs' names): jQuery 1.11.1 (code.jquery.com's
#   jquery-latest.min.js, frozen since 2014) and jquery-cookie 1.4.1 (cdnjs). Both MIT.
# hydo-fetch-image (it comes with hydo) checks every blob against its digest; curl's downloads are
# checked against the sha256 below. Then build the driver: gui-driver/build.sh (Rust 1.90.0).
set -eu
cd "$(dirname "$0")"
ARCH=${1:-amd64}
F=${HYDO_FETCH_IMAGE:-$(command -v hydo-fetch-image || true)}
[ -n "$F" ] || { echo "fetch-inputs.sh: hydo-fetch-image not found (put hydo's bin directory on PATH, or set HYDO_FETCH_IMAGE)" >&2; exit 1; }
mkdir -p images cdn/ajax/libs/jquery-cookie/1.4.1
f() {   # <repository> <amd64 digest> <arm64 digest> <file stem> <tag>
  if [ "$ARCH" = arm64 ]; then d=$3; else d=$2; fi
  [ -f "images/$4-$ARCH.docker.tar" ] || "$F" "$1" "$d" "$5" "images/$4-$ARCH.docker.tar"
}
f docker.io/dockersamples/examplevotingapp_vote sha256:e63c2cb6732d6e5026ec0c3ede7bd3052321068b91d30fa16561e0f334d807b8 sha256:0ab82675962403d98735aa431ca116e8014051d5a8f145d302e04f21346f9b58 dockersamples-examplevotingapp_vote-latest dockersamples/examplevotingapp_vote:latest
f docker.io/dockersamples/examplevotingapp_result sha256:7583fe9ceb85e658335e66d8d1f01c43b2bcdf9684f60e53f3989d6fc36a3bd0 sha256:c87f44054a409c8a457b19089efbb9bb039b4539af62b2f4906bf6f05d380c31 dockersamples-examplevotingapp_result-latest dockersamples/examplevotingapp_result:latest
f docker.io/dockersamples/examplevotingapp_worker sha256:8820615213ade73a83f930a90fffe4aa6c01865c2f277f2c9506e8a904a8ecbe sha256:374f13009def417269e568e3796746605a4efc4e166e7ccda69312face04b963 dockersamples-examplevotingapp_worker-latest dockersamples/examplevotingapp_worker:latest
f docker.io/library/redis sha256:16b124a2d3eacd620d917ead6fd26cbf1f1d7117fa94122398971931cfe2d403 sha256:0819246a2bb1979f7745a8bae2559a2ffa49071ef7a5e7213b3e347e162aeb26 redis-alpine redis:alpine
f docker.io/library/postgres sha256:1d4031d6ed0a9b774b365f99a3c16127217eb19f699d8d43fc43b732748601ef sha256:188e422ad801eaf156d9b9e276064d931553a81f4281a18db98752178dbc061d postgres-15-alpine postgres:15-alpine
f docker.io/chromedp/headless-shell sha256:21eb1d28a091ed52a382359a35a82c5233af7387d9e822206f6b1abef4bc5d3e sha256:3aa177f0d221ae85bc483407d2f4f2644a148066029d314f4c5ca99db1f39d8b chromedp-headless-shell-156.0.8078.48 chromedp/headless-shell:156.0.8078.48
sha() { shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1 || sha256sum "$1" | cut -d' ' -f1; }
get() {   # <url> <file> <sha256>
  [ -f "$2" ] && [ "$(sha "$2")" = "$3" ] && return 0
  curl -sSfL -o "$2.part" "$1"
  [ "$(sha "$2.part")" = "$3" ] || { echo "fetch-inputs.sh: $1 is not sha256 $3" >&2; rm -f "$2.part"; exit 1; }
  mv "$2.part" "$2"
}
get http://code.jquery.com/jquery-latest.min.js cdn/jquery-latest.min.js 540bc6dec1dd4b92ea4d3fb903f69eabf6d919afd48f4e312b163c28cff0f441
get https://cdnjs.cloudflare.com/ajax/libs/jquery-cookie/1.4.1/jquery.cookie.js cdn/ajax/libs/jquery-cookie/1.4.1/jquery.cookie.js b84161c9fbf7520cd14e7019f92120bd87a928a074156e91a992eba9fc9436e8
ls -l images/*-"$ARCH".docker.tar cdn/jquery-latest.min.js cdn/ajax/libs/jquery-cookie/1.4.1/jquery.cookie.js
