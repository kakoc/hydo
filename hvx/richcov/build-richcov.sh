#!/usr/bin/env bash
# build-richcov.sh -- the four-service stack image for WHOLE-SYSTEM PT coverage (M43 prep).
#
# Wraps the unmodified build-stack.sh with three overrides, so the guest is the normal
# stack except for where its programs sit in memory and whether they keep symbols:
#   * the front door keeps its Go symbol table and is linked at 0x2000001000
#     (STACK_GO_LDFLAGS, the same layout the M40/M41 coverage runs used);
#   * Redis is the debug build from build-redis-debug.sh: static, DWARF, text at 0x40000000
#     (HYDO_REDIS_OUT);
#   * output name STACK_OUT=richcov, so no other image is touched.
# MySQL is the pinned binary unchanged: a PIE with a full .symtab (57,815 functions) but no
# DWARF. Its load address is fixed by booting the guest with `norandmaps` on the kernel
# command line (no ASLR), which puts a PIE executable at 0x555555554000.
#
# It also collects the files the decoder needs into build/richcov-syms/:
#   hydostack (ELF), redis-server (ELF), mysqld (ELF), System.map (guest kernel), layout.txt.
#
# Run on the build box:  ./dev.sh "bash hvx/richcov/build-richcov.sh"
set -euo pipefail
TOP="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$TOP"
MYSQL="${HYDO_MYSQL:-/nix/store/aw6ljbiwgds7yw8x2jc3340dm46s7i2k-mysql-8.4.10}"

[ -x build/richcov-redis/bin/redis-server ] || bash hvx/richcov/build-redis-debug.sh

STACK_OUT=richcov \
STACK_GO_LDFLAGS="-T 0x2000001000 -buildid=" \
HYDO_REDIS_OUT="$TOP/build/richcov-redis" \
  bash ./build-stack.sh

S=build/richcov-syms
rm -rf "${S:?}"; mkdir -p "$S"
cp build/richcov-hydostack "$S/hydostack"
cp build/richcov-redis/bin/redis-server "$S/redis-server"
cp "$MYSQL/bin/mysqld" "$S/mysqld"; chmod u+w "$S/mysqld"
KDIR="$(dirname "$(readlink -f /run/current-system/kernel)")"
[ -f "$KDIR/System.map" ] && cp "$KDIR/System.map" "$S/System.map"
cat > "$S/layout.txt" <<'EOF'
# program      elf            load-bias        notes
hydostack      hydostack      0                static Go, text at 0x2000001000, symbols kept
redis-server   redis-server   0                static musl, text at 0x40001000, DWARF
mysqld         mysqld         0x555555554000   PIE; bias valid only with `norandmaps` on the guest cmdline
EOF
( cd "$S" && sha256sum hydostack redis-server mysqld System.map 2>/dev/null )
echo "  sha256(richcov-disk.img)=$(sha256sum build/richcov-disk.img | cut -d' ' -f1)"
