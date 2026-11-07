#!/usr/bin/env bash
# Build hydo-gui (the voter at the screen, compose-voting-browser) as static, byte-reproducible
# Linux binaries, on Linux or on a Mac (Rust's bundled lld and the musl targets' own crt objects
# and libc; no C toolchain), with hydo-read's recipe (docs/BUILD-PINS.md, "hydo-gui"):
#   gui-driver/build.sh [x86_64|aarch64|all] [OUTDIR]   (default x86_64, ../build)
# -> OUTDIR/hydo-gui (x86_64) and OUTDIR/hydo-gui-arm64 (aarch64).
# Pinned: Rust 1.90.0 with the musl targets, the crate's own Cargo.lock (--locked, no
# dependencies); the checkout path and $CARGO_HOME are remapped, so the bytes do not depend on
# where the tree or the crate cache lives.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../../../.." && pwd)"
WHICH="${1:-x86_64}"
OUT="${2:-$HERE/../build}"
CH="${CARGO_HOME:-$HOME/.cargo}"
mkdir -p "$OUT"
case "$WHICH" in
  all) ARCHES="x86_64 aarch64";;
  x86_64|aarch64) ARCHES="$WHICH";;
  *) echo "usage: build.sh [x86_64|aarch64|all] [OUTDIR]" >&2; exit 2;;
esac
sha() { shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1 || sha256sum "$1" | cut -d' ' -f1; }
for A in $ARCHES; do
  T="$A-unknown-linux-musl"
  RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static --remap-path-prefix=$TOP=/hydo --remap-path-prefix=$CH=/cargo" \
    cargo +1.90.0 build --release --locked --quiet --target "$T" --manifest-path "$HERE/Cargo.toml" \
      --target-dir "$HERE/target"
  NAME=hydo-gui; [ "$A" = aarch64 ] && NAME=hydo-gui-arm64
  cp "$HERE/target/$T/release/hydo-gui" "$OUT/$NAME"
  echo "built $OUT/$NAME ($(wc -c < "$OUT/$NAME" | tr -d ' ') bytes, static $T, sha256 $(sha "$OUT/$NAME" | cut -c1-16))"
done
