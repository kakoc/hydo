#!/usr/bin/env bash
# Builds hydo-agent as static, byte-reproducible Linux binaries, on Linux or on a Mac (Rust's
# bundled rust-lld and the musl targets' own crt objects and libc; no C toolchain):
#   hydo/crates/hydo-agent/build.sh [x86_64|aarch64|all] [OUTDIR]   (default all, <repo>/build)
# -> OUTDIR/hydo-agent (x86_64, for hvx guests) and OUTDIR/hydo-agent-arm64 (aarch64, for the Mac
# backend). The agent is inside guest images, so its bytes must not depend on the machine: M113's
# recipe (docs/BUILD-PINS.md, "The gRPC client"): rustc runs through hydo/grpcgen/rustc-stable
# (a host-independent `-C metadata`, the tree and $CARGO_HOME remapped to /hydo and /cargo), the
# crate's own Cargo.lock (--locked), and rust-lld's version line in .comment is set to
# "Linker: LLD" by hydo-elf-comment (the Linux and the Mac rust-lld name different build paths).
# Pinned: Rust 1.90.0 (rust-toolchain.toml) with both musl targets (docs/BUILD-PINS.md, "hydo-agent").
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../.." && pwd)"
WHICH="${1:-all}"
OUT="${2:-$TOP/build}"
CARGO="${CARGO:-cargo}"
CH="${CARGO_HOME:-$HOME/.cargo}"
case "$WHICH" in
  all) ARCHES="x86_64 aarch64";;
  x86_64|aarch64) ARCHES="$WHICH";;
  *) echo "usage: build.sh [x86_64|aarch64|all] [OUTDIR]" >&2; exit 2;;
esac
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
( cd "$TOP" && "$CARGO" build --release --locked -q -p hydo-tools --no-default-features --bin hydo-elf-comment )
sha() { if command -v sha256sum > /dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
for A in $ARCHES; do
  T="$A-unknown-linux-musl"
  NAME=hydo-agent; [ "$A" = aarch64 ] && NAME=hydo-agent-arm64
  ( cd "$HERE" && RUSTC_WRAPPER="$TOP/hydo/grpcgen/rustc-stable" HYDO_REMAP_TOP="$TOP" CARGO_HOME="$CH" \
      CARGO_ENCODED_RUSTFLAGS="-Ctarget-feature=+crt-static" "$CARGO" build --release --locked -q \
      --target "$T" --target-dir "$HERE/target/static" )
  install -m 0755 "$HERE/target/static/$T/release/hydo-agent" "$OUT/$NAME"
  "$TOP/target/release/hydo-elf-comment" "$OUT/$NAME" "Linker: LLD"
  echo "built $OUT/$NAME ($(wc -c < "$OUT/$NAME" | tr -d ' ') bytes, static $T, sha256 $(sha "$OUT/$NAME"))"
done
