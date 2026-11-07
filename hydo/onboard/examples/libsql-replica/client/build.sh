#!/bin/sh
# Build sqlgen as a static x86_64 musl binary (Rust 1.90.0 with the x86_64-unknown-linux-musl
# target; rust-lld, no C toolchain). Output: client/bin/sqlgen and its sha256.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../../../.." && pwd)"
cd "$HERE"
T=x86_64-unknown-linux-musl
export RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static --remap-path-prefix=$REPO=/hydo --remap-path-prefix=$HOME=/home"
cargo +1.90.0 build --release --locked --target $T
mkdir -p bin && cp target/$T/release/sqlgen bin/sqlgen
sha256sum bin/sqlgen 2>/dev/null || shasum -a 256 bin/sqlgen
