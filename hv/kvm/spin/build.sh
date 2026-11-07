#!/bin/sh
# Build the x86_64 spin-wait programs of the ledger-spin example (hv/kvm/spin/ledger-spin)
# with the pinned static musl gcc 13.2.0 and nixpkgs' binutils 2.41 (docs/BUILD-PINS.md):
#   GCC=<.../gcc-static-x86_64-unknown-linux-musl-13.2.0/bin/gcc> BINUTILS=<.../binutils-2.41/bin> \
#     sh hv/kvm/spin/build.sh [outdir]
# Default outdir: hv/kvm/spin/ledger-spin/bin. Prints each program's sha256.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-$here/ledger-spin/bin}
GCC=${GCC:-$HOME/hydo-gcroots/gcc-static-musl-13.2.0/bin/gcc}
[ -n "${BINUTILS:-}" ] && PATH=$BINUTILS:$PATH
mkdir -p "$out"
for m in "1 load" "5 count"; do
  set -- $m
  "$GCC" -static -nostdlib -fno-pie -no-pie -mgeneral-regs-only -fno-stack-protector -O2 \
    -fno-asynchronous-unwind-tables -Wl,--build-id=none \
    -DMODE="$1" -DNAME="\"$2\"" -o "$out/spin_$2" "$here/spinvar.c"
done
cd "$out" && sha256sum spin_load spin_count
