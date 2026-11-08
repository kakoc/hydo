#!/usr/bin/env bash
# Build hydo-sql (the ready-made SQL workloads and checks) as static,
# byte-reproducible Linux binaries, on Linux or on a Mac (Rust's bundled lld and the musl
# targets' own crt objects and libc; no C toolchain):
#   hydo/crates/hydo-sql/build.sh [x86_64|aarch64|all] [OUTDIR]   (default all, <repo>/build)
# -> OUTDIR/hydo-sql (x86_64: hvx and KVM guests) and OUTDIR/hydo-sql-arm64 (aarch64, the Mac),
# the names hydo-build looks for (HYDO_SQL_BIN, HYDO_SQL_BIN_ARM64).
# Pinned: Rust 1.90.0 with both musl targets (docs/BUILD-PINS.md, "hydo-sql"), the crate's own
# Cargo.lock (--locked). The bytes depend neither on the machine nor on where the tree or the
# crate cache lives (since 0.3.0, the gRPC client's recipe, docs/BUILD-PINS.md "The gRPC client"):
# rustc runs through hydo/grpcgen/rustc-stable, which replaces Cargo's `-C metadata` (it hashes
# the host's kind and the path of the path dependency hydo-read, so another machine or another
# checkout path gave other bytes) and remaps the tree and $CARGO_HOME to /hydo and /cargo, and the
# linker's version line is blanked (rust-lld's differs between the Linux and the Mac toolchain).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../.." && pwd)"
WHICH="${1:-all}"
OUT="${2:-$TOP/build}"
CH="${CARGO_HOME:-$HOME/.cargo}"
mkdir -p "$OUT"
case "$WHICH" in
  all) ARCHES="x86_64 aarch64";;
  x86_64|aarch64) ARCHES="$WHICH";;
  *) echo "usage: build.sh [x86_64|aarch64|all] [OUTDIR]" >&2; exit 2;;
esac
sha() { shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1 || sha256sum "$1" | cut -d' ' -f1; }
( cd "$TOP" && cargo build --release --locked -q -p hydo-tools --no-default-features --bin hydo-elf-comment )
for A in $ARCHES; do
  T="$A-unknown-linux-musl"
  RUSTC_WRAPPER="$TOP/hydo/grpcgen/rustc-stable" HYDO_REMAP_TOP="$TOP" CARGO_HOME="$CH" \
    RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static" \
    cargo +1.90.0 build --release --locked --quiet --target "$T" --manifest-path "$HERE/Cargo.toml"
  NAME=hydo-sql; [ "$A" = aarch64 ] && NAME=hydo-sql-arm64
  cp "$HERE/target/$T/release/hydo-sql" "$OUT/$NAME"
  "$TOP/target/release/hydo-elf-comment" "$OUT/$NAME" "Linker: LLD"
  echo "built $OUT/$NAME ($(wc -c < "$OUT/$NAME" | tr -d ' ') bytes, static $T, sha256 $(sha "$OUT/$NAME" | cut -c1-16))"
done
