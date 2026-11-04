#!/usr/bin/env bash
# build-spread.sh — FAST, ISOLATED builder for ONLY the SPREAD_ON_HVX Phase-1 rootfs
# assets. It stages the same pinned, byte-reproducible prelude that build-linux.sh uses
# (busybox, hvx_seedrng, the mke2fs+libfaketime+ctime-zero ext2/ext4 helpers, the virtio
# module set), then runs the AUTHORITATIVE spread §2m block by extracting it verbatim from
# build-linux.sh — so there is ONE source of truth for the spread target, and building it
# does NOT rebuild the slow JVM/Kafka closures. Run on the build machine via ./dev.sh "hv/hvx/images/build-spread.sh".
set -euo pipefail
cd "$(dirname "$0")/../../.."   # M100: this script lives in hv/hvx/images; every path below is relative to the repository root
mkdir -p build

# --- reproducible cpio (verbatim from build-linux.sh §prelude) ---
reproducible_cpio() {
  local dir="$1" out="$2"
  find "$dir" -exec touch --no-dereference -h -d @1600000000 {} + 2>/dev/null || \
    find "$dir" -exec touch -d @1600000000 {} +
  ( cd "$dir" && find . | LC_ALL=C sort | \
      cpio -o -H newc --owner=0:0 --reproducible 2>/dev/null | gzip -9n ) > "$out"
}

# --- 1. kernel: reuse the host's current NixOS bzImage ---
KERNEL="${HYDO_KERNEL:-/run/current-system/kernel}"
cp -f "$KERNEL" build/bzImage
echo "staged kernel: $KERNEL -> build/bzImage ($(stat -c%s build/bzImage) bytes)"

# --- 2. pinned static busybox ---
BB="${HYDO_BUSYBOX:-}"
if [ -z "$BB" ]; then
  BB=/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox
  [ -x "$BB" ] || BB=$(ls /nix/store/*busybox-static*/bin/busybox 2>/dev/null | head -1 || true)
fi
[ -x "$BB" ] || { echo "error: no static busybox (set HYDO_BUSYBOX)" >&2; exit 1; }

# --- 3. kernel module tree (virtio-blk + ext4 + ext2 stack for the switch_root initramfs) ---
KREL="${HYDO_KREL:-6.6.35}"
MODROOT="${HYDO_MODROOT:-/run/current-system/kernel-modules/lib/modules/$KREL}"
ROOTMODS="virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk crc16 crc32c_generic mbcache jbd2 ext4 ext2"

# --- 4. build hvx_seedrng (static musl), staged onto the ro-root by §2m ---
GCC_MUSL="${HYDO_GCC_MUSL:-}"
if [ -z "$GCC_MUSL" ]; then
  GCC_MUSL=/nix/store/aa1jsl19ywwfpqp0438vqbncc7sndnyw-gcc-static-x86_64-unknown-linux-musl-13.2.0/bin/gcc
  [ -x "$GCC_MUSL" ] || GCC_MUSL=$(ls /nix/store/*gcc-static*musl*/bin/gcc 2>/dev/null | head -1 || true)
fi
MUSL_ROOT="${HYDO_MUSL_ROOT:-}"
if [ -z "$MUSL_ROOT" ]; then
  MUSL_ROOT=/nix/store/0nlqfwsn4qpx1453gb4zvx6dvn3121l4-musl-static-x86_64-unknown-linux-musl-1.2.6
  [ -f "$MUSL_ROOT/lib/crt1.o" ] || MUSL_ROOT=$(dirname "$(dirname "$(ls /nix/store/*musl-static*/lib/crt1.o 2>/dev/null | head -1)")")
fi
[ -x "$GCC_MUSL" ] && [ -f "$MUSL_ROOT/lib/crt1.o" ] || { echo "error: no static musl gcc (set HYDO_GCC_MUSL + HYDO_MUSL_ROOT)" >&2; exit 1; }
"$GCC_MUSL" -static -O2 -Wall -B "${MUSL_ROOT%/}/lib" hv/hvx/images/hvx_seedrng.c -o build/hvx_seedrng
echo "built build/hvx_seedrng ($(stat -c%s build/hvx_seedrng) bytes, static musl)"

