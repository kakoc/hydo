#!/usr/bin/env bash
# Build the guest-AVX example (run from the repository root on the build machine):
#   hydo/onboard/examples/avx2/build.sh [hydo-build options]
# Compiles hydoavx.c with the pinned static musl gcc (docs/BUILD-PINS.md) and runs hydo-build.
# Output: build/explore-ob-avx2-disk.img
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
GCC_MUSL="${HYDO_GCC_MUSL:-/nix/store/aa1jsl19ywwfpqp0438vqbncc7sndnyw-gcc-static-x86_64-unknown-linux-musl-13.2.0/bin/gcc}"
MUSL_ROOT="${HYDO_MUSL_ROOT:-/nix/store/0nlqfwsn4qpx1453gb4zvx6dvn3121l4-musl-static-x86_64-unknown-linux-musl-1.2.6}"
[ -x "$GCC_MUSL" ] && [ -f "$MUSL_ROOT/lib/crt1.o" ] || { echo "build.sh: pinned static musl gcc missing" >&2; exit 2; }
"$GCC_MUSL" -static -O2 -Wall -pthread -B "$MUSL_ROOT/lib" "$HERE/hydoavx.c" -o "$HERE/hydoavx"
HB="${HYDO_BUILD_BIN:-$(command -v hydo-build || echo "$HERE/../../../../target/release/hydo-build")}"   # the Rust image builder
[ -x "$HB" ] || { echo "error: no hydo-build binary: cargo build --release -p hydo-build (or set HYDO_BUILD_BIN)" >&2; exit 1; }
HYDO_ONBOARD="$HERE/../.." "$HB" "$HERE/hydo.yaml" "$@"
