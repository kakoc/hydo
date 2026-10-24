#!/usr/bin/env bash
# Build the etcd example for one etcd release (run from the repository root on the build box):
#   hvx/onboard/examples/etcd/build.sh [VERSION]      # default 3.6.15
# Downloads the official static release tarball etcd-v<VERSION>-linux-amd64.tar.gz from GitHub
# into $HYDO_ETCD_DL (default ~/hydo-etcd-dl/v<VERSION>), checks it against the sha256 pinned
# below (taken from the release's SHA256SUMS), extracts etcd + etcdctl next to hydo.yaml and
# runs hydo-build with --set version=<VERSION>. hydo-build takes hvx_seedrng from the build
# directory (or HYDO_SEEDRNG); build it with the pinned static gcc + musl (hvx/BUILD-PINS.md,
# build-linux.sh), sha256 c2a440c5..., not with a stand-in.
# Output: build/explore-ob-etcd-<VERSION>-disk.img
# HYDO_ETCD_MANIFEST=hydo-hist.yaml builds that manifest instead (pass --out for a new name):
#   HYDO_ETCD_MANIFEST=hydo-hist.yaml hvx/onboard/examples/etcd/build.sh 3.5.5 --out explore-ob-etcd-hist-3.5.5
set -euo pipefail
V="${1:-3.6.15}"
declare -A SHA=(
  [3.6.15]=b51d86e6bff2168de5ba8cd631c0fe6c785c7a1b32b083079f5677d6b7e2772c
  [3.6.9]=633136f13fcadac52e5c0ddcb97912643af6fcb9cb362e75774d0f96f7666396
  [3.6.8]=cf9cfe91a4856cb90eed9c99e6aee4b708db2c7888b88a6f116281f04b0ea693
  [3.5.6]=4db32e3bc06dd0999e2171f76a87c1cffed8369475ec7aa7abee9023635670fb
  [3.5.5]=7910a2fdb1863c80b885d06f6729043bff0540f2006bf6af34674df2636cb906
)
[ -n "${SHA[$V]:-}" ] || { echo "build.sh: no pinned sha256 for etcd $V (known: ${!SHA[*]})" >&2; exit 2; }
HERE="$(cd "$(dirname "$0")" && pwd)"
DL="${HYDO_ETCD_DL:-$HOME/hydo-etcd-dl/v$V}"
T="etcd-v$V-linux-amd64.tar.gz"
mkdir -p "$DL"
[ -f "$DL/$T" ] || curl -sSfL -o "$DL/$T" "https://github.com/etcd-io/etcd/releases/download/v$V/$T"
echo "${SHA[$V]}  $DL/$T" | sha256sum -c -
X="$(mktemp -d)"; trap 'rm -rf "$X"' EXIT
tar -xzf "$DL/$T" -C "$X"
mkdir -p "$HERE/etcd-v$V"
install -m 0755 "$X/etcd-v$V-linux-amd64/etcd" "$X/etcd-v$V-linux-amd64/etcdctl" "$HERE/etcd-v$V/"
PY3="${HYDO_PY3:-$(command -v python3 || ls /nix/store/*python3-3.11*/bin/python3 | head -1)}"
"$PY3" "$HERE/../../hydo-build" "$HERE/${HYDO_ETCD_MANIFEST:-hydo.yaml}" --set "version=$V" --out "explore-ob-etcd-$V" "${@:2}"