# --- 5. pinned mke2fs + libfaketime + the reproducible ext2/ext4 helpers (verbatim) ---
MKE2FS="${HYDO_MKE2FS:-}"
if [ -z "$MKE2FS" ]; then
  MKE2FS=/nix/store/24s0phlx9kly7x7qb8gal3mmn51ndmlx-e2fsprogs-1.47.0-bin/bin/mke2fs
  [ -x "$MKE2FS" ] || MKE2FS=$(ls /nix/store/*e2fsprogs-*-bin/bin/mke2fs 2>/dev/null | head -1 || true)
fi
[ -x "$MKE2FS" ] || { echo "error: mke2fs not found (set HYDO_MKE2FS)" >&2; exit 1; }
FAKETIME_BIN="${HYDO_FAKETIME:-}"
if [ -z "$FAKETIME_BIN" ]; then
  FAKETIME_BIN=/nix/store/fs0gp7zyfjnbvaq82yvwbcsvqh7876d1-libfaketime-0.9.10/bin/faketime
  if [ ! -x "$FAKETIME_BIN" ]; then
    FAKETIME_BIN=$(ls /nix/store/*libfaketime*/bin/faketime 2>/dev/null | head -1 || true)
    if [ -z "$FAKETIME_BIN" ] || [ ! -x "$FAKETIME_BIN" ]; then
      P=$(nix build --no-link --print-out-paths nixpkgs#libfaketime 2>/dev/null | head -1 || true)
      [ -n "$P" ] && [ -x "$P/bin/faketime" ] && FAKETIME_BIN="$P/bin/faketime"
    fi
  fi
fi
[ -x "$FAKETIME_BIN" ] || { echo "error: faketime not found (set HYDO_FAKETIME)" >&2; exit 1; }
FAKE_TS="2020-09-13 16:26:40"
# libfaketime reads FAKE_TS in the local time zone; it is 1600000000 only in UTC+4, the zone of the
# Nix build machine that made every recorded image. FAKE_TZ (POSIX notation for UTC+4)
# is set on every faketime call, so a build in another zone gets the same bytes.
FAKE_TZ=UTC-4

PY3="${HYDO_PY3:-}"
if [ -z "$PY3" ]; then
  PY3=/nix/store/14bkh67gwzg2hvqkadi03dyinrd7iybd-python3-3.11.9-env/bin/python3
  [ -x "$PY3" ] || PY3=$(ls /nix/store/*python3*/bin/python3 2>/dev/null | head -1 || true)
  [ -x "$PY3" ] || PY3=$(command -v python3 || true)
fi
[ -x "$PY3" ] || { echo "error: python3 not found (set HYDO_PY3)" >&2; exit 1; }

mkext2_reproducible() {
  local out="$1" uuid="$2" seed="$3"
  : > "$out"
  truncate -s 16777216 "$out"
  TZ="$FAKE_TZ" SOURCE_DATE_EPOCH=1600000000 "$FAKETIME_BIN" "$FAKE_TS" "$MKE2FS" -q -F -t ext2 -b 1024 \
    -U "$uuid" \
    -E hash_seed="$seed",lazy_itable_init=0,lazy_journal_init=0 \
    "$out"
}
mkext4_rootfs_reproducible() {   # $1=out $2=srcdir $3=size_MiB $4=uuid $5=hash_seed
  local out="$1" src="$2" mib="$3" uuid="$4" seed="$5"
  : > "$out"; truncate -s $(( mib * 1024 * 1024 )) "$out"
  TZ="$FAKE_TZ" SOURCE_DATE_EPOCH=1600000000 "$FAKETIME_BIN" "$FAKE_TS" "$MKE2FS" -q -F -t ext4 -b 4096 \
    -O ^metadata_csum,^64bit,^flex_bg,^resize_inode \
    -U "$uuid" \
    -E hash_seed="$seed",lazy_itable_init=0,lazy_journal_init=0 \
    -d "$src" "$out"
  "$PY3" - "$out" <<'PYEOF'
import sys, struct, mmap
f = open(sys.argv[1], "r+b"); mm = mmap.mmap(f.fileno(), 0)
u16 = lambda o: struct.unpack_from("<H", mm, o)[0]
u32 = lambda o: struct.unpack_from("<I", mm, o)[0]
sb = 1024
inodes = u32(sb + 0x00); bs = 1024 << u32(sb + 0x18)
ipg = u32(sb + 0x28); isize = u16(sb + 0x58) or 128
gdtb = 2 if bs == 1024 else 1
ng = (inodes + ipg - 1) // ipg
for g in range(ng):
    itbl = u32(gdtb * bs + g * 32 + 0x08)
    base = g * ipg
    for i in range(ipg):
        if base + i >= inodes:
            break
        struct.pack_into("<I", mm, itbl * bs + i * isize + 0x0C, 0)
mm.flush(); mm.close(); f.close()
PYEOF
}

# --- 6. run the AUTHORITATIVE spread §2m block, extracted verbatim from build-linux.sh
#        (the marker line to EOF). Single source of truth; no divergent copy. ---
echo ">> running the spread §2m block from build-linux.sh"
eval "$(sed -n '/^# ---- 2m\. the SPREAD assets/,$p' hv/hvx/images/build-linux.sh)"
echo ">> build-spread.sh: DONE"
