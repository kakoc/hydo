#!/bin/sh
# Build rsv (service/: the Restate deployment on restate-sdk 0.13.0 and the test client) as static
# musl programs: bin/rsv-amd64 (hvx and KVM) and bin/rsv-arm64 (the Mac engine). The Restate
# server itself comes from its official image (hydo/fetch-images.sh).
# Needs Rust 1.90.0 with the targets x86_64-unknown-linux-musl and aarch64-unknown-linux-musl
# (rustup target add ...); no C toolchain: Rust's self-contained musl and rust-lld.
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
PATH="$HOME/.cargo/bin:$PATH"
mkdir -p bin
export RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static --remap-path-prefix=$HERE=/rsv --remap-path-prefix=$HOME=/home"
for t in x86_64 aarch64; do
  (cd service && cargo +1.90.0 build --release --locked --target $t-unknown-linux-musl)
done
cp service/target/x86_64-unknown-linux-musl/release/rsv bin/rsv-amd64
cp service/target/aarch64-unknown-linux-musl/release/rsv bin/rsv-arm64
ls -l bin/rsv-amd64 bin/rsv-arm64
