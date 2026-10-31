#!/usr/bin/env bash
# Build hydo-world (the fake outside services and fault proxies inside the guest, MANIFEST.md
# "Fake outside services") as one static musl binary with the workspace's pinned Rust toolchain
# (rust-toolchain.toml, docs/BUILD-PINS.md), linked by rust-lld, so any machine with rustup and
# the two musl targets builds it (a Mac too). Byte-reproducible: --locked, the release profile
# (stripped), and the checkout and Cargo home remapped out of the binary, so the bytes do not
# depend on where the tree or the crate cache lives. hydo-build stages <build dir>/hydo-world (or
# $HYDO_WORLDBIN) for manifests with fakes: or proxies:.
#   hydo/crates/hydo-world/build.sh [out]     (default: build/hydo-world, from the repository root)
# HYDO_WORLD_ARCH=arm64 builds the arm64 binary for hydo-build --arch arm64, which stages
# <build dir>/hydo-world-arm64 (or $HYDO_WORLDBIN_ARM64); default amd64.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../.." && pwd)"
OUT="${1:-build/hydo-world}"
ARCH="${HYDO_WORLD_ARCH:-amd64}"
case "$ARCH" in
  amd64 | x86_64) TARGET=x86_64-unknown-linux-musl ;;
  arm64 | aarch64) TARGET=aarch64-unknown-linux-musl ;;
  *) echo "error: HYDO_WORLD_ARCH=$ARCH: amd64 or arm64" >&2; exit 1 ;;
esac
CH="${CARGO_HOME:-$HOME/.cargo}"
CH="$(cd "$CH" && pwd)"
# RUSTFLAGS replaces the target's rustflags of .cargo/config.toml, so +crt-static is repeated here
export RUSTFLAGS="-C target-feature=+crt-static --remap-path-prefix=$TOP=/hydo --remap-path-prefix=$CH=/cargo"
( cd "$TOP" && cargo build --release --locked -p hydo-world --target "$TARGET" \
    --target-dir "$TOP/target/hydo-world" ) >&2
mkdir -p "$(dirname "$OUT")"
cp "$TOP/target/hydo-world/$TARGET/release/hydo-world" "$OUT"
if command -v sha256sum > /dev/null; then SHA="$(sha256sum "$OUT" | cut -c1-16)"; else SHA="$(shasum -a 256 "$OUT" | cut -c1-16)"; fi
echo "built $OUT ($(wc -c < "$OUT" | tr -d ' ') bytes, static Rust $TARGET, sha256 $SHA)"
