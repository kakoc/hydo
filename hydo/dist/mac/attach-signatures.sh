#!/bin/sh
# Attach the published Developer ID signatures to an unsigned Mac release build, giving the
# published (signed) tarball byte for byte (docs/design/signing.md, Milestone 172):
#
#     hydo/dist/mac/attach-signatures.sh <unsigned hydo-mac-<v>.tar.gz> <hydo-mac-<v>-signatures.tar.gz> <OUTDIR>
#         -> OUTDIR/hydo-mac-<v>/ (the signed release tree) and OUTDIR/hydo-mac-<v>.tar.gz
#
# The unsigned tarball is what hydo/dist/mac/build-release.sh builds from the release's commit (two
# builds give the same bytes); the signatures asset is published with the release. For each
# program the asset names, the unsigned program must have the sha256 the signature was made for,
# and the program with the signature attached must have the signed program's sha256
# (hydo-release signatures attach, hydo/crates/hydo-release/src/signatures.rs); the tree is then
# packed as build-release.sh packs it (pack-release.sh), so the result has the published
# tarball's sha256. Needs only this tree's Rust toolchain (hydo-release is built here, or taken from
# $HYDO_RELEASE_BIN); no key, no network.
set -e
umask 022
[ $# -eq 3 ] || { echo "usage: $0 <unsigned hydo-mac-<v>.tar.gz> <hydo-mac-<v>-signatures.tar.gz> <OUTDIR>" >&2; exit 2; }
D=$(cd "$(dirname "$0")" && pwd)
TOP=$(cd "$D/../../.." && pwd)
U=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
S=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
mkdir -p "$3"
OUT=$(cd "$3" && pwd)
NAME=$(basename "$U" .tar.gz)
case "$NAME" in hydo-mac-*) ;; *) echo "$U is not a hydo-mac-<version>.tar.gz" >&2; exit 2 ;; esac
[ "$(basename "$S")" = "$NAME-signatures.tar.gz" ] || echo "note: $(basename "$S") is not named $NAME-signatures.tar.gz" >&2
if [ -n "$HYDO_RELEASE_BIN" ]; then
  HR=$HYDO_RELEASE_BIN
else
  ( cd "$TOP" && cargo build --release --locked -q -p hydo-release ) >&2
  HR=$TOP/target/release/hydo-release
fi
[ -d "$OUT/$NAME" ] && chmod -R u+w "$OUT/$NAME" && rm -rf "$OUT/$NAME"
tar -xzf "$U" -C "$OUT"
"$HR" signatures attach "$OUT/$NAME" "$S"
sh "$D/pack-release.sh" "$OUT/$NAME"
