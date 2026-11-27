#!/bin/sh
# Build the KVM engine's guest kernel (kernel.nix) with the pinned nixpkgs and copy bzImage-kvm
# and its .config to $OUT. CHECK=1 then rebuilds it with `nix-build --check`, which fails unless
# the second build has the same bytes. SMP=1 builds the kernel for several vCPUs instead
# (kernel-smp.nix: bzImage-kvm-smp, docs/design/kvm-vcpus.md). CGROUPS=1 builds the kernel of
# images built at the fingerprint epoch 3 and later, with cgroups (kernel-cg.nix: bzImage-kvm-cg,
# or bzImage-kvm-smp-cg with SMP=1; docs/design/guest-cgroups.md). CONTAINERS=1 builds the variant
# `containers` that an image asks for with `guest.kernel: containers` (kernel-containers.nix:
# bzImage-kvm-containers, or bzImage-kvm-smp-containers with SMP=1; docs/design/k3s-in-guest.md).
# PROCIO=1 builds the kernels of images built at the fingerprint epoch 7 and later, with
# /proc/<pid>/io (kernel-procio.nix, Milestone 376): bzImage-kvm-io, or with SMP=1
# bzImage-kvm-smp-io, and with CONTAINERS=1 bzImage-kvm-containers-io or
# bzImage-kvm-smp-containers-io.
# Low priority on purpose: nice 19, $JOBS compile jobs.
#
#   [NIXPKGS=<nixpkgs>] [OUT=<dir>] [JOBS=8] [CHECK=1] [SMP=1] [CGROUPS=1 | CONTAINERS=1] [PROCIO=1] sh hv/kvm/linux/build-kernel.sh
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
ARGS=
if [ "${SMP:-0}" = 1 ]; then
  RECIPE=kernel-smp.nix
  NAME=bzImage-kvm-smp
fi
if [ "${CGROUPS:-0}" = 1 ]; then
  RECIPE=kernel-cg.nix
  NAME=$NAME-cg
  if [ "${SMP:-0}" = 1 ]; then ARGS="--arg smp true"; fi
fi
if [ "${CONTAINERS:-0}" = 1 ]; then
  RECIPE=kernel-containers.nix
  NAME=${NAME%-cg}-containers
  if [ "${SMP:-0}" = 1 ]; then ARGS="--arg smp true"; fi
fi
if [ "${PROCIO:-0}" = 1 ]; then
  RECIPE=kernel-procio.nix
  NAME=bzImage-kvm
  ARGS=
  if [ "${SMP:-0}" = 1 ]; then NAME=$NAME-smp; ARGS="--arg smp true"; fi
  if [ "${CONTAINERS:-0}" = 1 ]; then NAME=$NAME-containers; ARGS="$ARGS --arg containers true"; fi
  NAME=$NAME-io
fi
mkdir -p "$OUT"
build() {
  nice -n 19 nix-build --no-out-link --cores "$JOBS" --max-jobs 1 "$@" \
    "$HERE/$RECIPE" --arg nixpkgs "$NIXPKGS" $ARGS
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
