#!/bin/sh
# Build the two test programs this example runs and the C library they load, for linux/amd64:
#   bin/collection-buggy.test  Qdrant at QDRANT_COMMIT, the commit of issue 10200's last report
#   bin/collection-fixed.test  the same commit with pull request 11028's one commit applied
#   bin/lib/                   ld-linux-x86-64.so.2, libc.so.6, libm.so.6, libgcc_s.so.1 from
#                              Ubuntu 24.04's packages, by sha256 (the programs are dynamic)
# Each program is `cargo test -p collection --lib --no-run` in the dev profile, as Qdrant's CI
# builds it, with the build paths remapped and the debug information stripped (542 MB to
# 286 MB; the code and the symbols are unchanged).
# Needs a Linux x86_64 host with git, curl, ar, objcopy, Rust 1.99.0 (rustup), protoc 34.1 (the
# version Qdrant's CI pins) and the C compiler the pins were made with (Ubuntu 24.04's gcc 13.3.0,
# for aws-lc-sys and zstd-sys); another compiler gives other bytes and the check below says so.
#   PROTOC=<protoc 34.1> sh build-test.sh
#   QDRANT_SRC=<dir> sh build-test.sh      # reuse a clone
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
QDRANT_COMMIT=222b2e44ab1c3f17e8246396ead6558e45041df7   # master, 2026-10-21, issue 10200's report
FIX_COMMIT=b5ce700e0b502203f62942c2635624bf1c595130      # refs/pull/11028/head (one commit)
RUST=1.99.0                                               # stable on 2026-10-21
SNAP=https://snapshot.ubuntu.com/ubuntu/20261101T000000Z/pool/main
LIBC_DEB=$SNAP/g/glibc/libc6_2.39-0ubuntu8.9_amd64.deb
LIBC_DEB_SHA=ff5557d99b51f761c4b7c92368b9cc45565eda17df9bf9eb4b134d09825008be
LIBGCC_DEB=$SNAP/g/gcc-14/libgcc-s1_14.2.0-4ubuntu2~24.04.1_amd64.deb
LIBGCC_DEB_SHA=aa7fadbe33b78bcf99885318040601c550c208929565b179891d9a3cc2aa68cd
SRC=${QDRANT_SRC:-$HERE/work/qdrant}
WORK=$(dirname "$SRC")
: "${PROTOC:?set PROTOC to protoc 34.1}"
export PROTOC
nolfs="-c filter.lfs.process= -c filter.lfs.smudge=cat -c filter.lfs.required=false"
if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin https://github.com/qdrant/qdrant.git
fi
GIT_LFS_SKIP_SMUDGE=1 git -C "$SRC" $nolfs fetch -q --depth 2 origin "$QDRANT_COMMIT" "$FIX_COMMIT"
mkdir -p bin/lib
CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
build() {   # output name; the checkout is prepared by the caller
  json=$WORK/build-$1.json
  ( cd "$SRC" && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$WORK/target" \
      RUSTFLAGS="--remap-path-prefix=$WORK/target=/target --remap-path-prefix=$SRC=/qdrant --remap-path-prefix=$CARGO_HOME=/cargo" \
      CFLAGS="-ffile-prefix-map=$WORK/target=/target -ffile-prefix-map=$SRC=/qdrant -ffile-prefix-map=$CARGO_HOME=/cargo" \
      cargo +$RUST test -p collection --lib --no-run --locked --target x86_64-unknown-linux-gnu \
      --message-format=json-render-diagnostics > "$json" )
  exe=$(grep -o '"executable":"[^"]*collection-[^"]*"' "$json" | tail -1 | cut -d'"' -f4)
  objcopy --strip-debug "$exe" "bin/$1"
}
git -C "$SRC" $nolfs -c advice.detachedHead=false checkout -q -f "$QDRANT_COMMIT"
build collection-buggy.test
git -C "$SRC" diff "$FIX_COMMIT~1" "$FIX_COMMIT" | git -C "$SRC" apply
build collection-fixed.test
git -C "$SRC" $nolfs checkout -q -f "$QDRANT_COMMIT"
fetch_deb() {   # url sha256 member...
  url=$1; sha=$2; shift 2
  d=$WORK/deb.$$; rm -rf "$d"; mkdir -p "$d"
  curl -fsSL --retry 3 -o "$d/p.deb" "$url"
  got=$( (sha256sum "$d/p.deb" 2>/dev/null || shasum -a 256 "$d/p.deb") | cut -d' ' -f1)
  [ "$got" = "$sha" ] || { echo "$url: sha256 $got, expected $sha" >&2; exit 1; }
  ( cd "$d" && ar x p.deb && tar xf data.tar.* )
  for m in "$@"; do cp "$d/$m" bin/lib/; done
  rm -rf "$d"
}
fetch_deb "$LIBC_DEB" "$LIBC_DEB_SHA" usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2 \
  usr/lib/x86_64-linux-gnu/libc.so.6 usr/lib/x86_64-linux-gnu/libm.so.6
fetch_deb "$LIBGCC_DEB" "$LIBGCC_DEB_SHA" usr/lib/x86_64-linux-gnu/libgcc_s.so.1
check() {   # file sha256
  got=$( (sha256sum "$1" 2>/dev/null || shasum -a 256 "$1") | cut -d' ' -f1)
  if [ "$got" = "$2" ]; then echo "$1 ok"; else echo "$1: sha256 $got, expected $2" >&2; exit 1; fi
}
check bin/collection-buggy.test ff20b709ed9904636b0e682c370c82d542e7629b4b57831750ae06e9a16eebe1
check bin/collection-fixed.test 15143bd7bca55649d1fda889307570a9bb6277f9bc0ab1d65870e4333465ce37
check bin/lib/ld-linux-x86-64.so.2 c20a2dc8917c755f02b94049356320fe1f62ac7d9f8994731f807d9df39302da
check bin/lib/libc.so.6 3a15d66867d83762c7f2f1e37359cb8f6c5743edb369c65285cb0b1c4f7498bf
check bin/lib/libm.so.6 fce00b6f25f459cf4ae0b7fae4257909a1a8a86f9149e7c029a5d617baf1ccd0
check bin/lib/libgcc_s.so.1 d93224d2b0dab4247598be683adca02f5cf00586f99c187579cd7e92058fb7cb
