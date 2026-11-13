#!/bin/sh
# Build the KVM engine's guest kernel (kernel.nix) with the pinned nixpkgs and copy bzImage-kvm
# and its .config to $OUT. CHECK=1 then rebuilds it with `nix-build --check`, which fails unless
# the second build has the same bytes. SMP=1 builds the kernel for several vCPUs instead
# (kernel-smp.nix: bzImage-kvm-smp, docs/design/kvm-vcpus.md). Low priority on purpose: nice 19,
# $JOBS compile jobs.
#
#   [NIXPKGS=<nixpkgs>] [OUT=<dir>] [JOBS=8] [CHECK=1] [SMP=1] sh hv/kvm/linux/build-kernel.sh
#
# nix-build must be on PATH (on the bare-metal machine: . ~/hydo-nixb-work/nixenv.sh).
# Pins: docs/BUILD-PINS.md "The KVM engine's guest kernel".
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
NIXPKGS=${NIXPKGS:-/nix/store/f0ddmw6s86y567yg06h5019z72szbzch-source}
OUT=${OUT:-$PWD/out}
JOBS=${JOBS:-8}
RECIPE=kernel.nix
NAME=bzImage-kvm
if [ "${SMP:-0}" = 1 ]; then
  RECIPE=kernel-smp.nix
  NAME=bzImage-kvm-smp
fi
mkdir -p "$OUT"
build() {
  nice -n 19 nix-build --no-out-link --cores "$JOBS" --max-jobs 1 "$@" \
    "$HERE/$RECIPE" --arg nixpkgs "$NIXPKGS"
}
r=$(build)
if [ "${CHECK:-0}" = 1 ]; then
  build --check >/dev/null
  echo "nix-build --check: the second build has the same bytes"
fi
cp -f "$r/$NAME" "$r/$NAME.config" "$OUT/"
chmod 644 "$OUT/$NAME" "$OUT/$NAME.config"
echo "store path: $r"
cd "$OUT" && sha256sum "$NAME" "$NAME.config"
