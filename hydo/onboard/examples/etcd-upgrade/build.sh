#!/usr/bin/env bash
# Build the etcd upgrade example (Milestone 238) with three etcd releases in one image (run from
# the repository root on a Linux machine):
#   hydo/onboard/examples/etcd-upgrade/build.sh [OLD NEW MID] [hydo-build options]
# The defaults are 3.5.5 (the version the cluster starts on), 3.6.0-rc.2 (the next version) and
# 3.5.26 (the 3.5 patch release etcd's upgrade notes ask for before 3.6). It downloads the
# official static release tarballs etcd-v<V>-linux-amd64.tar.gz from GitHub into $HYDO_ETCD_DL
# (default ~/hydo-etcd-dl/v<V>), checks each against the sha256 pinned below (taken from the
# release's SHA256SUMS), extracts etcd and etcdctl next to hydo.yaml and runs hydo-build with
# --set old=... --set new=... --set mid=...
# Output: build/explore-ob-etcd-upgrade-<OLD>-<NEW>-disk.img
set -euo pipefail
OLD="${1:-3.5.5}"; NEW="${2:-3.6.0-rc.2}"; MID="${3:-3.5.26}"
[ $# -ge 3 ] && shift 3 || shift $#
declare -A SHA=(
  [3.5.5]=7910a2fdb1863c80b885d06f6729043bff0540f2006bf6af34674df2636cb906
  [3.5.21]=adddda4b06718e68671ffabff2f8cee48488ba61ad82900e639d108f2148501c
  [3.5.26]=0a682a91201dc8351d507210bc30b021a11e254eab806f03224b51e8fad29abb
  [3.6.0-rc.2]=2e15ad2a0550734bdbabeaf377bb98d80887b915d064b51401adc4a826b7f36a
  [3.6.0]=42305b0dcbba7b6fdff0382d0c7b99c42026c88c44847a619ab58cde216725d9
)
HERE="$(cd "$(dirname "$0")" && pwd)"
for V in "$OLD" "$NEW" "$MID"; do
  [ -n "${SHA[$V]:-}" ] || { echo "build.sh: no pinned sha256 for etcd $V (known: ${!SHA[*]})" >&2; exit 2; }
  DL="${HYDO_ETCD_DL:-$HOME/hydo-etcd-dl/v$V}"
  T="etcd-v$V-linux-amd64.tar.gz"
  mkdir -p "$DL"
  [ -f "$DL/$T" ] || curl -sSfL -o "$DL/$T" "https://github.com/etcd-io/etcd/releases/download/v$V/$T"
  echo "${SHA[$V]}  $DL/$T" | sha256sum -c -
  X="$(mktemp -d)"
  tar -xzf "$DL/$T" -C "$X"
  mkdir -p "$HERE/etcd-v$V"
  install -m 0755 "$X/etcd-v$V-linux-amd64/etcd" "$X/etcd-v$V-linux-amd64/etcdctl" "$HERE/etcd-v$V/"
  rm -rf "$X"
done
HB="${HYDO_BUILD_BIN:-$(command -v hydo-build || echo "$HERE/../../../../target/release/hydo-build")}"
[ -x "$HB" ] || { echo "error: no hydo-build binary: cargo build --release -p hydo-build (or set HYDO_BUILD_BIN)" >&2; exit 1; }
HYDO_ONBOARD="$HERE/../.." "$HB" "$HERE/hydo.yaml" --set "old=$OLD" --set "new=$NEW" --set "mid=$MID" \
  --out "explore-ob-etcd-upgrade-$OLD-$NEW" "$@"
