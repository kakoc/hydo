#!/bin/sh
# Build walrace-<v> (static) for SQLite 3.51.2 and 3.51.3 from the official amalgamation
# zips, pinned by sha256. Run on the bare-metal test machine (Ubuntu 24.04, gcc 13.3.0, static glibc):
#   sh hydo/knownbugs/sqlite/build.sh [outdir, default: bin/ next to this script]
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${1:-$HERE/bin}
mkdir -p "$OUT"
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
pin() {   # <v> <sha256>
  z=sqlite-amalgamation-$1.zip
  [ -f "$HERE/.cache/$z" ] || { mkdir -p "$HERE/.cache"; curl -sfL -o "$HERE/.cache/$z" https://www.sqlite.org/2026/$z; }
  echo "$2  $HERE/.cache/$z" | sha256sum -c - >/dev/null || { echo "sha256 mismatch for $z" >&2; exit 1; }
  (cd "$W" && unzip -q -o "$HERE/.cache/$z")
  d=$W/sqlite-amalgamation-$1
  # SQLITE_THREADSAFE=1 (serialized) is the amalgamation default; spelled out for clarity.
  gcc -O2 -static -pthread -s -DSQLITE_THREADSAFE=1 -DSQLITE_OMIT_LOAD_EXTENSION \
      -ffile-prefix-map="$W"=/src -I"$d" -o "$OUT/walrace-$1" "$HERE/walrace.c" "$d/sqlite3.c"
  echo "walrace-$1 $(sha256sum < "$OUT/walrace-$1" | cut -c1-16)"
}
pin 3510200 6e2a845a493026bdbad0618b2b5a0cf48584faab47384480ed9f592d912f23ec
pin 3510300 acb1e6f5d832484bf6d32b681e858c38add8b2acdfd42ac5df24b8afb46552b4
