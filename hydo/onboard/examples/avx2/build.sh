#!/usr/bin/env bash
# Build the guest-AVX example (run from the repository root on the build box):
#   hvx/onboard/examples/avx2/build.sh [hydo-build options]
# Compiles hydoavx.c with the pinned static musl gcc (hvx/BUILD-PINS.md) and runs hydo-build.
# Output: build/explore-ob-avx2-disk.img
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
GCC_MUSL="${HYDO_GCC_MUSL:-/nix/store/aa1jsl19ywwfpqp0438vqbncc7sndnyw-gcc-static-x86_64-unknown-linux-musl-13.2.0/bin/gcc}"
MUSL_ROOT="${HYDO_MUSL_ROOT:-/nix/store/0nlqfwsn4qpx1453gb4zvx6dvn3121l4-musl-static-x86_64-unknown-linux-musl-1.2.6}"
[ -x "$GCC_MUSL" ] && [ -f "$MUSL_ROOT/lib/crt1.o" ] || { echo "build.sh: pinned static musl gcc missing" >&2; exit 2; }
"$GCC_MUSL" -static -O2 -Wall -pthread -B "$MUSL_ROOT/lib" "$HERE/hydoavx.c" -o "$HERE/hydoavx"
PY3="${HYDO_PY3:-$(command -v python3 || ls /nix/store/*python3-3.11*/bin/python3 | head -1)}"
"$PY3" "$HERE/../../hydo-build" "$HERE/hydo.yaml" "$@"
