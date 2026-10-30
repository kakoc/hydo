#!/usr/bin/env bash
# build-redis-debug.sh -- Redis 7.2.4 (redis-server + redis-cli) built statically with DWARF
# debug info and linked at a UNIQUE text address, for whole-system PT coverage (M43 prep).
#
# Why a rebuild: the pinned nix redis-static (/nix/store/ky1xq6xa...-7.2.4) keeps a .symtab
# (function names) but no DWARF, and it is a non-PIE EXEC at 0x400000 -- the same address as
# the guest's static busybox and other helpers, so a PT IP filter cannot tell them apart.
# This build: same 7.2.4 source tarball nix used, the project's pinned static musl gcc, -g,
# text segment at 0x40000000 (1 GiB). Above 2 GiB the static musl crt1 no longer links
# (its 32-bit PC-relative reference to the weak _DYNAMIC overflows), so Redis sits low and
# ONE PT address range [0x40000000, end of the front door at 0x2000001000+) covers both
# programs; nothing else executes in that window (busybox is at 0x400000, mysqld at the
# no-ASLR PIE base 0x555555554000, shared libraries and the JVM near 0x7fff...).
# Differences from the flagship Redis, on purpose and documented:
# MALLOC=libc (musl malloc, not the bundled jemalloc -- avoids jemalloc's own configure step)
# and BUILD_TLS=no. Use this binary only in the coverage image (STACK_OUT=richcov).
#
# Output: build/richcov-redis/bin/{redis-server,redis-cli}  (a drop-in HYDO_REDIS_OUT for
# build-stack.sh). Byte-reproducible: fixed tarball, pinned toolchain, -ffile-prefix-map,
# no build id, SOURCE_DATE_EPOCH.
# Run on the build box:  ./dev.sh "bash hydo/richcov/build-redis-debug.sh"
set -euo pipefail
TOP="$(cd "$(dirname "$0")/../.." && pwd)"
SRC_TGZ="${HYDO_REDIS_SRC:-/nix/store/lvkfs16yj6j49lmahzfyq435a3pxhxla-redis-7.2.4.tar.gz}"
GCC_MUSL="${HYDO_GCC_MUSL:-/nix/store/aa1jsl19ywwfpqp0438vqbncc7sndnyw-gcc-static-x86_64-unknown-linux-musl-13.2.0/bin/gcc}"
MUSL_ROOT="${HYDO_MUSL_ROOT:-/nix/store/0nlqfwsn4qpx1453gb4zvx6dvn3121l4-musl-static-x86_64-unknown-linux-musl-1.2.6}"
MAKE="${HYDO_MAKE:-/nix/store/18z454gyz0wpb641rw6gpqk0vi4wbxy6-gnumake-4.4.1/bin/make}"
BINUTILS="${HYDO_BINUTILS:-/nix/store/1fn92b0783crypjcxvdv6ycmvi27by0j-binutils-2.40/bin}"
TEXT_BASE="${HYDO_REDIS_TEXT:-0x40000000}"
for need in "$SRC_TGZ" "$GCC_MUSL" "$MUSL_ROOT/lib/crt1.o" "$MAKE" "$BINUTILS/ar"; do
  [ -e "$need" ] || { echo "error: missing $need" >&2; exit 1; }
done
export SOURCE_DATE_EPOCH=1600000000
W="$TOP/build/richcov-redis-src"
rm -rf "${W:?}"; mkdir -p "$W"
tar -xzf "$SRC_TGZ" -C "$W"
cd "$W/redis-7.2.4"
export PATH="$BINUTILS:$PATH"		# ar/ranlib for the bundled deps (lua, hiredis, ...)
# One target per make invocation: naming both on one command line runs the deps build
# (with its "make clean") twice in parallel, which left liblua.a incomplete.
for t in redis-server redis-cli; do
  "$MAKE" -j"$(nproc)" \
    CC="$GCC_MUSL -B $MUSL_ROOT/lib" AR="$BINUTILS/ar" RANLIB="$BINUTILS/ranlib" \
    MALLOC=libc BUILD_TLS=no USE_SYSTEMD=no OPTIMIZATION=-O2 \
    CFLAGS="-g -ffile-prefix-map=$W=/src" \
    LDFLAGS="-static -Wl,-Ttext-segment=$TEXT_BASE -Wl,--build-id=none" \
    "$t" >> "$W/make.log" 2>&1 || { grep -nE "error|undefined" "$W/make.log" | head -20; exit 1; }
done
OUT="$TOP/build/richcov-redis/bin"
rm -rf "${OUT:?}"; mkdir -p "$OUT"
cp src/redis-server src/redis-cli "$OUT/"
echo "built $OUT/redis-server ($(stat -c%s "$OUT/redis-server") bytes, sha256 $(sha256sum "$OUT/redis-server" | cut -c1-16))"
echo "built $OUT/redis-cli    ($(stat -c%s "$OUT/redis-cli") bytes, sha256 $(sha256sum "$OUT/redis-cli" | cut -c1-16))"
