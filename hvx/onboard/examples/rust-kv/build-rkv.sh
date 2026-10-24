#!/usr/bin/env bash
# Build rkv as a static x86_64 musl binary (no C toolchain needed: Rust's self-contained musl
# crt/libc and rust-lld). Pinned toolchain: Rust 1.90.0 with the x86_64-unknown-linux-musl
# target (`rustup target add x86_64-unknown-linux-musl --toolchain 1.90.0`).
# Output: target/x86_64-unknown-linux-musl/release/rkv (and its sha256 on stdout).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
cd "$HERE"
export RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static --remap-path-prefix=$REPO=/hydo --remap-path-prefix=$HOME=/home"
cargo +1.90.0 build --release --locked --target x86_64-unknown-linux-musl
shasum -a 256 target/x86_64-unknown-linux-musl/release/rkv 2>/dev/null || sha256sum target/x86_64-unknown-linux-musl/release/rkv
