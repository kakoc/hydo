#!/usr/bin/env bash
# Builds the static guest client hydo-grpc (hydo/crates/hydo-grpc) of this tree:
#   hydo/grpcgen/build-client.sh [--out DIR] [amd64] [arm64]      (default: amd64; DIR default
#                                                                  target/grpc-client/out)
# amd64 -> DIR/hydo-grpc (x86_64-unknown-linux-musl), arm64 -> DIR/hydo-grpc.arm64
# (aarch64-unknown-linux-musl): static, linked by rust-lld (.cargo/config.toml), so any host with
# the pinned toolchain (rust-toolchain.toml) and the two musl targets builds the same bytes. The
# client is inside guest images, so its bytes must not depend on the machine: the tree and
# $CARGO_HOME are remapped to /hydo and /cargo in the binary (panic locations), the profile strips
# symbols, and the build uses its own target directory. docs/BUILD-PINS.md, "The gRPC client".
set -euo pipefail
TOP="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$TOP/target/grpc-client/out"
archs=()
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2;;
    --out=*) OUT="${1#*=}"; shift;;
    amd64|arm64) archs+=("$1"); shift;;
    *) echo "build-client.sh: unknown argument $1 (amd64, arm64, --out DIR)" >&2; exit 2;;
  esac
done
[ ${#archs[@]} -gt 0 ] || archs=(amd64)
CARGO="${CARGO:-cargo}"
CH="${CARGO_HOME:-$HOME/.cargo}"
mkdir -p "$OUT"
for a in "${archs[@]}"; do
  case "$a" in
    amd64) triple=x86_64-unknown-linux-musl; name=hydo-grpc;;
    arm64) triple=aarch64-unknown-linux-musl; name=hydo-grpc.arm64;;
  esac
  # one flag per \x1f-separated word, so paths with blanks stay whole
  flags="-Ctarget-feature=+crt-static"$'\x1f'"--remap-path-prefix=$TOP=/hydo"$'\x1f'"--remap-path-prefix=$CH=/cargo"
  ( cd "$TOP" && CARGO_ENCODED_RUSTFLAGS="$flags" "$CARGO" build --release --locked -q -p hydo-grpc --bin hydo-grpc \
      --target "$triple" --target-dir "$TOP/target/grpc-client" )
  install -m 0755 "$TOP/target/grpc-client/$triple/release/hydo-grpc" "$OUT/$name"
  if command -v sha256sum > /dev/null; then sha256sum "$OUT/$name"; else shasum -a 256 "$OUT/$name"; fi
done
