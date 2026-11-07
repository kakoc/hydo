#!/bin/sh
# Build the KVM engine's guest kernel (kernel.nix) with the pinned nixpkgs and copy bzImage-kvm
# and its .config to $OUT. CHECK=1 then rebuilds it with `nix-build --check`, which fails unless
# the second build has the same bytes. Low priority on purpose: nice 19, $JOBS compile jobs.
#
#   [NIXPKGS=<nixpkgs>] [OUT=<dir>] [JOBS=8] [CHECK=1] sh hv/kvm/linux/build-kernel.sh
#
# nix-build must be on PATH (on the bare-metal machine: . ~/hydo-nixb-work/nixenv.sh).
# Pins: docs/BUILD-PINS.md "The KVM engine's guest kernel".
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
NIXPKGS=${NIXPKGS:-/nix/store/f0ddmw6s86y567yg06h5019z72szbzch-source}
OUT=${OUT:-$PWD/out}
JOBS=${JOBS:-8}
mkdir -p "$OUT"
build() {
  nice -n 19 nix-build --no-out-link --cores "$JOBS" --max-jobs 1 "$@" \
    "$HERE/kernel.nix" --arg nixpkgs "$NIXPKGS"
}
r=$(build)
if [ "${CHECK:-0}" = 1 ]; then
  build --check >/dev/null
  echo "nix-build --check: the second build has the same bytes"
fi
cp -f "$r/bzImage-kvm" "$r/bzImage-kvm.config" "$OUT/"
chmod 644 "$OUT/bzImage-kvm" "$OUT/bzImage-kvm.config"
echo "store path: $r"
cd "$OUT" && sha256sum bzImage-kvm bzImage-kvm.config
