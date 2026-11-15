#!/usr/bin/env bash
# Build the example's programs as static Linux binaries with their debug information, which
# function contracts read (docs/design/contracts.md):
#   build.sh            arm64 (the Mac engine)
#   build.sh x86_64     x86_64 (hvx and the KVM engine)
# Output: bin/ledger-rs (Rust 1.90.0 with the musl target) and bin/ledger-go (Go 1.27.3), the
# account servers; bin/types-rs and bin/types-go, the programs of the types the contracts read
# (types/, hydo-types.yaml); and the sha256 of each on stdout.
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
export RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static --remap-path-prefix=$HOME=/home --remap-path-prefix=$REPO=/hydo"
cd "$HERE/rust"
cargo +1.90.0 build --release --locked --target "$T"
cp "target/$T/release/ledger" "$HERE/bin/ledger-rs"
cd "$HERE/types/rust"
cargo +1.90.0 build --release --locked --target "$T"
cp "target/$T/release/types" "$HERE/bin/types-rs"
cd "$HERE/go"
CGO_ENABLED=0 GOOS=linux GOARCH=$GOARCH go build -trimpath -buildvcs=false -o "$HERE/bin/ledger-go" .
cd "$HERE/types/go"
CGO_ENABLED=0 GOOS=linux GOARCH=$GOARCH go build -trimpath -buildvcs=false -o "$HERE/bin/types-go" .
cd "$HERE"
shasum -a 256 bin/ledger-rs bin/ledger-go bin/types-rs bin/types-go 2>/dev/null \
  || sha256sum bin/ledger-rs bin/ledger-go bin/types-rs bin/types-go
