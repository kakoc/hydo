#!/bin/sh
# Build the shop's backend image (Medusa 2.18.0 from medusa-starter-default at a pinned commit,
# with this directory's medusa-config.ts and seed script) and save it where hydo.yaml expects it:
#   sh app/build-app.sh            -> hydo/images/medusa-shop-amd64.docker.tar
# Needs Docker (rootless is fine) and the network (git, the npm registry). The image is then pinned
# by its id (`image_ref:` in hydo/hydo.yaml): a rebuild resolves the dependencies again and gives
# other bytes, so after a rebuild update image_ref.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
EX=$(cd "$HERE/.." && pwd)
STARTER_COMMIT=9565d9d5487e470137c63824fe58c59a2098bde8   # medusa-starter-default, Medusa 2.18.0
TAG=${MEDUSA_SHOP_TAG:-medusa-shop:hydo}
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
git clone -q https://github.com/medusajs/medusa-starter-default "$W/starter"
git -C "$W/starter" checkout -q "$STARTER_COMMIT"
rm -rf "$W/starter/.git"
cp -R "$HERE/Dockerfile" "$HERE/medusa-config.ts" "$HERE/src" "$W/"
docker build --platform linux/amd64 -t "$TAG" "$W"
mkdir -p "$EX/hydo/images"
docker save -o "$EX/hydo/images/medusa-shop-amd64.docker.tar" "$TAG"
echo "image id: $(docker image inspect --format '{{.Id}}' "$TAG")"
