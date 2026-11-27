#!/usr/bin/env bash
# Builds hydo-clock (one service's own wall clock, Milestone 372) as a static x86_64 Linux binary,
# on Linux or on a Mac, with the recipe of hydo-agent's build.sh (Rust's bundled rust-lld, the musl
# target's own crt objects and libc, a host-independent `-C metadata` through
# hydo/grpcgen/rustc-stable, the crate's own Cargo.lock, and "Linker: LLD" in .comment):
#   hydo/crates/hydo-clock/build.sh [OUTDIR]   (default <repo>/build)
# -> OUTDIR/hydo-clock, which hydo-build stages as /bin/hydo-clock for a service of `clock: own`
# (HYDO_CLOCKBIN names another file), and OUTDIR/hydo-clock-words, the build with the cargo
# feature `words` (Milestone 379: the traced process's memory one word at a time where the kernel
# has no process_vm_readv), which it stages instead for an image of `guest.kernel: containers`
# (HYDO_CLOCKBIN_WORDS). Rust 1.90.0 with the x86_64 musl target.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../.." && pwd)"
OUT="${1:-$TOP/build}"
CARGO="${CARGO:-cargo}"
CH="${CARGO_HOME:-$HOME/.cargo}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
( cd "$TOP" && "$CARGO" build --release --locked -q -p hydo-tools --no-default-features --bin hydo-elf-comment )
sha() { if command -v sha256sum > /dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
T=x86_64-unknown-linux-musl
# one build per feature set, each in a target folder of its own (the default build's is
# Milestone 372's)
one() {
  local name=$1 dir=$2; shift 2
  ( cd "$HERE" && RUSTC_WRAPPER="$TOP/hydo/grpcgen/rustc-stable" HYDO_REMAP_TOP="$TOP" CARGO_HOME="$CH" \
      CARGO_ENCODED_RUSTFLAGS="-Ctarget-feature=+crt-static" "$CARGO" build --release --locked -q \
      --target "$T" --target-dir "$HERE/target/$dir" "$@" )
  install -m 0755 "$HERE/target/$dir/$T/release/hydo-clock" "$OUT/$name"
  "$TOP/target/release/hydo-elf-comment" "$OUT/$name" "Linker: LLD"
  echo "built $OUT/$name ($(wc -c < "$OUT/$name" | tr -d ' ') bytes, static $T, sha256 $(sha "$OUT/$name"))"
}
one hydo-clock static
one hydo-clock-words static-words --features words
