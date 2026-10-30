#!/usr/bin/env bash
# Build rkv as a static musl binary (no C toolchain needed: Rust's self-contained musl crt/libc
# and rust-lld). Pinned toolchain: Rust 1.90.0 with the musl target
# (`rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl --toolchain 1.90.0`).
#   build-rkv.sh            x86_64 (hvx, the bare-metal backend)
#   build-rkv.sh aarch64    arm64 (the Mac backend)
# Output: target/<arch>-unknown-linux-musl/release/rkv, copied to bin/rkv for hydo.yaml (and
# its sha256 on stdout).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
cd "$HERE"
T="${1:-x86_64}-unknown-linux-musl"
export RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static --remap-path-prefix=$REPO=/hydo --remap-path-prefix=$HOME=/home"
cargo +1.90.0 build --release --locked --target $T
shasum -a 256 target/$T/release/rkv 2>/dev/null || sha256sum target/$T/release/rkv
mkdir -p bin && cp target/$T/release/rkv bin/rkv
