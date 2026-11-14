#!/usr/bin/env bash
# Build both ledger programs as static Linux binaries with their debug information, which
# function contracts read (docs/design/contracts.md):
#   build.sh            arm64 (the Mac engine)
#   build.sh x86_64     x86_64 (hvx and the KVM engine)
# Output: bin/ledger-rs (Rust 1.90.0 with the musl target) and bin/ledger-go (Go 1.27.3), and
# the sha256 of each on stdout.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
ARCH="${1:-aarch64}"
case "$ARCH" in
  aarch64) GOARCH=arm64 ;;
  x86_64) GOARCH=amd64 ;;
  *) echo "build.sh: aarch64 or x86_64" >&2; exit 2 ;;
esac
T="$ARCH-unknown-linux-musl"
mkdir -p "$HERE/bin"
cd "$HERE/rust"
RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static --remap-path-prefix=$HOME=/home --remap-path-prefix=$REPO=/hydo" \
  cargo +1.90.0 build --release --locked --target "$T"
cp "target/$T/release/ledger" "$HERE/bin/ledger-rs"
cd "$HERE/go"
CGO_ENABLED=0 GOOS=linux GOARCH=$GOARCH go build -trimpath -o "$HERE/bin/ledger-go" .
cd "$HERE"
shasum -a 256 bin/ledger-rs bin/ledger-go 2>/dev/null || sha256sum bin/ledger-rs bin/ledger-go
