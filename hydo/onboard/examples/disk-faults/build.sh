#!/usr/bin/env bash
# Build recs as a static musl binary (no C toolchain needed: Rust's self-contained musl crt/libc
# and rust-lld). Pinned toolchain: Rust 1.90.0 with the musl target.
#   build.sh            aarch64 (the Mac engine)
#   build.sh x86_64     x86_64 (KVM)
# Output: target/<arch>-unknown-linux-musl/release/recs, copied to bin/recs for hydo.yaml (and
# its sha256 on stdout).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
cd "$HERE"
T="${1:-aarch64}-unknown-linux-musl"
export RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static --remap-path-prefix=$REPO=/hydo --remap-path-prefix=$HOME=/home"
cargo +1.90.0 build --release --target "$T"
shasum -a 256 "target/$T/release/recs" 2>/dev/null || sha256sum "target/$T/release/recs"
mkdir -p bin && cp "target/$T/release/recs" bin/recs
