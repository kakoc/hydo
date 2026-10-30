#!/usr/bin/env bash
# Rebuild the inputs of the Redis example image with the REAL pinned tools (no HYDO_GCC_MUSL /
# HYDO_REDIS_OUT overrides) in a private directory, then build the example images and print
# their sha256. See docs/BUILD-PINS.md.
#
# Usage on the build box:  PROOF=~/hydo-pins-proof hv/hvx/images/pins-proof.sh   (run from the tree root)
# The tree is expected at $PROOF/hydo; HOME is set to $PROOF so the manifests' ~/hydo/build/...
# sources resolve inside the proof directory, never to the real ~/hydo/build.
set -euo pipefail
PROOF="${PROOF:?set PROOF}"
cd "$PROOF/hydo"
unset HYDO_GCC_MUSL HYDO_MUSL_ROOT HYDO_REDIS_OUT HYDO_SEEDRNG
mkdir -p build

# The exact code of build-linux.sh, not a copy: the gcc/musl resolution block, the redis block
# and the hvx_seedrng compile line are cut out of the script by their line ranges.
blk() { sed -n "$1p" build-linux.sh; }
grep -q 'GCC_MUSL="${HYDO_GCC_MUSL:-}"' <(blk 225,225) || { echo "line ranges moved" >&2; exit 1; }
eval "$(blk 225,238)"          # GCC_MUSL / MUSL_ROOT (pinned paths, no overrides)
eval "$(blk 519,535)"          # REDIS_OUT -> build/redis-server, build/redis-cli
eval "$(blk 559,559)"          # build/hvx_seedrng
"$GCC_MUSL" -static -O2 -Wall -B "${MUSL_ROOT%/}/lib" hv/hvx/images/hydokv.c  -o build/hydokv
"$GCC_MUSL" -static -O2 -Wall -B "${MUSL_ROOT%/}/lib" hv/hvx/images/hydoapp.c -o build/hydoapp
echo "GCC_MUSL=$GCC_MUSL"
echo "REDIS_OUT=$REDIS_OUT"
sha256sum build/hvx_seedrng build/hydokv build/hydoapp build/redis-server build/redis-cli

HB="${HYDO_BUILD_BIN:-$(command -v hydo-build || echo "$PROOF/hydo/target/release/hydo-build")}"   # the Rust image builder
[ -x "$HB" ] || { echo "error: no hydo-build binary: cargo build --release -p hydo-build (or set HYDO_BUILD_BIN)" >&2; exit 1; }
HOME="$PROOF" HYDO_ONBOARD="$PROOF/hydo/hydo/onboard" "$HB" hydo/onboard/examples/redis/hydo.yaml --out explore-ob-redis
sha256sum build/explore-ob-redis-disk.img

if [ "${STACK:-1}" = 1 ]; then
  # examples/stack (FRONT=fix): the front door is a prebuilt Go binary (an input, not a pin).
  # The image needs ~6 GB; STACK_BDIR puts it on a disk with room (the root fs had ~6 GB free).
  SB="${STACK_BDIR:-$PROOF/hydo/build}"
  mkdir -p "$SB"
  [ "$SB" = "$PROOF/hydo/build" ] || cp build/hvx_seedrng "$SB/hvx_seedrng"
  cp "${FRONT_BIN:-/home/user/hydo/build/explore-fix-hydostack}" build/explore-fix-hydostack
  HOME="$PROOF" HYDO_BUILD_BIN="$HB" bash hydo/onboard/examples/stack/build.sh --build-dir "$SB"
  sha256sum "$SB/explore-ob-stack-fix-disk.img"
fi
