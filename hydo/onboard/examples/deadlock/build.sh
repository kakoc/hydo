#!/usr/bin/env bash
# Build the example's two programs as static x86_64 Linux binaries (hvx), with what `hydo
# inspect stacks` reads to name their frames: the Go program keeps its DWARF and its table of
# functions, the Rust program its line tables and call frame information.
#   build.sh go     the Go program only (bin/bank-go; Go 1.27.3, from any host)
#   build.sh rust   the Rust program only (bin/bank-rs; Rust 1.90.0 with the musl target)
#   build.sh        both
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
mkdir -p "$HERE/bin"
what="${1:-all}"
if [ "$what" = all ] || [ "$what" = go ]; then
  cd "$HERE/go"
  CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -trimpath -buildvcs=false -o "$HERE/bin/bank-go" .
fi
if [ "$what" = all ] || [ "$what" = rust ]; then
  cd "$HERE/rust"
  export RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static -C relocation-model=static --remap-path-prefix=$HOME=/home --remap-path-prefix=$REPO=/hydo"
  cargo +1.90.0 build --release --locked --target x86_64-unknown-linux-musl
  cp target/x86_64-unknown-linux-musl/release/bank "$HERE/bin/bank-rs"
fi
cd "$HERE"
ls bin/bank-* | xargs shasum -a 256 2>/dev/null || ls bin/bank-* | xargs sha256sum
