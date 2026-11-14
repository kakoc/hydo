#!/bin/sh
# Build the test program of Qdrant's crate `collection` for this machine, at the commit of issue
# 10200's last report:
#   sh build-program.sh               # -> work/collection.test
# This is how the Mac runs of RESULTS.md got their program (aarch64, `cargo test -p collection
# --lib --no-run`, Rust 1.99.0). On Linux x86_64, ../build-test.sh builds the pinned program
# instead (bin/collection-buggy.test).
# Needs git, network once, rustup with Rust 1.99.0, and protoc (Qdrant's CI pins protoc 34.1; set
# PROTOC to its path). The clone and Cargo's build directory are in work/ (several GB).
set -eu
cd "$(dirname "$0")"
HERE=$(pwd)
COMMIT=222b2e44ab1c3f17e8246396ead6558e45041df7   # master, 2026-10-21, issue 10200's report
SRC=$HERE/work/qdrant-src
nolfs="-c filter.lfs.process= -c filter.lfs.smudge=cat -c filter.lfs.required=false"
if [ ! -d "$SRC/.git" ]; then
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin https://github.com/qdrant/qdrant.git
fi
GIT_LFS_SKIP_SMUDGE=1 git -C "$SRC" $nolfs fetch -q --depth 1 origin "$COMMIT"
git -C "$SRC" $nolfs -c advice.detachedHead=false checkout -q -f "$COMMIT"
json=$HERE/work/build.json
(cd "$SRC" && CARGO_TARGET_DIR="$HERE/work/target" \
   cargo +1.99.0 test -p collection --lib --no-run --locked --message-format=json-render-diagnostics > "$json")
exe=$(grep -o '"executable":"[^"]*collection-[^"]*"' "$json" | tail -1 | cut -d'"' -f4)
cp "$exe" "$HERE/work/collection.test"
echo "work/collection.test"
