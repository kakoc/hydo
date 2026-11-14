#!/usr/bin/env bash
# Build pkv as a static musl binary (no C toolchain needed: Rust's self-contained musl crt/libc
# and rust-lld). Pinned toolchain: Rust 1.90.0 with the musl target.
#   build-pkv.sh            x86_64 (hvx and KVM)
#   build-pkv.sh aarch64    arm64 (the Mac engine)
# PKV_EXTRA_PERMILLE=<n> plants a slowdown of n thousandths of the server's work per PUT;
# PKV_SYNC_EACH_PUT=1 adds an fdatasync after every PUT; PKV_TAG=<text> changes the binary's
# bytes without changing its work (src/main.rs).
# Output: target/<arch>-unknown-linux-musl/release/pkv, copied to bin/pkv for hydo.yaml (and its
# sha256 on stdout).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
cd "$HERE"
T="${1:-x86_64}-unknown-linux-musl"
export RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static --remap-path-prefix=$REPO=/hydo --remap-path-prefix=$HOME=/home"
cargo +1.90.0 build --release --target "$T"
shasum -a 256 "target/$T/release/pkv" 2>/dev/null || sha256sum "target/$T/release/pkv"
mkdir -p bin && cp "target/$T/release/pkv" bin/pkv
