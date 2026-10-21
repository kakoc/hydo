#!/bin/sh
# Fetch the official redis:7.2.4 image as files next to hydo.yaml (hydo-build never pulls).
# Pinned by digest: the multi-platform index of docker.io/library/redis:7.2.4 is
#   sha256:5a93f6b2e391b78e8bd3f9e7e1e1e06aeb5295043b4703fb88392835cec924a0
# and these are its linux/amd64 and linux/arm64/v8 manifests.
# Needs skopeo (nix: nix build nixpkgs#skopeo); with Docker instead:
#   docker pull redis@sha256:9341b654...; docker save -o redis-7.2.4-amd64.docker.tar redis@sha256:9341b654...
set -eu
cd "$(dirname "$0")"
# `nix build` prints one store path per output (skopeo-1.15.1 and skopeo-1.15.1-man): take the
# one that has bin/skopeo
nix_skopeo() {
  for p in $(nix build --no-link --print-out-paths nixpkgs#skopeo); do
    [ -x "$p/bin/skopeo" ] && { echo "$p/bin/skopeo"; return; }
  done
  echo "fetch-image.sh: nix build nixpkgs#skopeo gave no bin/skopeo" >&2; exit 1
}
SKOPEO=${SKOPEO:-$(command -v skopeo || nix_skopeo)}
POL=$(mktemp); trap 'rm -f "$POL"' EXIT
echo '{"default":[{"type":"insecureAcceptAnything"}]}' > "$POL"   # the digest pins the content
R=docker://docker.io/library/redis
AMD64=sha256:9341b6548cc35b64a6de0085555264336e2f570e17ecff20190bf62222f2bd64
ARM64=sha256:4da694ba727afad9a027c075a3e32356affea4db13482d489822107dcf09d4ed
[ -f redis-7.2.4-amd64.docker.tar ] || "$SKOPEO" --policy "$POL" copy "$R@$AMD64" docker-archive:redis-7.2.4-amd64.docker.tar:redis:7.2.4
[ -d redis-7.2.4-arm64-oci ] || "$SKOPEO" --policy "$POL" copy "$R@$ARM64" oci:redis-7.2.4-arm64-oci:redis:7.2.4
ls -ld redis-7.2.4-amd64.docker.tar redis-7.2.4-arm64-oci
