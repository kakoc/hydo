#!/bin/sh
# Fetch the official restatedev/restate:1.7.13 image (linux/amd64) as a `docker save`-format
# archive next to hydo.yaml (hydo-build never pulls). Pinned by digest: the index of
# docker.io/restatedev/restate:1.7.13 lists this linux/amd64 manifest. Its restate-server is
# built with -Ctarget-cpu=broadwell (AVX2), so it needs guest.cpu: v3.
# Needs skopeo (nix build nixpkgs#skopeo) and network, once.
set -eu
cd "$(dirname "$0")"
nix_skopeo() {
  for p in $(nix build --no-link --print-out-paths nixpkgs#skopeo); do
    [ -x "$p/bin/skopeo" ] && { echo "$p/bin/skopeo"; return; }
  done
  echo "fetch-image.sh: nix build nixpkgs#skopeo gave no bin/skopeo" >&2; exit 1
}
SKOPEO=${SKOPEO:-$(command -v skopeo || nix_skopeo)}
POL=$(mktemp); trap 'rm -f "$POL"' EXIT
echo '{"default":[{"type":"insecureAcceptAnything"}]}' > "$POL"   # the digest pins the content
AMD64=sha256:d25e52b505f1b21c382282c9aa3339a79ff3eaab0c9b01c9516a1f349a895d07
[ -f restate-1.7.13-amd64.docker.tar ] || "$SKOPEO" --policy "$POL" copy \
  "docker://docker.io/restatedev/restate@$AMD64" docker-archive:restate-1.7.13-amd64.docker.tar:restatedev/restate:1.7.13
ls -l restate-1.7.13-amd64.docker.tar
