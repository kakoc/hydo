#!/usr/bin/env bash
# Build hydo-world (the fake outside services and fault proxies inside the guest, MANIFEST.md
# "Fake outside services") as one static musl binary with the workspace's pinned Rust toolchain
# (rust-toolchain.toml, docs/BUILD-PINS.md), linked by rust-lld, so any machine with rustup and
# the two musl targets builds it (a Mac too). Byte-reproducible: --locked, the release profile
# (stripped), the checkout and Cargo home remapped out of the binary, and the linker's
# .comment dropped, so the bytes do not depend on where the tree or the crate cache lives, or on
# whether a Mac or a Linux machine built it. hydo-build stages <build dir>/hydo-world (or
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
# Cargo is told one host for every machine (rustc-host.sh says why)
HYDO_REAL_RUSTC="$(cd "$TOP" && rustup which rustc 2>/dev/null || command -v rustc)"
export HYDO_REAL_RUSTC
export RUSTC="$HERE/rustc-host.sh"
( cd "$TOP" && cargo build --release --locked -p hydo-world --target "$TARGET" \
    --target-dir "$TOP/target/hydo-world" ) >&2
mkdir -p "$(dirname "$OUT")"
# rust-lld writes its own identification into the unloaded .comment section, and the Mac's and
# Linux's rust-lld of the same LLVM commit name their source differently there: drop the section
# with the toolchain's own objcopy, so a Mac and a Linux machine give the same bytes
SYSROOT="$("$HYDO_REAL_RUSTC" --print sysroot)"
HOST="$("$HYDO_REAL_RUSTC" -vV | sed -n 's/^host: //p')"
# (on Linux it loads the toolchain's libLLVM, as when rustc itself runs it)
LD_LIBRARY_PATH="$SYSROOT/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  "$SYSROOT/lib/rustlib/$HOST/bin/rust-objcopy" --remove-section=.comment \
  "$TOP/target/hydo-world/$TARGET/release/hydo-world" "$OUT"
if command -v sha256sum > /dev/null; then SHA="$(sha256sum "$OUT" | cut -c1-16)"; else SHA="$(shasum -a 256 "$OUT" | cut -c1-16)"; fi
echo "built $OUT ($(wc -c < "$OUT" | tr -d ' ') bytes, static Rust $TARGET, sha256 $SHA)"
