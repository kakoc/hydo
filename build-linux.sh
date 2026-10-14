#!/usr/bin/env bash
# Stage a real Linux kernel + minimal busybox initramfs images for the hvx L2 boot.
# Run this on the KVM host (via ./dev.sh "./build-linux.sh"). It writes into build/,
# which dev.sh excludes from rsync, so the artifacts persist on the host.
#
#   build/bzImage               a real x86-64 kernel (the NixOS host kernel)
#   build/initramfs.cpio.gz     busybox initramfs whose /init drops to a shell
#                               (the ORIGINAL, unchanged -- the no-disk boot uses it
#                                and MUST keep the M16 fingerprint 1bdf266a0db52bf7)
#   build/initramfs-disk.cpio.gz  busybox initramfs that ALSO insmods the virtio-blk
#                               driver stack and reads /dev/vda (milestone 21 -- proves
#                               real Linux uses the virtio-blk disk over MSI-X)
#   build/vblk-disk.img         a small backing disk with a recognizable marker at
#                               sector 0 that the disk /init prints to the console
set -euo pipefail

mkdir -p build

# Build a byte-REPRODUCIBLE gzip'd cpio (newc) from a directory. cpio embeds each
# file's mtime/uid/gid, and `find` order + gzip's header timestamp vary run to run,
# so a naive `find | cpio | gzip` produces a different archive every build -- which,
# because the initramfs image is loaded into (hashed) guest RAM, changes the boot
# fingerprint. Normalize all of it (fixed mtime, owner 0:0, sorted names, gzip -n)
# so the same tree always yields the same bytes and the boot fingerprint is stable.
# $1 = source dir, $2 = output .cpio.gz
reproducible_cpio() {
  local dir="$1" out="$2"
  find "$dir" -exec touch --no-dereference -h -d @1600000000 {} + 2>/dev/null || \
    find "$dir" -exec touch -d @1600000000 {} +
  ( cd "$dir" && find . | LC_ALL=C sort | \
      cpio -o -H newc --owner=0:0 --reproducible 2>/dev/null | gzip -9n ) > "$out"
}

# 1. A real kernel: reuse the host's current NixOS bzImage.
KERNEL="${HYDO_KERNEL:-/run/current-system/kernel}"
cp -f "$KERNEL" build/bzImage
echo "staged kernel: $KERNEL -> build/bzImage ($(stat -c%s build/bzImage) bytes)"

# 2. A minimal initramfs from a statically-linked busybox found in the store.
BB="${HYDO_BUSYBOX:-}"
if [ -z "$BB" ]; then
  # PIN the exact busybox that established the documented boot fingerprints
  # (no-disk 640ece2f900fe5b3 / with-disk 36583d533d7c989b). The nix store may hold
  # SEVERAL busybox-static builds; a bare `ls | head -1` is order-dependent and
  # DRIFTS as the store gains entries -- a newly-added build can sort first, change
  # the busybox binary baked into the initramfs, and thus change the boot
  # fingerprint (the initramfs image is loaded into hashed guest RAM). Prefer the
  # known-good build; fall back to the first available only if it has been GC'd.
  BB=/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox
  [ -x "$BB" ] || BB=$(ls /nix/store/*busybox-static*/bin/busybox 2>/dev/null | head -1 || true)
fi
if [ -z "$BB" ] || [ ! -x "$BB" ]; then
  echo "error: no static busybox found; set HYDO_BUSYBOX=/path/to/busybox" >&2
  exit 1
fi

# ---- 2a. the ORIGINAL initramfs (unchanged; keeps the no-disk M16 fingerprint) ----
rm -rf build/initramfs
mkdir -p build/initramfs/bin build/initramfs/proc build/initramfs/sys build/initramfs/dev
cp "$BB" build/initramfs/bin/busybox
chmod +x build/initramfs/bin/busybox

cat > build/initramfs/init <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
echo ""
echo "=================================================="
echo "  hydo: initramfs reached — userspace is alive!"
echo "  uname: $(uname -a)"
echo "=================================================="
echo "hydo-boot-marker: launching shell on ttyS0"
exec /bin/busybox sh
INIT
chmod +x build/initramfs/init

reproducible_cpio build/initramfs build/initramfs.cpio.gz
echo "built build/initramfs.cpio.gz ($(stat -c%s build/initramfs.cpio.gz) bytes) from $BB"

# ---- 2b. a DISK-ENABLED initramfs (milestone 21): load the virtio-blk driver stack
#          and read /dev/vda. virtio is built as modules in this NixOS kernel, and the
#          busybox initramfs has none, so we stage the exact .ko's (decompressed) and
#          insmod them in dependency order from /init. This is a SEPARATE image so the
#          original (no-disk) boot above is byte-identical and keeps 1bdf266a0db52bf7. --
KREL="${HYDO_KREL:-6.6.35}"
MODROOT="${HYDO_MODROOT:-/run/current-system/kernel-modules/lib/modules/$KREL}"
rm -rf build/initramfs-disk
mkdir -p build/initramfs-disk/bin build/initramfs-disk/proc build/initramfs-disk/sys \
         build/initramfs-disk/dev build/initramfs-disk/lib/modules
cp "$BB" build/initramfs-disk/bin/busybox
chmod +x build/initramfs-disk/bin/busybox

# Load order matters (deps): virtio, virtio_ring, the two pci_*_dev libs, virtio_pci,
# then virtio_blk. Copy each .ko.xz out of the module tree and decompress to a plain
# .ko so busybox insmod (no xz support in insmod) can load it.
MODS="virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk"
for m in $MODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  if [ -z "$src" ]; then echo "error: module $m.ko.xz not found under $MODROOT" >&2; exit 1; fi
  xz -dc "$src" > "build/initramfs-disk/lib/modules/$m.ko"
done
echo "staged virtio modules: $MODS"

cat > build/initramfs-disk/init <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
echo ""
echo "=================================================="
echo "  hydo: initramfs reached (virtio-blk disk variant)"
echo "=================================================="
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk; do
  insmod /lib/modules/$m.ko && echo "hydo-vblk: insmod $m ok" || echo "hydo-vblk: insmod $m FAILED"
done
# devtmpfs creates /dev/vda when virtio_blk binds; wait briefly in case probe is async.
n=0
while [ ! -b /dev/vda ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
if [ -b /dev/vda ]; then
  echo "hydo-vblk: /dev/vda present"
  printf 'hydo-vblk-read: '
  dd if=/dev/vda bs=1 count=32 2>/dev/null
  echo ""
else
  echo "hydo-vblk: /dev/vda MISSING (virtio-blk did not bind)"
fi
echo "hydo-disk-marker: vda read complete"
echo "hydo-boot-marker: launching shell on ttyS0"
exec /bin/busybox sh
INIT
chmod +x build/initramfs-disk/init

reproducible_cpio build/initramfs-disk build/initramfs-disk.cpio.gz
echo "built build/initramfs-disk.cpio.gz ($(stat -c%s build/initramfs-disk.cpio.gz) bytes)"

# ---- 2d. a NET-ENABLED initramfs (milestone 23): load the virtio-net driver stack,
#          bring up eth0 with a STATIC IP (no DHCP -- DHCP needs a responder + adds
#          nondeterminism), so the kernel replies to a scripted ARP request the loader
#          pushes via HVX_PUT_RX. Separate image; the no-disk/no-net boots stay
#          byte-identical and keep their fingerprints. --
rm -rf build/initramfs-net
mkdir -p build/initramfs-net/bin build/initramfs-net/proc build/initramfs-net/sys \
         build/initramfs-net/dev build/initramfs-net/lib/modules
cp "$BB" build/initramfs-net/bin/busybox
chmod +x build/initramfs-net/bin/busybox

# virtio-net deps: virtio, virtio_ring, the pci_*_dev libs, virtio_pci, then
# failover + net_failover (virtio_net depends on them), then virtio_net.
NETMODS="virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci failover net_failover virtio_net"
for m in $NETMODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  if [ -z "$src" ]; then echo "error: module $m.ko.xz not found under $MODROOT" >&2; exit 1; fi
  xz -dc "$src" > "build/initramfs-net/lib/modules/$m.ko"
done
echo "staged virtio-net modules: $NETMODS"

cat > build/initramfs-net/init <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
echo ""
echo "=================================================="
echo "  hydo: initramfs reached (virtio-net variant)"
echo "=================================================="
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci failover net_failover virtio_net; do
  insmod /lib/modules/$m.ko && echo "hydo-net: insmod $m ok" || echo "hydo-net: insmod $m FAILED"
done
# eth0 appears when virtio_net binds; wait briefly in case probe is async.
n=0
while [ ! -e /sys/class/net/eth0 ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
if [ -e /sys/class/net/eth0 ]; then
  echo "hydo-net: eth0 mac $(cat /sys/class/net/eth0/address)"
  # STATIC IP (no DHCP): assign 10.0.0.2/24 and bring the link up. ifconfig sets the
  # address before the UP flag, so the IP is live when virtnet_open kicks the receiveq
  # (which delivers the scripted ARP request the loader pushed before boot).
  echo 1 > /proc/sys/net/ipv4/conf/all/arp_accept 2>/dev/null
  echo 0 > /proc/sys/net/ipv4/conf/all/rp_filter 2>/dev/null
  echo 0 > /proc/sys/net/ipv4/conf/eth0/rp_filter 2>/dev/null
  ifconfig eth0 10.0.0.2 netmask 255.255.255.0 up
  echo "hydo-net: eth0 up ip=10.0.0.2"
  echo "hydo-net: ifconfig:"; ifconfig eth0
  echo "hydo-net: route:"; cat /proc/net/route
  # give the RX interrupt + NAPI a moment to deliver the pushed ARP and reply
  sleep 2
  echo "hydo-net: rx_packets=$(cat /sys/class/net/eth0/statistics/rx_packets) tx_packets=$(cat /sys/class/net/eth0/statistics/tx_packets) rx_dropped=$(cat /sys/class/net/eth0/statistics/rx_dropped)"
  echo "hydo-net: arp table:"; cat /proc/net/arp
else
  echo "hydo-net: eth0 MISSING (virtio_net did not bind)"
fi
echo "hydo-net-marker: interface configured"
echo "hydo-boot-marker: launching shell on ttyS0"
exec /bin/busybox sh
INIT
chmod +x build/initramfs-net/init

reproducible_cpio build/initramfs-net build/initramfs-net.cpio.gz
echo "built build/initramfs-net.cpio.gz ($(stat -c%s build/initramfs-net.cpio.gz) bytes)"

# ---- 2c. a backing disk with a recognizable 32-byte marker at sector 0 ----
#      (the disk /init above reads and prints the first 32 bytes). Kept well under
#      HVX_DISK_MAX (1 MiB); the marker is fixed-length with no embedded NULs. --
: > build/vblk-disk.img
printf 'HYDO-VBLK-DISK-OK-M21-0123456789' > build/vblk-disk.img   # exactly 32 bytes
truncate -s 65536 build/vblk-disk.img                             # 64 KiB backing image
echo "built build/vblk-disk.img ($(stat -c%s build/vblk-disk.img) bytes, marker at sector 0)"

# ---- 2e. the FLAGSHIP KV initramfs (milestone 25): the software-under-test.
#          Builds the hydokv raw-block KV store (hvx/hydokv.c) STATICALLY with the
#          pinned musl toolchain on the host, stages it with the SAME virtio-blk
#          module stack as the disk initramfs, and its /init runs the workload +
#          oracle and prints the machine-checkable HYDO-ORACLE verdict + the
#          flagship stop marker. A SEPARATE image, so every prior boot stays
#          byte-identical and keeps its fingerprint. The store is fault-agnostic:
#          the seeded disk fault (HVX_FAULT) is injected BELOW it by the module. --
#
# Build hydokv static+reproducible with the pinned musl gcc (same toolchain family
# as the pinned static busybox). Bare invocation of the store-path gcc needs its
# companion musl crt/libc via -B; PIN both paths (fall back to a store scan if
# GC'd) so the binary -- hence the initramfs -- is byte-identical run to run.
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
if [ -z "$GCC_MUSL" ] || [ ! -x "$GCC_MUSL" ] || [ ! -f "$MUSL_ROOT/lib/crt1.o" ]; then
  echo "error: no static musl gcc found (set HYDO_GCC_MUSL + HYDO_MUSL_ROOT)" >&2
  exit 1
fi
"$GCC_MUSL" -static -O2 -Wall -B "${MUSL_ROOT%/}/lib" hvx/hydokv.c -o build/hydokv
echo "built build/hydokv ($(stat -c%s build/hydokv) bytes, static musl) via $GCC_MUSL"

rm -rf build/initramfs-kv
mkdir -p build/initramfs-kv/bin build/initramfs-kv/proc build/initramfs-kv/sys \
         build/initramfs-kv/dev build/initramfs-kv/lib/modules
cp "$BB" build/initramfs-kv/bin/busybox
chmod +x build/initramfs-kv/bin/busybox
cp build/hydokv build/initramfs-kv/bin/hydokv
chmod +x build/initramfs-kv/bin/hydokv

# same virtio-blk load order as the disk initramfs
for m in $MODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  if [ -z "$src" ]; then echo "error: module $m.ko.xz not found under $MODROOT" >&2; exit 1; fi
  xz -dc "$src" > "build/initramfs-kv/lib/modules/$m.ko"
done
echo "staged virtio modules for the KV initramfs: $MODS"

cat > build/initramfs-kv/init <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
echo ""
echo "=================================================="
echo "  hydo: initramfs reached (M25 flagship KV store)"
echo "=================================================="
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk; do
  insmod /lib/modules/$m.ko && echo "hydo-kv: insmod $m ok" || echo "hydo-kv: insmod $m FAILED"
done
# devtmpfs creates /dev/vda when virtio_blk binds; wait briefly for async probe.
n=0
while [ ! -b /dev/vda ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
if [ -b /dev/vda ]; then
  echo "hydo-kv: /dev/vda present -- running hydokv (workload + oracle)"
  /bin/hydokv
else
  echo "hydo-kv: /dev/vda MISSING (virtio-blk did not bind)"
  echo "HYDO-ORACLE: ERROR no device"
  echo "hydo-flagship-marker: oracle complete"
fi
echo "hydo-boot-marker: launching shell on ttyS0"
exec /bin/busybox sh
INIT
chmod +x build/initramfs-kv/init

reproducible_cpio build/initramfs-kv build/initramfs-kv.cpio.gz
echo "built build/initramfs-kv.cpio.gz ($(stat -c%s build/initramfs-kv.cpio.gz) bytes)"

# ---- 2f. a ZEROED backing disk for the KV store (milestone 25). hydokv writes
#      one 512-byte record per key at sectors 1..NKEYS; starting from all-zeros
#      makes a DROP-faulted (lost) record read back as a zero sector, so the
#      oracle cleanly detects the loss. 64 KiB (128 sectors), well under
#      HVX_DISK_MAX (1 MiB). --
: > build/kv-disk.img
truncate -s 65536 build/kv-disk.img
echo "built build/kv-disk.img ($(stat -c%s build/kv-disk.img) bytes, zeroed)"

# ---- 2g. the M26 FLAGSHIP-ON-UNMODIFIED-SQLITE assets. Unlike M25's purpose-built
#      hydokv, this runs an UNMODIFIED, off-the-shelf static `sqlite3` CLI on a REAL
#      ext2 filesystem staged as /dev/vda. Two byte-reproducible artifacts:
#        build/sqlite3            a STATIC musl sqlite3 CLI (nobody wrote it for us)
#        build/sql-disk.img       a byte-reproducible EMPTY ext2 image (pinned UUID +
#                                 SOURCE_DATE_EPOCH so the staged bytes never drift)
#        build/initramfs-sql.cpio.gz  the virtio-blk + ext2 module stack + sqlite3 +
#                                 an /init that mounts the fs, runs committed INSERTs,
#                                 drops the page cache (so a fresh read hits the
#                                 DEVICE, not the cache), and an oracle that re-reads
#                                 every committed row and prints HYDO-ORACLE.
#      The SQLite DB itself is CREATED AT RUNTIME by /init (deterministic: fixed keys,
#      value a pure function of the key, a fixed pad), so only the empty fs image is
#      staged. All a few MiB, well under the bumped HVX_DISK_MAX (64 MiB). --

# A static, unmodified sqlite3 CLI. pkgsStatic.sqlite's -bin output carries it; PIN the
# exact store path (fetchable from cache) with a `nix build` fallback so the binary --
# hence the initramfs -- is byte-identical run to run (same discipline as busybox/gcc).
SQLITE3="${HYDO_SQLITE3:-}"
if [ -z "$SQLITE3" ]; then
  SQLITE3=/nix/store/6c7pf5c2cy30hglc0dfsdd5v25db362p-sqlite-static-x86_64-unknown-linux-musl-3.45.3-bin/bin/sqlite3
  if [ ! -x "$SQLITE3" ]; then
    P=$(nix build --no-link --print-out-paths "nixpkgs#pkgsStatic.sqlite" 2>/dev/null | head -1 || true)
    [ -n "$P" ] && [ -x "$P/bin/sqlite3" ] && SQLITE3="$P/bin/sqlite3"
  fi
fi
if [ -z "$SQLITE3" ] || [ ! -x "$SQLITE3" ]; then
  echo "error: no static sqlite3 CLI found (set HYDO_SQLITE3=/path/to/sqlite3)" >&2
  exit 1
fi
cp -f "$SQLITE3" build/sqlite3
chmod +x build/sqlite3
echo "built build/sqlite3 ($(stat -c%s build/sqlite3) bytes, static) from $SQLITE3"

# A BYTE-REPRODUCIBLE empty ext2 image. mke2fs stamps NON-deterministic bytes on every run:
# a random filesystem UUID, superblock/inode TIMESTAMPS (mkfs / last-mount / last-write /
# last-check + every inode's a/c/m/crtime), and a random directory HASH SEED -- and this
# e2fsprogs build (1.47.0) does NOT honor SOURCE_DATE_EPOCH (verified on the remote: with it
# set/exported, the created/last-write/last-checked and all inode times are still stamped with
# the real wall clock). A naive `mke2fs` therefore drifts the image bytes every build, which --
# because the image is loaded into (hashed) guest RAM as the disk -- DRIFTS the M26/M27 disk
# fingerprints (exactly the regression the M27 agent hit: the sqlite baseline moved only because
# this image was rebuilt). PIN ALL OF IT so a rebuild is byte-identical (same determinism
# discipline as the pinned busybox/musl toolchain):
#   * fixed UUID (-U) and fixed directory hash_seed (-E hash_seed=...);
#   * FROZEN timestamps via libfaketime (SOURCE_DATE_EPOCH is ineffective in this build): every
#     time() inside mke2fs returns one fixed instant, so ALL superblock + inode time fields are
#     constant. SOURCE_DATE_EPOCH is still exported (harmless; documents intent + future-proofs
#     against an e2fsprogs that does honor it);
#   * -E lazy_itable_init=0,lazy_journal_init=0 so nothing is written lazily/nondeterministically;
#   * the mke2fs (e2fsprogs) AND faketime binaries are PINNED to fixed nix store paths (with a
#     fallback scan / nix-build), so the TOOLS themselves can't drift as the shared box's nix
#     store churns -- the same recurring drift class that moved the busybox pin (M21/M23) and,
#     via the host mke2fs version, this fs image (M27).
# 16 MiB / 1 KiB blocks; no journal (ext2) to keep the guest module set + mount path minimal.
# Proven byte-identical across separate builds by `sha256sum` (the reproducibility deliverable).
MKE2FS="${HYDO_MKE2FS:-}"
if [ -z "$MKE2FS" ]; then
  # PIN the exact e2fsprogs build; fall back to the first store match only if GC'd. (The two
  # 1.47.0 builds present on the box produce byte-identical images, so the fallback is safe.)
  MKE2FS=/nix/store/24s0phlx9kly7x7qb8gal3mmn51ndmlx-e2fsprogs-1.47.0-bin/bin/mke2fs
  [ -x "$MKE2FS" ] || MKE2FS=$(ls /nix/store/*e2fsprogs-*-bin/bin/mke2fs 2>/dev/null | head -1 || true)
fi
if [ -z "$MKE2FS" ] || [ ! -x "$MKE2FS" ]; then echo "error: mke2fs not found (set HYDO_MKE2FS)" >&2; exit 1; fi
FAKETIME_BIN="${HYDO_FAKETIME:-}"
if [ -z "$FAKETIME_BIN" ]; then
  # PIN libfaketime; fall back to a store scan, then a nix build (cache-fetchable).
  FAKETIME_BIN=/nix/store/fs0gp7zyfjnbvaq82yvwbcsvqh7876d1-libfaketime-0.9.10/bin/faketime
  if [ ! -x "$FAKETIME_BIN" ]; then
    FAKETIME_BIN=$(ls /nix/store/*libfaketime*/bin/faketime 2>/dev/null | head -1 || true)
    if [ -z "$FAKETIME_BIN" ] || [ ! -x "$FAKETIME_BIN" ]; then
      P=$(nix build --no-link --print-out-paths nixpkgs#libfaketime 2>/dev/null | head -1 || true)
      [ -n "$P" ] && [ -x "$P/bin/faketime" ] && FAKETIME_BIN="$P/bin/faketime"
    fi
  fi
fi
if [ -z "$FAKETIME_BIN" ] || [ ! -x "$FAKETIME_BIN" ]; then
  echo "error: faketime (libfaketime) not found (set HYDO_FAKETIME)" >&2; exit 1
fi
FAKE_TS="2020-09-13 16:26:40"   # == SOURCE_DATE_EPOCH 1600000000, frozen for every mke2fs time field

# Build ONE byte-reproducible empty ext2 image.  $1=output  $2=UUID  $3=hash_seed
mkext2_reproducible() {
  local out="$1" uuid="$2" seed="$3"
  : > "$out"
  truncate -s 16777216 "$out"
  SOURCE_DATE_EPOCH=1600000000 "$FAKETIME_BIN" "$FAKE_TS" "$MKE2FS" -q -F -t ext2 -b 1024 \
    -U "$uuid" \
    -E hash_seed="$seed",lazy_itable_init=0,lazy_journal_init=0 \
    "$out"
}

mkext2_reproducible build/sql-disk.img \
  11111111-2222-3333-4444-555555555555 66666666-7777-8888-9999-aaaaaaaaaaaa
echo "built build/sql-disk.img ($(stat -c%s build/sql-disk.img) bytes, reproducible ext2) via $MKE2FS + faketime"

# The SQL initramfs: virtio-blk stack + ext2 stack + the static sqlite3 + the /init.
SQLMODS="virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk mbcache ext2"
rm -rf build/initramfs-sql
mkdir -p build/initramfs-sql/bin build/initramfs-sql/proc build/initramfs-sql/sys \
         build/initramfs-sql/dev build/initramfs-sql/mnt build/initramfs-sql/lib/modules
cp "$BB" build/initramfs-sql/bin/busybox
chmod +x build/initramfs-sql/bin/busybox
cp build/sqlite3 build/initramfs-sql/bin/sqlite3
chmod +x build/initramfs-sql/bin/sqlite3
for m in $SQLMODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  if [ -z "$src" ]; then echo "error: module $m.ko.xz not found under $MODROOT" >&2; exit 1; fi
  xz -dc "$src" > "build/initramfs-sql/lib/modules/$m.ko"
done
echo "staged modules for the SQL initramfs: $SQLMODS"

cat > build/initramfs-sql/init <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
mkdir -p /tmp /mnt
echo ""
echo "=================================================="
echo "  hydo: initramfs reached (M26 flagship: UNMODIFIED SQLite on a real ext2 fs)"
echo "=================================================="
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk mbcache ext2; do
  insmod /lib/modules/$m.ko 2>/dev/null && echo "hydo-sql: insmod $m ok" || echo "hydo-sql: insmod $m (skipped/failed)"
done
n=0
while [ ! -b /dev/vda ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
if [ ! -b /dev/vda ]; then
  echo "hydo-sql: /dev/vda MISSING (virtio-blk did not bind)"
  echo "HYDO-ORACLE: ERROR no device"
  echo "hydo-sqlite-marker: oracle complete"
  exec /bin/busybox sh
fi
mkdir -p /mnt
if ! mount -t ext2 -o rw /dev/vda /mnt 2>/dev/null; then
  echo "hydo-sql: mount /dev/vda (ext2) FAILED"
  echo "HYDO-ORACLE: ERROR mount failed"
  echo "hydo-sqlite-marker: oracle complete"
  exec /bin/busybox sh
fi
echo "hydo-sql: mounted ext2 /dev/vda on /mnt; sqlite3 $(/bin/sqlite3 --version 2>/dev/null | cut -d' ' -f1)"
DB=/mnt/test.db
NKEYS=64
# a fixed, deterministic ~2000-byte pad so each row spans roughly one 4 KiB DB page
# (many pages => a dropped physical write reliably loses a committed row's page).
PAD=$(awk 'BEGIN{s="";while(length(s)<2000)s=s"0123456789abcdef";print substr(s,1,2000)}')
# ---- WORKLOAD: create the DB + NKEYS COMMITTED rows. synchronous=FULL => sqlite
#      fsyncs on every commit, so each row is acknowledged as durable (correct app
#      configuration -- NOT synchronous=OFF). journal_mode=DELETE (rollback journal
#      deleted after each commit) keeps only test.db behind. Keys 0..NKEYS-1, value a
#      pure function of the key, no time/RNG -> the workload is deterministic. ----
{
  echo "PRAGMA page_size=4096;"
  echo "PRAGMA journal_mode=DELETE;"
  echo "PRAGMA synchronous=FULL;"
  echo "PRAGMA mmap_size=0;"
  echo "CREATE TABLE kv(k INTEGER PRIMARY KEY, v INTEGER NOT NULL, pad TEXT);"
  k=0
  while [ $k -lt $NKEYS ]; do
    echo "INSERT INTO kv VALUES($k, $((100000+k)), '$PAD');"
    k=$((k+1))
  done
} | /bin/sqlite3 "$DB"
sync
echo "hydo-sql: committed $NKEYS rows (synchronous=FULL); db=$(wc -c < $DB 2>/dev/null) bytes"
# ---- CRASH / RECOVER: force everything to the device, then DROP the page cache so a
#      fresh read must hit the DEVICE (the true on-disk state), not the cache holding
#      the value the guest wrote. A dropped physical write is invisible to a cached
#      read; this makes the durability check real. The fs stays mounted (metadata
#      cached), so a dropped write is only observable as lost DB-file content. ----
sync
echo 3 > /proc/sys/vm/drop_caches
echo "hydo-sql: synced + dropped page cache (a fresh read now hits the device)"
# ---- ORACLE: a FRESH sqlite3 connection re-reads every committed row. Invariant =
#      READ-COMMITTED-DURABILITY (every acknowledged-committed key reads back with its
#      committed value). Falsified by a lost/corrupt committed write. ----
/bin/sqlite3 "$DB" "PRAGMA mmap_size=0; SELECT k||' '||v FROM kv ORDER BY k;" > /tmp/got 2>/tmp/err
# NOTE: `exp` is a reserved awk builtin (exponential); the expected-value array is `expv`.
awk -v n=$NKEYS '
  BEGIN{ for(i=0;i<n;i++) expv[i]=100000+i; viol=-1; lost=0 }
  { seen[$1]=$2 }
  END{
    for(i=0;i<n;i++){
      if(!(i in seen)){ if(viol<0){viol=i; vexp=expv[i]; vgot="MISSING"} lost++ }
      else if(seen[i]+0 != expv[i]){ if(viol<0){viol=i; vexp=expv[i]; vgot=seen[i]} lost++ }
    }
    if(viol<0) printf "HYDO-ORACLE: OK keys=%d durable\n", n;
    else printf "HYDO-ORACLE: VIOLATION key=%d expected=%d got=%s\n", viol, vexp, vgot;
    if(lost>0) printf "hydo-sql: %d of %d committed keys LOST or corrupt (a committed row vanished)\n", lost, n;
  }' /tmp/got
if [ -s /tmp/err ]; then echo "hydo-sql: sqlite reported: $(head -c 160 /tmp/err | tr '\n' ' ')"; fi
echo "hydo-sqlite-marker: oracle complete"
umount /mnt 2>/dev/null
exec /bin/busybox sh
INIT
chmod +x build/initramfs-sql/init

reproducible_cpio build/initramfs-sql build/initramfs-sql.cpio.gz
echo "built build/initramfs-sql.cpio.gz ($(stat -c%s build/initramfs-sql.cpio.gz) bytes)"

# ---- 2h. the M27 FLAGSHIP-ON-UNMODIFIED-REDIS assets. An UNMODIFIED, off-the-shelf
#      static `redis-server` + `redis-cli` (pkgsStatic.redis), CO-LOCATED with a client
#      in ONE guest talking over the in-kernel 127.0.0.1 LOOPBACK (loopback-first: no
#      virtio-net, deterministic for free). Persistence is the AOF on a REAL ext2 fs on
#      /dev/vda (reuses M26's fs+disk work). Three byte-reproducible artifacts:
#        build/redis-server, build/redis-cli   static musl redis 7.2.4 (nobody wrote it for us)
#        build/redis-disk.img                   a byte-reproducible EMPTY ext2 image (pinned
#                                               UUID + SOURCE_DATE_EPOCH so it never drifts)
#        build/initramfs-redis.cpio.gz          virtio-blk + ext2 stack + redis + an /init
#                                               that (per the kernel cmdline hydo_phase=)
#                                               brings up lo, mounts the fs, and either
#                                               drives committed SETs (workload) or AOF-
#                                               replays + an oracle re-reads them (oracle),
#                                               or both in one timeline (solo).
#      All a few MiB, well under HVX_DISK_MAX (64 MiB). --

# A static, unmodified redis. pkgsStatic.redis builds it musl-static; its checkPhase runs
# the redis tcl test suite which fails in the sandbox on one AOF-manifest assertion (a
# harmless test-env quirk, not a code bug), so we skip the check with an overrideAttrs.
# PIN the exact store path (same discipline as busybox/sqlite) with a `nix build` fallback.
REDIS_OUT="${HYDO_REDIS_OUT:-}"
if [ -z "$REDIS_OUT" ]; then
  REDIS_OUT=/nix/store/ky1xq6xa88qhwwagnh4anm4sjl3c7vyk-redis-static-x86_64-unknown-linux-musl-7.2.4
  if [ ! -x "$REDIS_OUT/bin/redis-server" ]; then
    REDIS_OUT=$(nix build --no-link --print-out-paths --impure --expr \
      '((builtins.getFlake "nixpkgs").legacyPackages.x86_64-linux.pkgsStatic.redis.overrideAttrs (o: { doCheck = false; doInstallCheck = false; })).outPath' \
      2>/dev/null | head -1 || true)
  fi
fi
if [ -z "$REDIS_OUT" ] || [ ! -x "$REDIS_OUT/bin/redis-server" ] || [ ! -x "$REDIS_OUT/bin/redis-cli" ]; then
  echo "error: no static redis found (set HYDO_REDIS_OUT=/nix/store/...-redis-static-...)" >&2
  exit 1
fi
cp -f "$REDIS_OUT/bin/redis-server" build/redis-server
cp -f "$REDIS_OUT/bin/redis-cli"    build/redis-cli
chmod +x build/redis-server build/redis-cli
echo "built build/redis-server ($(stat -c%s build/redis-server) bytes) + redis-cli ($(stat -c%s build/redis-cli) bytes, static) from $REDIS_OUT"

# A BYTE-REPRODUCIBLE empty ext2 image for the AOF (same pinned recipe as the SQL disk via
# mkext2_reproducible: pinned mke2fs + faketime-frozen timestamps + fixed UUID + fixed
# hash_seed + no lazy init). 16 MiB / 1 KiB blocks; ext2 (no journal) keeps the guest module
# set + mount path minimal. The AOF is created at RUNTIME.
mkext2_reproducible build/redis-disk.img \
  22222222-3333-4444-5555-666666666666 77777777-8888-9999-aaaa-bbbbbbbbbbbb
echo "built build/redis-disk.img ($(stat -c%s build/redis-disk.img) bytes, reproducible ext2) via $MKE2FS + faketime"

# The redis initramfs: virtio-blk + ext2 module stack + redis binaries + the /init.
REDISMODS="$SQLMODS"     # same virtio-blk + ext2 stack as the SQL initramfs
rm -rf build/initramfs-redis
mkdir -p build/initramfs-redis/bin build/initramfs-redis/proc build/initramfs-redis/sys \
         build/initramfs-redis/dev build/initramfs-redis/mnt build/initramfs-redis/tmp \
         build/initramfs-redis/lib/modules
cp "$BB" build/initramfs-redis/bin/busybox
chmod +x build/initramfs-redis/bin/busybox
cp build/redis-server build/initramfs-redis/bin/redis-server
cp build/redis-cli    build/initramfs-redis/bin/redis-cli
chmod +x build/initramfs-redis/bin/redis-server build/initramfs-redis/bin/redis-cli
# hvx_seedrng: deterministically seed the CRNG so redis's blocking getrandom() does not
# hang (built static+reproducible with the same pinned musl gcc as hydokv, §2e).
"$GCC_MUSL" -static -O2 -Wall -B "${MUSL_ROOT%/}/lib" hvx/hvx_seedrng.c -o build/hvx_seedrng
cp build/hvx_seedrng build/initramfs-redis/bin/hvx_seedrng
chmod +x build/initramfs-redis/bin/hvx_seedrng
echo "built build/hvx_seedrng ($(stat -c%s build/hvx_seedrng) bytes, static musl)"
for m in $REDISMODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  if [ -z "$src" ]; then echo "error: module $m.ko.xz not found under $MODROOT" >&2; exit 1; fi
  xz -dc "$src" > "build/initramfs-redis/lib/modules/$m.ko"
done
echo "staged modules for the redis initramfs: $REDISMODS"

cat > build/initramfs-redis/init <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
mkdir -p /tmp /mnt
# ---- parse the kernel cmdline: which phase, which fsync policy ----
PHASE=solo
FSYNC=everysec
NKEYS=48
for tok in $(cat /proc/cmdline); do
  case "$tok" in
    hydo_phase=*) PHASE=${tok#hydo_phase=} ;;
    hydo_fsync=*) FSYNC=${tok#hydo_fsync=} ;;
  esac
done
echo ""
echo "=================================================="
echo "  hydo: initramfs reached (M27 flagship: UNMODIFIED redis, loopback client+server)"
echo "  phase=$PHASE fsync=$FSYNC keys=$NKEYS"
echo "=================================================="
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk mbcache ext2; do
  insmod /lib/modules/$m.ko 2>/dev/null && echo "hydo-redis: insmod $m ok" || echo "hydo-redis: insmod $m (skipped/failed)"
done
# ---- bring up the in-kernel loopback (deterministic; no virtio-net needed) ----
ifconfig lo 127.0.0.1 netmask 255.0.0.0 up 2>/dev/null
ip link set lo up 2>/dev/null
echo "hydo-redis: loopback -> $(ifconfig lo 2>/dev/null | grep -o 'inet addr:[0-9.]*' | head -1)"
n=0
while [ ! -b /dev/vda ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
if [ ! -b /dev/vda ]; then
  echo "hydo-redis: /dev/vda MISSING (virtio-blk did not bind)"
  echo "HYDO-ORACLE: ERROR no device"
  echo "hydo-redis-marker: oracle complete"
  while true; do sleep 100; done
fi
if ! mount -t ext2 -o rw /dev/vda /mnt 2>/dev/null; then
  echo "hydo-redis: mount /dev/vda (ext2) FAILED"
  echo "HYDO-ORACLE: ERROR mount failed"
  echo "hydo-redis-marker: oracle complete"
  while true; do sleep 100; done
fi
echo "hydo-redis: mounted ext2 /dev/vda on /mnt"

# ---- seed the CRNG deterministically so redis's blocking getrandom() (hash seed) does
#      not spin: a deterministic hypervisor exposes no hardware entropy + a jitter-free
#      TSC, so the kernel's entropy loop never converges. hvx_seedrng credits a fixed
#      block via RNDADDENTROPY -> CRNG initialised -> getrandom() returns at once. ----
hvx_seedrng || echo "hydo-redis: hvx_seedrng FAILED (redis may hang on getrandom)"

# ---- redis config: AOF on the ext2 fs; loopback only; NO RDB, NO auto-rewrite (avoid
#      background-save forks). appendfsync from the cmdline (always | everysec). ----
cat > /tmp/redis.conf <<EOF
port 6379
bind 127.0.0.1
protected-mode no
dir /mnt
save ""
appendonly yes
appendfsync $FSYNC
appenddirname appendonlydir
appendfilename appendonly.aof
auto-aof-rewrite-percentage 0
aof-load-truncated yes
aof-timestamp-enabled no
daemonize no
logfile /tmp/redis.log
loglevel notice
hz 1
dynamic-hz no
EOF

# Precompute the space-separated key list once (used by the SET pipe + the MGET oracle).
KEYLIST=""; k=0; while [ $k -lt $NKEYS ]; do KEYLIST="$KEYLIST key:$k"; k=$((k+1)); done

start_redis() {
  echo "hydo-redis: starting redis-server (fsync=$FSYNC)"
  redis-server /tmp/redis.conf >/tmp/redis.out 2>&1 &
  RPID=$!
  sleep 3
  echo "hydo-redis: --- redis.out (first 3s) ---"; head -30 /tmp/redis.out 2>/dev/null
  echo "hydo-redis: --- redis.log (first 3s) ---"; tail -18 /tmp/redis.log 2>/dev/null
  echo "hydo-redis: --- end redis startup dump ---"
  n=0
  while [ $n -lt 20 ]; do
    if redis-cli -p 6379 ping 2>/dev/null | grep -q PONG; then
      echo "hydo-redis: redis ready after $n polls (loopback client<->server OK)"
      return 0
    fi
    n=$((n+1)); sleep 1
  done
  echo "hydo-redis: redis NOT ready; redis.log:"; tail -12 /tmp/redis.log 2>/dev/null
  return 1
}

# A fixed ~1500-byte pad so each committed value spans ~half a 4 KiB AOF page: the AOF for
# NKEYS keys then covers many independent pages, so a per-request DROP (F3) reliably loses a
# NAMED key (not masked by a later same-page rewrite) and different seeds lose different keys.
VPAD=$(awk 'BEGIN{s="";while(length(s)<1500)s=s"0123456789abcdef";print substr(s,1,1500)}')

do_workload() {   # drive NKEYS committed SETs over 127.0.0.1 (each reply OK = acked)
  # value = "<100000+k>_<pad>" (deterministic); the oracle recovers the number before "_".
  { k=0; while [ $k -lt $NKEYS ]; do echo "SET key:$k $((100000+k))_$VPAD"; k=$((k+1)); done; } \
    | redis-cli -p 6379 > /tmp/setout 2>&1
  ACKED=$(grep -c '^OK' /tmp/setout)
  echo "hydo-redis: workload SET acked=$ACKED / $NKEYS over 127.0.0.1"
}

do_oracle() {     # re-read every acked key in ONE MGET; invariant = READ-COMMITTED-DURABILITY
  redis-cli -p 6379 MGET $KEYLIST > /tmp/getout 2>&1
  # each MGET reply is one line "<number>_<pad>" (or empty for a nil/missing key).
  awk -v n=$NKEYS '
    { split($0, a, "_"); val[NR-1] = a[1]; seen[NR-1] = 1 }
    END{
      viol=-1; lost=0;
      for (i=0; i<n; i++) {
        e = 100000 + i;
        if (!(i in seen) || val[i] == "") { lost++; if (viol<0){ viol=i; vexp=e; vgot="MISSING" } }
        else if (val[i]+0 != e)           { lost++; if (viol<0){ viol=i; vexp=e; vgot=val[i] } }
      }
      if (viol<0) printf "HYDO-ORACLE: OK keys=%d durable\n", n;
      else        printf "HYDO-ORACLE: VIOLATION key=%d expected=%d got=%s\n", viol, vexp, vgot;
      if (lost>0) printf "hydo-redis: %d of %d acked keys LOST or wrong (a committed write vanished on the crash)\n", lost, n;
    }' /tmp/getout
  echo "hydo-redis-marker: oracle complete"
}

case "$PHASE" in
  rdiag)
    # DIAGNOSTIC: run redis in the FOREGROUND logging to the console (stdout) so we can
    # see exactly where it hangs/spins under hvx. appendonly no isolates the clock issue.
    cat > /tmp/rdiag.conf <<EOF
port 6379
bind 127.0.0.1
protected-mode no
save ""
appendonly no
daemonize no
logfile ""
loglevel debug
hz 1
dynamic-hz no
EOF
    echo "hydo-redis: FOREGROUND redis diagnostic (console log)"
    redis-server /tmp/rdiag.conf
    echo "hydo-redis: redis exited rc=$?"
    while true; do sleep 100; done
    ;;
  workload)
    # BOOT1 of a crash-recover: write the keys, then FREEZE redis (SIGSTOP) at the
    # just-acked instant so no periodic fsync commits the volatile write cache before the
    # harness extracts the durable disk. always => already fsync'd (durable); everysec =>
    # un-flushed (the documented window).
    start_redis || { echo "HYDO-ORACLE: ERROR redis start"; echo "hydo-redis-workload-done acked=0"; while true; do sleep 100; done; }
    do_workload
    kill -STOP $RPID 2>/dev/null
    echo "hydo-redis: redis STOPped (durability frozen at the acked instant)"
    echo "hydo-redis-workload-done acked=$ACKED"
    while true; do sleep 100; done
    ;;
  oracle)
    # BOOT2 of a crash-recover (fresh VM, durable disk, no fault): redis AOF-replays on
    # startup; the oracle re-reads every acked key. The MID marker (pre-redis-start) is the
    # branch mid-point so a branch reproduces the whole AOF replay + oracle.
    echo "hydo-redis: recovering from AOF"
    # If redis cannot even start, the on-disk AOF is corrupt enough that the committed data
    # is unrecoverable -- itself a durability VIOLATION (the crash lost committed writes).
    start_redis || { echo "HYDO-ORACLE: VIOLATION redis cannot recover (AOF corrupt: committed data unrecoverable)"; echo "hydo-redis-marker: oracle complete"; while true; do sleep 100; done; }
    do_oracle
    while true; do sleep 100; done
    ;;
  *)
    # SOLO: workload + oracle in ONE timeline (no crash) -> read-your-writes -> OK. Proves
    # redis + loopback client<->server + AOF work and are correct absent the power loss.
    start_redis || { echo "HYDO-ORACLE: ERROR redis start"; echo "hydo-redis-marker: oracle complete"; while true; do sleep 100; done; }
    do_workload
    do_oracle
    while true; do sleep 100; done
    ;;
esac
INIT
chmod +x build/initramfs-redis/init

reproducible_cpio build/initramfs-redis build/initramfs-redis.cpio.gz
echo "built build/initramfs-redis.cpio.gz ($(stat -c%s build/initramfs-redis.cpio.gz) bytes)"

# ---- 2i. the MILESTONE 29 ROOTFS assets (Rung 3, phases R3.4/R3.5): a REAL ext4
#      READ-ONLY root booted via switch_root (instead of the in-RAM initramfs), plus
#      a small ext2 READ-WRITE data disk. Both live on ONE MBR-partitioned virtio-blk
#      disk (build/rootfs-disk.img):
#          p1 @ LBA 2048     = ext4 ro ROOT (busybox + /sbin/init: workload + oracle)
#          p2 @ LBA 1050624  = ext2 rw DATA disk (the workload's scratch)
#      This is the "ro-root + small rw disk" model the design chose so GB-scale fits
#      L1's 4 GiB: only guest RAM + the SMALL rw disk are snapshotted; the big static
#      ro-root is excluded (re-attached identically, its hash asserted stable). See
#      hvx/hvx_rootfs_verify.c. Three byte-reproducible artifacts:
#        build/rootfs-ext4.img       the ext4 ro-root (p1), populated via mke2fs -d
#        build/rootfs-data.img       the empty ext2 rw-data (p2), via mkext2_reproducible
#        build/rootfs-disk.img       MBR + p1 + p2, assembled with dd (hand-crafted MBR)
#        build/initramfs-rootfs.cpio.gz  virtio-blk + ext4 + ext2 module stack + an
#                                    /init that mounts /dev/vda1 READ-ONLY and switch_roots.
#
# ext4 REPRODUCIBILITY (the new surface vs. the ext2 hardening): mke2fs -d populates
# the fs from a directory, and it copies each SOURCE FILE'S REAL st_ctime -- which
# `touch` cannot set and this e2fsprogs does NOT clamp (atime/mtime/crtime ARE frozen
# by libfaketime, only ctime leaks). So two builds seconds apart differ by exactly the
# per-inode ctime. Fix (proven byte-identical across rebuilds by sha256): build a
# SIMPLE-layout ext4 (no metadata_csum/64bit/flex_bg/resize_inode -> a flat, trivially
# parseable inode table and no checksums to keep consistent), then a python pass ZEROES
# i_ctime for every inode. The rest of the pinning is the SAME discipline as the ext2
# images (pinned mke2fs + libfaketime-frozen time + fixed UUID + fixed hash_seed +
# non-lazy init). The root is mounted ro,noload so it is never written at runtime.

# python3 for the ext4 ctime normalizer + the MBR writer (pinned, with fallbacks).
PY3="${HYDO_PY3:-}"
if [ -z "$PY3" ]; then
  PY3=/nix/store/14bkh67gwzg2hvqkadi03dyinrd7iybd-python3-3.11.9-env/bin/python3
  [ -x "$PY3" ] || PY3=$(command -v python3 || true)
  [ -x "$PY3" ] || PY3=$(ls /nix/store/*python3*/bin/python3 2>/dev/null | head -1 || true)
fi
if [ -z "$PY3" ] || [ ! -x "$PY3" ]; then echo "error: python3 not found (set HYDO_PY3)" >&2; exit 1; fi

# --- 2i.1 the ro-root source tree: busybox + /sbin/init (the workload + oracle). The
#     /init uses `/bin/busybox <applet>` for externals + ash builtins, so the image
#     needs only busybox itself in /bin (a maximally-deterministic, tiny dir tree). ---
rm -rf build/rootfs-root
mkdir -p build/rootfs-root/bin build/rootfs-root/sbin build/rootfs-root/proc \
         build/rootfs-root/sys build/rootfs-root/dev build/rootfs-root/data \
         build/rootfs-root/mnt build/rootfs-root/etc
cp "$BB" build/rootfs-root/bin/busybox
chmod +x build/rootfs-root/bin/busybox

cat > build/rootfs-root/sbin/init <<'RINIT'
#!/bin/busybox sh
# /sbin/init on the REAL ext4 read-only root (switch_root target). Deterministic:
# fixed keys, value a pure function of the key, no time/RNG.
BB=/bin/busybox
# /dev was moved into the new root by the initramfs; if that failed, mount a fresh one.
[ -b /dev/vda2 ] || $BB mount -t devtmpfs none /dev 2>/dev/null
$BB mount -t proc none /proc 2>/dev/null
$BB mount -t sysfs none /sys 2>/dev/null
echo ""
echo "=================================================="
echo "  hydo: switched into the REAL ext4 read-only root (M29)"
echo "  uname: $($BB uname -sr)"
echo "=================================================="
echo "hydo-rootfs-mid: real root is live"
NKEYS=32
MERR=$($BB mount -t ext2 -o rw /dev/vda2 /data 2>&1); MRC=$?
if [ $MRC -ne 0 ]; then
  echo "hydo-rootfs: mount /dev/vda2 (ext2 rw data) FAILED rc=$MRC: $MERR"
  echo "HYDO-ORACLE: ERROR mount data"
  echo "hydo-rootfs-marker: workload complete"
  echo "hydo-boot-marker: launching shell on ttyS0"
  exec $BB sh
fi
echo "hydo-rootfs: mounted ext2 /dev/vda2 rw on /data"
# ---- WORKLOAD: write NKEYS committed records to the rw DATA disk ----
k=0
while [ $k -lt $NKEYS ]; do echo "$((100000+k))" > /data/key$k; k=$((k+1)); done
$BB sync
# ---- CRASH/RECOVER: drop the page cache so the oracle reads the DEVICE (a DROP-
#      faulted physical write is invisible to a cached read) ----
echo 3 > /proc/sys/vm/drop_caches
echo "hydo-rootfs: wrote + synced $NKEYS records to /dev/vda2; dropped page cache"
# ---- ORACLE: re-read every committed record from the device ----
viol=-1; vg=""
k=0
while [ $k -lt $NKEYS ]; do
  want=$((100000+k))
  got=$($BB cat /data/key$k 2>/dev/null)
  if [ "$got" != "$want" ]; then viol=$k; vg="$got"; break; fi
  k=$((k+1))
done
if [ $viol -lt 0 ]; then
  echo "HYDO-ORACLE: OK keys=$NKEYS durable"
else
  [ -z "$vg" ] && vg=MISSING
  echo "HYDO-ORACLE: VIOLATION key=$viol expected=$((100000+viol)) got=$vg"
fi
$BB umount /data 2>/dev/null
echo "hydo-rootfs-marker: workload complete"
echo "hydo-boot-marker: launching shell on ttyS0"
exec $BB sh
RINIT
chmod +x build/rootfs-root/sbin/init
find build/rootfs-root -exec touch --no-dereference -h -d @1600000000 {} + 2>/dev/null || \
  find build/rootfs-root -exec touch -d @1600000000 {} +

# --- 2i.2 the byte-reproducible ext4 ro-root image (p1) ---
ROOTFS_EXT4_MIB=512
ROOTFS_DATA_MIB=16
mkext4_rootfs_reproducible() {   # $1=out  $2=srcdir  $3=size_MiB  $4=uuid  $5=hash_seed
  local out="$1" src="$2" mib="$3" uuid="$4" seed="$5"
  : > "$out"; truncate -s $(( mib * 1024 * 1024 )) "$out"
  SOURCE_DATE_EPOCH=1600000000 "$FAKETIME_BIN" "$FAKE_TS" "$MKE2FS" -q -F -t ext4 -b 4096 \
    -O ^metadata_csum,^64bit,^flex_bg,^resize_inode \
    -U "$uuid" \
    -E hash_seed="$seed",lazy_itable_init=0,lazy_journal_init=0 \
    -d "$src" "$out"
  # Zero i_ctime for every inode (the only field mke2fs -d leaves non-reproducible).
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
    itbl = u32(gdtb * bs + g * 32 + 0x08)      # bg_inode_table (32-byte GDT, no 64bit)
    base = g * ipg
    for i in range(ipg):
        if base + i >= inodes:
            break
        struct.pack_into("<I", mm, itbl * bs + i * isize + 0x0C, 0)   # i_ctime := 0
mm.flush(); mm.close(); f.close()
PYEOF
}
mkext4_rootfs_reproducible build/rootfs-ext4.img build/rootfs-root $ROOTFS_EXT4_MIB \
  aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee 12344321-5678-8765-9abc-cba987654321
echo "built build/rootfs-ext4.img ($(stat -c%s build/rootfs-ext4.img) bytes, reproducible ext4 ro-root) via $MKE2FS + faketime + ctime-normalize"

# --- 2i.3 the small ext2 rw DATA disk (p2), same pinned reproducible recipe ---
mkext2_reproducible build/rootfs-data.img \
  dddddddd-eeee-ffff-0000-111111111111 55554444-3333-2222-1111-000000000000
# mkext2_reproducible sizes the image to 16 MiB; that IS ROOTFS_DATA_MIB.
echo "built build/rootfs-data.img ($(stat -c%s build/rootfs-data.img) bytes, reproducible ext2 rw-data)"

# --- 2i.4 assemble the MBR-partitioned disk (hand-crafted MBR => no partitioning tool
#     to drift; only the LBA/size fields matter to Linux, CHS is the 0xFE 0xFF 0xFF
#     "use-LBA" sentinel). ---
P1_LBA=2048
P1_SECTORS=$(( ROOTFS_EXT4_MIB * 1024 * 1024 / 512 ))
P2_SECTORS=$(( ROOTFS_DATA_MIB * 1024 * 1024 / 512 ))
P2_LBA=$(( P1_LBA + P1_SECTORS ))
DISK_SECTORS=$(( P2_LBA + P2_SECTORS ))
: > build/rootfs-disk.img
truncate -s $(( DISK_SECTORS * 512 )) build/rootfs-disk.img
"$PY3" - build/rootfs-disk.img "$P1_LBA" "$P1_SECTORS" "$P2_LBA" "$P2_SECTORS" <<'PYEOF'
import sys, struct
img, p1s, p1n, p2s, p2n = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
mbr = bytearray(512)
mbr[0x1B8:0x1BC] = b"\x0d\xd0\xea\x1d"          # fixed disk signature (deterministic)
def part(off, lba, n, ptype=0x83):
    mbr[off] = 0x00                             # not bootable
    mbr[off + 1:off + 4] = b"\xfe\xff\xff"      # CHS start (LBA sentinel)
    mbr[off + 4] = ptype                        # type: Linux
    mbr[off + 5:off + 8] = b"\xfe\xff\xff"      # CHS end
    struct.pack_into("<I", mbr, off + 8, lba)   # LBA first sector
    struct.pack_into("<I", mbr, off + 12, n)    # sector count
part(0x1BE, p1s, p1n)                           # partition 1: ext4 ro root
part(0x1CE, p2s, p2n)                           # partition 2: ext2 rw data
mbr[0x1FE] = 0x55; mbr[0x1FF] = 0xAA            # boot signature
with open(img, "r+b") as f:
    f.write(mbr)
PYEOF
dd if=build/rootfs-ext4.img of=build/rootfs-disk.img bs=512 seek=$P1_LBA conv=notrunc status=none
dd if=build/rootfs-data.img of=build/rootfs-disk.img bs=512 seek=$P2_LBA conv=notrunc status=none
echo "built build/rootfs-disk.img ($(stat -c%s build/rootfs-disk.img) bytes: MBR + p1 ext4 ro-root @ LBA $P1_LBA + p2 ext2 rw-data @ LBA $P2_LBA)"
echo "  sha256(rootfs-ext4.img)=$(sha256sum build/rootfs-ext4.img | cut -d' ' -f1)"
echo "  sha256(rootfs-disk.img)=$(sha256sum build/rootfs-disk.img | cut -d' ' -f1)"

# --- 2i.5 the tiny switch_root initramfs: load the virtio-blk + ext4 + ext2 module
#     stack, mount /dev/vda1 (ext4) READ-ONLY, and switch_root into it. ---
# crc32c_generic registers the "crc32c" crypto shash that libcrc32c/jbd2/ext4 need at
# MOUNT time (without it: "EXT4-fs (vda1): Cannot load crc32c driver"); there is no
# module autoloader in the initramfs, so it must be insmod'd explicitly.
ROOTMODS="virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk crc16 crc32c_generic mbcache jbd2 ext4 ext2"
rm -rf build/initramfs-rootfs
mkdir -p build/initramfs-rootfs/bin build/initramfs-rootfs/proc build/initramfs-rootfs/sys \
         build/initramfs-rootfs/dev build/initramfs-rootfs/newroot build/initramfs-rootfs/lib/modules
cp "$BB" build/initramfs-rootfs/bin/busybox
chmod +x build/initramfs-rootfs/bin/busybox
for m in $ROOTMODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  if [ -z "$src" ]; then echo "note: rootfs module $m.ko.xz not found under $MODROOT (built-in?)" >&2; continue; fi
  xz -dc "$src" > "build/initramfs-rootfs/lib/modules/$m.ko"
done
echo "staged modules for the rootfs initramfs: $ROOTMODS"

cat > build/initramfs-rootfs/init <<'RINITRAMFS'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
echo ""
echo "=================================================="
echo "  hydo: initramfs reached (M29 ext4 ro-root + switch_root)"
echo "=================================================="
ROOT=/dev/vda1
for tok in $(cat /proc/cmdline); do case "$tok" in root=*) ROOT=${tok#root=};; esac; done
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk crc16 crc32c_generic mbcache jbd2 ext4 ext2; do
  [ -f /lib/modules/$m.ko ] || continue
  insmod /lib/modules/$m.ko 2>/dev/null && echo "hydo-rootfs: insmod $m ok" || echo "hydo-rootfs: insmod $m (skipped/failed)"
done
# devtmpfs creates /dev/vda1 when virtio_blk binds + the partition scan runs.
n=0
while [ ! -b "$ROOT" ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
if [ ! -b "$ROOT" ]; then
  echo "hydo-rootfs: root device $ROOT MISSING (virtio-blk did not bind / no partition)"
  echo "HYDO-ORACLE: ERROR no root device"
  echo "hydo-rootfs-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"
  exec /bin/busybox sh
fi
mkdir -p /newroot
# ro,noload: the offline-built ext4 has a clean journal, so a ro mount writes NOTHING
# to the device -- the ro-root stays byte-stable across the whole boot.
if ! mount -t ext4 -o ro,noload "$ROOT" /newroot 2>/dev/null; then
  echo "hydo-rootfs: mount $ROOT (ext4 ro) FAILED"
  echo "HYDO-ORACLE: ERROR mount root"
  echo "hydo-rootfs-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"
  exec /bin/busybox sh
fi
echo "hydo-rootfs: mounted $ROOT ext4 read-only at /newroot; switching root"
# carry the working devtmpfs (with /dev/vda2) into the new root so the real /sbin/init
# can mount the rw data disk; fall back to a fresh devtmpfs mount over there if needed.
mkdir -p /newroot/dev /newroot/proc /newroot/sys
mount --move /dev /newroot/dev 2>/dev/null || mount -o move /dev /newroot/dev 2>/dev/null || \
  mount -t devtmpfs none /newroot/dev 2>/dev/null || true
exec switch_root /newroot /sbin/init
RINITRAMFS
chmod +x build/initramfs-rootfs/init

reproducible_cpio build/initramfs-rootfs build/initramfs-rootfs.cpio.gz
echo "built build/initramfs-rootfs.cpio.gz ($(stat -c%s build/initramfs-rootfs.cpio.gz) bytes)"

# ---- 2j. the MILESTONE 30 MULTI-SERVICE SYSTEM assets (Rung 4): TWO real services
#      co-located in ONE L2 guest, talking over the in-kernel 127.0.0.1 LOOPBACK, plus a
#      client -- so a seeded durability fault surfaces a SYSTEM-LEVEL (cross-service) bug:
#
#          client (hydoapp load/verify)  --loopback:8080-->  hydoapp server (app tier)
#                                                            --loopback:6379-->  redis-server
#
#      The app tier (build/hydoapp) is a REAL second service backed by redis over loopback:
#      LOAD writes led:0..N-1 into redis and ACKs only after redis +OKs all N; VERIFY reads
#      them back and computes an AGGREGATE (sum + missing). The SYSTEM invariant: the app's
#      recomputed aggregate == what the client committed. Under Redis's everysec window a
#      committed write is lost on a power loss, so the app returns a WRONG answer -- the app
#      and the client's committed history DISAGREE (vs M27, whose invariant was Redis's OWN
#      data with a direct redis-cli client). All ADDITIVE: it reuses the M27 redis binaries +
#      the flush-aware fault + the idle heartbeat, and adds new artifacts, so §2h (redis) is
#      byte-untouched and the M27 baselines are preserved.
#        build/hydoapp                    static musl app-server+client (deterministic)
#        build/system-disk.img            a byte-reproducible EMPTY ext2 image (redis AOF)
#        build/initramfs-system.cpio.gz   virtio-blk + ext2 stack + redis + hydoapp + seedrng
#                                         + an /init that wires client->app->redis per phase.
#      256 MiB guest is ample (redis 3.4 MB + hydoapp + a 48-key dataset + AOF on a 16 MiB
#      disk), well under HVX_DISK_MAX and L1's RAM ceiling. --

# The app tier: static + reproducible with the SAME pinned musl gcc as hydokv/seedrng (§2e).
"$GCC_MUSL" -static -O2 -Wall -B "${MUSL_ROOT%/}/lib" hvx/hydoapp.c -o build/hydoapp
echo "built build/hydoapp ($(stat -c%s build/hydoapp) bytes, static musl) via $GCC_MUSL"

# A BYTE-REPRODUCIBLE empty ext2 image for the AOF (distinct UUID/hash_seed from the redis
# disk so §2h stays byte-identical). Same pinned mke2fs + faketime recipe, PLUS a python pass
# that ZEROES every superblock + inode time field. WHY the extra pass: libfaketime (no `@`
# freeze) advances its faked clock in real time, so mke2fs's time() calls round to the start
# second ONLY if the run stays inside it; the existing images (redis/sql/rootfs) complete fast
# enough to always land there, but this image's metadata layout put mke2fs near a second
# boundary and two full builds landed on DIFFERENT seconds (f92e.. vs 7e22..). Zeroing the
# time fields makes the image a pure function of structure + the pinned UUID/hash_seed,
# robustly byte-identical across sessions -- the same technique as the M29 ext4 ctime
# normalizer. This is a NEW image (no prior baseline), and it touches ONLY system-disk.img,
# so mkext2_reproducible and every existing fs image stay byte-identical.
mkext2_reproducible build/system-disk.img \
  90909090-8080-7070-6060-505050505050 a1a1a1a1-b2b2-c3c3-d4d4-e5e5e5e5e5e5
"$PY3" - build/system-disk.img <<'PYEOF'
import sys, struct, mmap
f = open(sys.argv[1], "r+b"); mm = mmap.mmap(f.fileno(), 0)
u16 = lambda o: struct.unpack_from("<H", mm, o)[0]
u32 = lambda o: struct.unpack_from("<I", mm, o)[0]
sb = 1024
inodes = u32(sb + 0x00); bs = 1024 << u32(sb + 0x18)
ipg = u32(sb + 0x28); isize = u16(sb + 0x58) or 128
for o in (0x2C, 0x30, 0x40, 0x108):                 # s_mtime s_wtime s_lastcheck s_mkfs_time
    struct.pack_into("<I", mm, sb + o, 0)
gdtb = 2 if bs == 1024 else 1
ng = (inodes + ipg - 1) // ipg
for g in range(ng):
    itbl = u32(gdtb * bs + g * 32 + 0x08)           # bg_inode_table (32-byte GDT, no 64bit)
    base = g * ipg
    for i in range(ipg):
        if base + i >= inodes:
            break
        io = itbl * bs + i * isize
        for o in (0x08, 0x0C, 0x10, 0x14):          # i_atime i_ctime i_mtime i_dtime
            struct.pack_into("<I", mm, io + o, 0)
mm.flush(); mm.close(); f.close()
PYEOF
echo "built build/system-disk.img ($(stat -c%s build/system-disk.img) bytes, reproducible ext2, time-normalized) via $MKE2FS + faketime + zero-times"

# The system initramfs: virtio-blk + ext2 module stack + redis + hydoapp + seedrng + /init.
SYSMODS="$SQLMODS"       # same virtio-blk + ext2 stack as the SQL/redis initramfs
rm -rf build/initramfs-system
mkdir -p build/initramfs-system/bin build/initramfs-system/proc build/initramfs-system/sys \
         build/initramfs-system/dev build/initramfs-system/mnt build/initramfs-system/tmp \
         build/initramfs-system/lib/modules
cp "$BB" build/initramfs-system/bin/busybox
chmod +x build/initramfs-system/bin/busybox
cp build/redis-server build/initramfs-system/bin/redis-server
cp build/redis-cli    build/initramfs-system/bin/redis-cli
cp build/hydoapp      build/initramfs-system/bin/hydoapp
cp build/hvx_seedrng  build/initramfs-system/bin/hvx_seedrng
chmod +x build/initramfs-system/bin/redis-server build/initramfs-system/bin/redis-cli \
         build/initramfs-system/bin/hydoapp build/initramfs-system/bin/hvx_seedrng
for m in $SYSMODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  if [ -z "$src" ]; then echo "error: module $m.ko.xz not found under $MODROOT" >&2; exit 1; fi
  xz -dc "$src" > "build/initramfs-system/lib/modules/$m.ko"
done
echo "staged modules for the system initramfs: $SYSMODS"

cat > build/initramfs-system/init <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
mkdir -p /tmp /mnt
# ---- parse the kernel cmdline: which phase, which redis fsync policy ----
PHASE=solo
FSYNC=everysec
NKEYS=48
for tok in $(cat /proc/cmdline); do
  case "$tok" in
    hydo_phase=*) PHASE=${tok#hydo_phase=} ;;
    hydo_fsync=*) FSYNC=${tok#hydo_fsync=} ;;
  esac
done
# committed aggregate the CLIENT expects = sum_{k=0}^{N-1}(100000+k)
EXP=$(( NKEYS*100000 + NKEYS*(NKEYS-1)/2 ))
echo ""
echo "=================================================="
echo "  hydo: initramfs reached (M30 Rung 4: multi-service system over loopback)"
echo "  client -> hydoapp:8080 -> redis:6379   phase=$PHASE fsync=$FSYNC keys=$NKEYS exp=$EXP"
echo "=================================================="
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk mbcache ext2; do
  insmod /lib/modules/$m.ko 2>/dev/null && echo "hydo-sys: insmod $m ok" || echo "hydo-sys: insmod $m (skipped/failed)"
done
# ---- bring up the in-kernel loopback (deterministic; no virtio-net needed) ----
ifconfig lo 127.0.0.1 netmask 255.0.0.0 up 2>/dev/null
ip link set lo up 2>/dev/null
echo "hydo-sys: loopback -> $(ifconfig lo 2>/dev/null | grep -o 'inet addr:[0-9.]*' | head -1)"
n=0
while [ ! -b /dev/vda ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
if [ ! -b /dev/vda ]; then
  echo "hydo-sys: /dev/vda MISSING (virtio-blk did not bind)"
  echo "HYDO-ORACLE: ERROR no device"; echo "hydo-system-marker: oracle complete"
  while true; do sleep 100; done
fi
if ! mount -t ext2 -o rw /dev/vda /mnt 2>/dev/null; then
  echo "hydo-sys: mount /dev/vda (ext2) FAILED"
  echo "HYDO-ORACLE: ERROR mount failed"; echo "hydo-system-marker: oracle complete"
  while true; do sleep 100; done
fi
echo "hydo-sys: mounted ext2 /dev/vda on /mnt"
# ---- seed the CRNG deterministically so redis's blocking getrandom() does not spin on a
#      deterministic hypervisor's jitter-free TSC (M27). ----
hvx_seedrng || echo "hydo-sys: hvx_seedrng FAILED (redis may hang on getrandom)"

# ---- redis config: AOF on the ext2 fs; loopback only; NO RDB, NO auto-rewrite; the app is
#      the ONLY writer, appendfsync from the cmdline (always | everysec). ----
cat > /tmp/redis.conf <<EOF
port 6379
bind 127.0.0.1
protected-mode no
timeout 0
dir /mnt
save ""
appendonly yes
appendfsync $FSYNC
appenddirname appendonlydir
appendfilename appendonly.aof
auto-aof-rewrite-percentage 0
aof-load-truncated yes
aof-timestamp-enabled no
daemonize no
logfile /tmp/redis.log
loglevel notice
hz 1
dynamic-hz no
EOF

start_redis() {   # service B
  redis-server /tmp/redis.conf >/tmp/redis.out 2>&1 &
  RPID=$!
  sleep 3
  n=0
  while [ $n -lt 20 ]; do
    if redis-cli -p 6379 ping 2>/dev/null | grep -q PONG; then
      echo "hydo-sys: redis ready after $n polls"; return 0
    fi
    n=$((n+1)); sleep 1
  done
  echo "hydo-sys: redis NOT ready; redis.log:"; tail -12 /tmp/redis.log 2>/dev/null
  return 1
}

start_app() {     # service A (the app tier), backed by redis over loopback
  hydoapp server >/tmp/app.out 2>&1 &
  APID=$!
  n=0
  while [ $n -lt 20 ]; do
    if hydoapp ping 2>/dev/null | grep -q PONG; then
      echo "hydo-sys: app tier ready after $n polls (client<->app<->redis over 127.0.0.1)"; return 0
    fi
    n=$((n+1)); sleep 1
  done
  echo "hydo-sys: app NOT ready; app.out:"; tail -8 /tmp/app.out 2>/dev/null
  return 1
}

do_load() {       # the CLIENT commits NKEYS values THROUGH the app (app -> redis SETs)
  R=$(hydoapp load $NKEYS 2>/dev/null)
  echo "hydo-sys: client LOAD reply: |$R|"
  ACKED=$(echo "$R" | sed -n 's/.*count=\([0-9]*\).*/\1/p')
  [ -z "$ACKED" ] && ACKED=0
  echo "hydo-sys: client committed acked=$ACKED / $NKEYS through the app over 127.0.0.1"
}

do_verify() {     # the SYSTEM oracle: client asks the app to recompute the aggregate from
                  # redis and compares it to what the client committed (cross-service check).
  R=$(hydoapp verify $NKEYS 2>/dev/null)
  echo "hydo-sys: client VERIFY reply: |$R|"
  case "$R" in
    SUM\ *)
      T=$(echo "$R" | sed -n 's/.*total=\([0-9-]*\).*/\1/p')
      M=$(echo "$R" | sed -n 's/.*missing=\([0-9-]*\).*/\1/p')
      FM=$(echo "$R" | sed -n 's/.*firstmiss=\([0-9-]*\).*/\1/p')
      [ -z "$T" ] && T=-1; [ -z "$M" ] && M=-1; [ -z "$FM" ] && FM=-1
      if [ "$M" -gt 0 ] 2>/dev/null; then
        echo "HYDO-ORACLE: VIOLATION key=$FM expected=$((100000+FM)) got=MISSING"
        echo "hydo-sys: the app's aggregate DISAGREES with the client's committed history ($M of $NKEYS committed writes vanished on the crash)"
      elif [ "$T" != "$EXP" ]; then
        echo "HYDO-ORACLE: VIOLATION aggregate expected=$EXP got=$T"
        echo "hydo-sys: the app recomputed a WRONG aggregate from redis (a committed value was corrupted)"
      else
        echo "HYDO-ORACLE: OK keys=$NKEYS durable aggregate=$T"
      fi ;;
    *)
      # app could not read redis (redis refused a corrupt AOF -> committed data unrecoverable)
      echo "HYDO-ORACLE: VIOLATION system cannot recover (app<->redis unrecoverable: $R)" ;;
  esac
  echo "hydo-system-marker: oracle complete"
}

case "$PHASE" in
  workload)
    # BOOT1 of a crash-recover: bring up the whole system, the client commits NKEYS values
    # through the app, then FREEZE redis (SIGSTOP) at the just-acked instant so no periodic
    # fsync commits the volatile write cache before the harness extracts the durable disk.
    start_redis || { echo "HYDO-ORACLE: ERROR redis start"; echo "hydo-system-workload-done acked=0"; while true; do sleep 100; done; }
    start_app   || { echo "HYDO-ORACLE: ERROR app start";   echo "hydo-system-workload-done acked=0"; while true; do sleep 100; done; }
    do_load
    kill -STOP $RPID 2>/dev/null
    echo "hydo-sys: redis STOPped (durability frozen at the acked instant)"
    echo "hydo-system-workload-done acked=$ACKED"
    while true; do sleep 100; done
    ;;
  oracle)
    # BOOT2 of a crash-recover (fresh VM, durable disk, no fault): redis AOF-replays on
    # startup; the app tier comes up; the client re-verifies the aggregate through the app.
    echo "hydo-system: recovering"
    start_redis || { echo "HYDO-ORACLE: VIOLATION system cannot recover (redis refused the AOF: committed data unrecoverable)"; echo "hydo-system-marker: oracle complete"; while true; do sleep 100; done; }
    start_app   || { echo "HYDO-ORACLE: ERROR app start"; echo "hydo-system-marker: oracle complete"; while true; do sleep 100; done; }
    do_verify
    while true; do sleep 100; done
    ;;
  *)
    # SOLO: bring up the whole system, commit + verify in ONE timeline (no crash) -> OK.
    # Proves client<->app<->redis over loopback + AOF work and are correct absent a power loss.
    start_redis || { echo "HYDO-ORACLE: ERROR redis start"; echo "hydo-system-marker: oracle complete"; while true; do sleep 100; done; }
    start_app   || { echo "HYDO-ORACLE: ERROR app start";   echo "hydo-system-marker: oracle complete"; while true; do sleep 100; done; }
    do_load
    do_verify
    while true; do sleep 100; done
    ;;
esac
INIT
chmod +x build/initramfs-system/init

reproducible_cpio build/initramfs-system build/initramfs-system.cpio.gz
echo "built build/initramfs-system.cpio.gz ($(stat -c%s build/initramfs-system.cpio.gz) bytes)"

# ---- 2k. the MILESTONE 31 (Rung 5) JVM assets: a REAL, unmodified OpenJDK JRE and a
#      real multi-threaded Java workload on the M29 ext4 READ-ONLY root, run
#      DETERMINISTICALLY under hvx. Reuses the M29 ro-root + small-rw-disk model (the
#      big JRE closure lives on the ro-root, which is EXCLUDED from the snapshot, so a
#      hundreds-of-MiB JRE does not bloat the fingerprint). Byte-reproducible: pinned
#      JRE/JDK store paths + the M29 mke2fs+libfaketime+ctime-zero discipline.
#        build/HydoWork.class          the compiled workload (no timestamp -> stable)
#        build/rootfs-jvm-root/        ro-root tree: busybox + hvx_seedrng + /app + the
#                                      FULL JRE closure under /nix/store (ld.so eagerly
#                                      loads bin/java's gtk/cups/X DT_NEEDED, so the
#                                      whole closure is required, not a trimmed subset)
#        build/jvm-ext4.img            ext4 ro-root (p1)
#        build/jvm-data.img            ext2 rw-data (p2, the fault target)
#        build/jvm-disk.img            MBR + p1 + p2 (hand-crafted MBR, like M29)
#        build/initramfs-jvm.cpio.gz   virtio-blk + ext4 + ext2 stack + switch_root
#      Guest RAM 640 MiB, JVM heap fixed at 128 MiB (-Xms==-Xmx), interpreter-only
#      (-Xint: no JIT/compiler threads), single-threaded SerialGC, perfdata off. --

# The RUNTIME JRE staged onto the ro-root (unmodified upstream openjdk-minimal-jre-17):
# PIN the store path with a `nix build` fallback (same discipline as busybox/sqlite/redis).
JRE_MIN="${HYDO_JRE:-}"
if [ -z "$JRE_MIN" ]; then
  JRE_MIN=/nix/store/0g011k1pyfkbh0sj182mh70d19f168zk-openjdk-minimal-jre-17.0.7+9
  [ -x "$JRE_MIN/bin/java" ] || JRE_MIN=/nix/store/0g011k1pyfkbh0sj182mh70d19f168zk-openjdk-minimal-jre-17.0.7+7
  if [ ! -x "$JRE_MIN/bin/java" ]; then
    P=$(nix build --no-link --print-out-paths "nixpkgs#jre17_minimal" 2>/dev/null | head -1 || true)
    [ -n "$P" ] && [ -x "$P/bin/java" ] && JRE_MIN="$P"
  fi
fi
if [ -z "$JRE_MIN" ] || [ ! -x "$JRE_MIN/bin/java" ]; then
  echo "error: no minimal JRE found (set HYDO_JRE=/path/to/jre with bin/java)" >&2; exit 1
fi
# The BUILD-TIME javac (headless JDK 17) to compile HydoWork.java -> a .class (no jar,
# no embedded timestamp -> byte-reproducible). Not staged into the rootfs.
JAVAC="${HYDO_JAVAC:-}"
if [ -z "$JAVAC" ]; then
  JAVAC=/nix/store/vjcrra860yal7fzk1m81ddirrafqrccz-openjdk-headless-17.0.7+7/bin/javac
  if [ ! -x "$JAVAC" ]; then
    P=$(nix build --no-link --print-out-paths "nixpkgs#jdk17_headless" 2>/dev/null | head -1 || true)
    [ -n "$P" ] && [ -x "$P/bin/javac" ] && JAVAC="$P/bin/javac"
    [ -x "$JAVAC" ] || JAVAC=$(ls /nix/store/*openjdk-headless-17*/bin/javac 2>/dev/null | head -1 || true)
  fi
fi
if [ -z "$JAVAC" ] || [ ! -x "$JAVAC" ]; then
  echo "error: no javac 17 found (set HYDO_JAVAC=/path/to/javac)" >&2; exit 1
fi

# --- 2k.1 compile the workload (deterministic: fixed source + fixed javac; a bare
#     .class carries no timestamp). Compile in a temp dir, stage the .class(es). ---
rm -rf build/jvm-app; mkdir -p build/jvm-app
"$JAVAC" -d build/jvm-app hvx/HydoWork.java
echo "compiled HydoWork.java -> $(ls build/jvm-app/*.class | tr '\n' ' ')(via $JAVAC)"
cp build/jvm-app/HydoWork.class build/HydoWork.class

# --- 2k.2 the ro-root source tree: busybox + hvx_seedrng + /app + the FULL JRE closure
#     replicated under /nix/store so bin/java's absolute ELF-interp + RPATHs resolve. ---
# The prior build's copied nix-store closure has read-only (0555) dirs, so a plain
# `rm -rf` cannot traverse them; make it writable first so a rebuild is idempotent.
chmod -R u+w build/rootfs-jvm-root 2>/dev/null || true
rm -rf build/rootfs-jvm-root
mkdir -p build/rootfs-jvm-root/bin build/rootfs-jvm-root/sbin build/rootfs-jvm-root/proc \
         build/rootfs-jvm-root/sys build/rootfs-jvm-root/dev build/rootfs-jvm-root/data \
         build/rootfs-jvm-root/tmp build/rootfs-jvm-root/etc build/rootfs-jvm-root/app \
         build/rootfs-jvm-root/nix/store
cp "$BB" build/rootfs-jvm-root/bin/busybox
chmod +x build/rootfs-jvm-root/bin/busybox
cp build/hvx_seedrng build/rootfs-jvm-root/bin/hvx_seedrng   # M27 CRNG seeder (built in §2h)
chmod +x build/rootfs-jvm-root/bin/hvx_seedrng
cp build/jvm-app/*.class build/rootfs-jvm-root/app/

# Replicate the JRE's full runtime closure under the rootfs /nix/store (verbatim, so the
# patchelf'd absolute /nix/store RPATHs + ELF interpreter resolve with zero patching).
echo "staging JRE closure ($(nix-store -qR "$JRE_MIN" | wc -l) store paths) onto the ro-root..."
for p in $(nix-store -qR "$JRE_MIN"); do
  cp -a "$p" build/rootfs-jvm-root/nix/store/ 2>/dev/null || true
done
JRE_CLOSURE_MIB=$(du -sm build/rootfs-jvm-root/nix/store 2>/dev/null | cut -f1)
echo "staged JRE closure: ${JRE_CLOSURE_MIB} MiB under build/rootfs-jvm-root/nix/store"

# Bake the java path (a pure function of the pinned JRE store path) for /sbin/init.
echo "JAVA=$JRE_MIN/bin/java" > build/rootfs-jvm-root/etc/jvm-env

# /sbin/init on the JVM ro-root: seed the CRNG (M27), mount the ext2 rw data disk, run a
# REAL JVM deterministically (interpreter, SerialGC, fixed heap), then the M29-style oracle.
cat > build/rootfs-jvm-root/sbin/init <<'JINIT'
#!/bin/busybox sh
BB=/bin/busybox
[ -c /dev/null ] || $BB mount -t devtmpfs none /dev 2>/dev/null
$BB mount -t proc none /proc 2>/dev/null
$BB mount -t sysfs none /sys 2>/dev/null
$BB mount -t tmpfs none /tmp 2>/dev/null
. /etc/jvm-env      # sets JAVA=/nix/store/.../bin/java
echo ""
echo "=================================================="
echo "  hydo: switched into the ext4 ro-root (M31 JVM / Rung 5)"
echo "  uname: $($BB uname -sr)"
echo "=================================================="
echo "hydo-jvm-mid: real root is live"
# M27: credit deterministic entropy so SecureRandom/getrandom returns instead of
# spinning on the jitter-free deterministic TSC.
/bin/hvx_seedrng 2>/dev/null || echo "hydo-jvm: hvx_seedrng missing/failed"
NKEYS=32
MERR=$($BB mount -t ext2 -o rw /dev/vda2 /data 2>&1); MRC=$?
if [ $MRC -ne 0 ]; then
  echo "HYDO-ORACLE: ERROR mount data ($MERR)"
  echo "hydo-jvm-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"; exec $BB sh
fi
echo "hydo-jvm: mounted ext2 /dev/vda2 rw on /data"
# M32 Part 1 (JIT-determinism experiment): select the HotSpot compiler config from the
# kernel cmdline token hydo_jit=<config>. Default (absent/unknown -> int) reproduces the
# M31 -Xint (interpreter-only) config. The other configs ENABLE the JIT to test whether it
# stays bit-for-bit deterministic under hvx (and how much faster it is):
#   int        -Xint                                      (M31 baseline: interpreter, no JIT)
#   c1         -XX:TieredStopAtLevel=1                    (C1 only, background compiler threads)
#   c1sync     -XX:TieredStopAtLevel=1 -XX:CICompilerCount=1 -XX:-BackgroundCompilation
#                                                         (C1 only, ONE synchronous compiler)
#   tiered     (none)                                     (default C1+C2 tiered, background)
#   tieredsync -XX:-BackgroundCompilation                 (C1+C2 tiered, synchronous compiles)
JIT=int
for tok in $($BB cat /proc/cmdline 2>/dev/null); do
  case "$tok" in hydo_jit=*) JIT="${tok#hydo_jit=}";; esac
done
case "$JIT" in
  int)        JITFLAGS="-Xint";;
  c1)         JITFLAGS="-XX:TieredStopAtLevel=1";;
  c1sync)     JITFLAGS="-XX:TieredStopAtLevel=1 -XX:CICompilerCount=1 -XX:-BackgroundCompilation";;
  tiered)     JITFLAGS="";;
  tieredsync) JITFLAGS="-XX:-BackgroundCompilation";;
  *)          JIT=int; JITFLAGS="-Xint";;
esac
echo "hydo-jvm: JIT config = $JIT (flags: $JITFLAGS)"
echo "hydo-jvm: launching REAL JVM ($JAVA): jit=$JIT SerialGC fixed-heap deterministic"
$JAVA \
  $JITFLAGS -XX:+UseSerialGC -XX:-UsePerfData -XX:-UseContainerSupport \
  -XX:ActiveProcessorCount=1 -Xshare:off -Xms64m -Xmx64m \
  -Djava.security.egd=file:/dev/urandom \
  -Duser.name=hydo -Duser.home=/tmp -Duser.timezone=UTC \
  -Duser.language=en -Duser.country=US -Dfile.encoding=UTF-8 -Djava.io.tmpdir=/tmp \
  -cp /app HydoWork /data > /tmp/jvm.log 2>&1
JRC=$?
echo "hydo-jvm: java exited rc=$JRC"
$BB grep -a 'HYDO-JVM' /tmp/jvm.log 2>/dev/null || { echo "hydo-jvm: no HYDO-JVM line; jvm.log tail:"; $BB tail -20 /tmp/jvm.log 2>/dev/null; }
$BB sync
echo 3 > /proc/sys/vm/drop_caches
# ORACLE: re-read every committed record from the DEVICE (a DROP-faulted write is
# invisible to a cached read, so drop the cache first -- the M25/M26/M29 discipline).
viol=-1; vg=""
k=0
while [ $k -lt $NKEYS ]; do
  want=$((100000+k)); got=$($BB cat /data/key$k 2>/dev/null)
  if [ "$got" != "$want" ]; then viol=$k; vg="$got"; break; fi
  k=$((k+1))
done
if [ $viol -lt 0 ]; then
  echo "HYDO-ORACLE: OK keys=$NKEYS durable jvm_rc=$JRC"
else
  [ -z "$vg" ] && vg=MISSING
  echo "HYDO-ORACLE: VIOLATION key=$viol expected=$((100000+viol)) got=$vg"
fi
$BB umount /data 2>/dev/null
echo "hydo-jvm-marker: workload complete"
echo "hydo-boot-marker: launching shell on ttyS0"
exec $BB sh
JINIT
chmod +x build/rootfs-jvm-root/sbin/init
# Freeze mtimes across the (build-user-owned) tree; nix store copies are owned by the
# builder so utime() succeeds even on mode-0444 files. ctime is zeroed by the ext4 pass.
find build/rootfs-jvm-root -exec touch --no-dereference -h -d @1600000000 {} + 2>/dev/null || true

# --- 2k.3 the byte-reproducible ext4 ro-root image (p1). Sized to hold the ~600 MiB JRE
#     closure + slack; reuses the M29 mkext4_rootfs_reproducible (pinned mke2fs +
#     libfaketime-frozen times + fixed UUID/hash_seed + non-lazy init + i_ctime zeroing). ---
JVM_EXT4_MIB=720
JVM_DATA_MIB=16
: > build/jvm-ext4.img; truncate -s $(( JVM_EXT4_MIB * 1024 * 1024 )) build/jvm-ext4.img
mkext4_rootfs_reproducible build/jvm-ext4.img build/rootfs-jvm-root $JVM_EXT4_MIB \
  bbbbcccc-dddd-eeee-ffff-000011112222 abcdef01-2345-6789-abcd-ef0123456789
echo "built build/jvm-ext4.img ($(stat -c%s build/jvm-ext4.img) bytes, reproducible ext4 JVM ro-root)"

# --- 2k.4 the small ext2 rw DATA disk (p2), same pinned recipe (distinct UUID/hash_seed) ---
mkext2_reproducible build/jvm-data.img \
  cccc0000-1111-2222-3333-444455556666 66665555-4444-3333-2222-111100000000
# mkext2_reproducible sizes to 16 MiB (== JVM_DATA_MIB).
echo "built build/jvm-data.img ($(stat -c%s build/jvm-data.img) bytes, reproducible ext2 rw-data)"

# --- 2k.5 assemble the MBR-partitioned disk (hand-crafted MBR, like M29 §2i.4) ---
JP1_LBA=2048
JP1_SECTORS=$(( JVM_EXT4_MIB * 1024 * 1024 / 512 ))
JP2_SECTORS=$(( JVM_DATA_MIB * 1024 * 1024 / 512 ))
JP2_LBA=$(( JP1_LBA + JP1_SECTORS ))
JDISK_SECTORS=$(( JP2_LBA + JP2_SECTORS ))
: > build/jvm-disk.img
truncate -s $(( JDISK_SECTORS * 512 )) build/jvm-disk.img
"$PY3" - build/jvm-disk.img "$JP1_LBA" "$JP1_SECTORS" "$JP2_LBA" "$JP2_SECTORS" <<'PYEOF'
import sys, struct
img, p1s, p1n, p2s, p2n = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
mbr = bytearray(512)
mbr[0x1B8:0x1BC] = b"\x0d\xd0\xea\x1d"
def part(off, lba, n, ptype=0x83):
    mbr[off] = 0x00
    mbr[off + 1:off + 4] = b"\xfe\xff\xff"
    mbr[off + 4] = ptype
    mbr[off + 5:off + 8] = b"\xfe\xff\xff"
    struct.pack_into("<I", mbr, off + 8, lba)
    struct.pack_into("<I", mbr, off + 12, n)
part(0x1BE, p1s, p1n)
part(0x1CE, p2s, p2n)
mbr[0x1FE] = 0x55; mbr[0x1FF] = 0xAA
with open(img, "r+b") as f:
    f.write(mbr)
PYEOF
dd if=build/jvm-ext4.img of=build/jvm-disk.img bs=512 seek=$JP1_LBA conv=notrunc status=none
dd if=build/jvm-data.img of=build/jvm-disk.img bs=512 seek=$JP2_LBA conv=notrunc status=none
echo "built build/jvm-disk.img ($(stat -c%s build/jvm-disk.img) bytes: MBR + p1 ext4 JVM ro-root @ LBA $JP1_LBA + p2 ext2 rw-data @ LBA $JP2_LBA)"
echo "  sha256(jvm-ext4.img)=$(sha256sum build/jvm-ext4.img | cut -d' ' -f1)"
echo "  sha256(jvm-disk.img)=$(sha256sum build/jvm-disk.img | cut -d' ' -f1)"

# --- 2k.6 the tiny switch_root initramfs (same virtio-blk + ext4 + ext2 module stack as
#     M29's; mounts /dev/vda1 ext4 ro,noload and switch_roots). Reuses $ROOTMODS. ---
rm -rf build/initramfs-jvm
mkdir -p build/initramfs-jvm/bin build/initramfs-jvm/proc build/initramfs-jvm/sys \
         build/initramfs-jvm/dev build/initramfs-jvm/newroot build/initramfs-jvm/lib/modules
cp "$BB" build/initramfs-jvm/bin/busybox
chmod +x build/initramfs-jvm/bin/busybox
for m in $ROOTMODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  [ -z "$src" ] && { echo "note: jvm-initramfs module $m.ko.xz not found (built-in?)" >&2; continue; }
  xz -dc "$src" > "build/initramfs-jvm/lib/modules/$m.ko"
done
echo "staged modules for the jvm initramfs: $ROOTMODS"

cat > build/initramfs-jvm/init <<'JRINITRAMFS'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
echo ""
echo "=================================================="
echo "  hydo: initramfs reached (M31 JVM ext4 ro-root + switch_root)"
echo "=================================================="
ROOT=/dev/vda1
for tok in $(cat /proc/cmdline); do case "$tok" in root=*) ROOT=${tok#root=};; esac; done
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk crc16 crc32c_generic mbcache jbd2 ext4 ext2; do
  [ -f /lib/modules/$m.ko ] || continue
  insmod /lib/modules/$m.ko 2>/dev/null && echo "hydo-jvm-boot: insmod $m ok" || echo "hydo-jvm-boot: insmod $m (skipped/failed)"
done
n=0
while [ ! -b "$ROOT" ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
if [ ! -b "$ROOT" ]; then
  echo "hydo-jvm-boot: root device $ROOT MISSING"
  echo "HYDO-ORACLE: ERROR no root device"
  echo "hydo-jvm-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"
  exec /bin/busybox sh
fi
mkdir -p /newroot
if ! mount -t ext4 -o ro,noload "$ROOT" /newroot 2>/dev/null; then
  echo "hydo-jvm-boot: mount $ROOT (ext4 ro) FAILED"
  echo "HYDO-ORACLE: ERROR mount root"
  echo "hydo-jvm-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"
  exec /bin/busybox sh
fi
echo "hydo-jvm-boot: mounted $ROOT ext4 read-only at /newroot; switching root"
mkdir -p /newroot/dev /newroot/proc /newroot/sys
mount --move /dev /newroot/dev 2>/dev/null || mount -o move /dev /newroot/dev 2>/dev/null || \
  mount -t devtmpfs none /newroot/dev 2>/dev/null || true
exec switch_root /newroot /sbin/init
JRINITRAMFS
chmod +x build/initramfs-jvm/init

reproducible_cpio build/initramfs-jvm build/initramfs-jvm.cpio.gz
echo "built build/initramfs-jvm.cpio.gz ($(stat -c%s build/initramfs-jvm.cpio.gz) bytes)"

# ---- 2l. the MILESTONE 33 (Rung 5/6) KAFKA assets: a REAL, unmodified single-broker
#      Apache Kafka 3.5.0 in KRaft mode + a producer/consumer client, run
#      DETERMINISTICALLY under hvx over the in-kernel 127.0.0.1 loopback. Reuses the M29
#      ext4 ro-root + small-rw-disk model (the big JDK+Kafka-libs closure lives on the
#      ro-root, EXCLUDED from the snapshot; Kafka's log dir is on the SMALL rw disk = the
#      fault target + the only snapshotted disk). Uses the M32 recipe: the HEADLESS JDK
#      (NOT the minimal JRE -- Kafka needs java.lang.management.ManagementFactory), the
#      QUOTED classpath -cp "$KP/libs/*", storage-format via kafka.tools.StorageTool with
#      a FIXED cluster UUID (baked; NOT random-uuid at boot -> deterministic), the
#      deterministic-JIT c1sync flags, the M27 CRNG seed + idle heartbeat, and the M31
#      x86-64-v2 CPUID. Byte-reproducible: pinned JDK/Kafka store paths + the M29
#      mke2fs+libfaketime+time-zero discipline.
#        build/KafkaClient.class       the producer+consumer+oracle client (no timestamp)
#        build/rootfs-kafka-root/      ro-root tree: busybox + hvx_seedrng + JDK closure +
#                                      /opt/kafka/libs (105 jars) + /app + /etc/kafka
#        build/kafka-ext4.img          ext4 ro-root (p1)
#        build/kafka-data.img          ext2 rw-data (p2, Kafka's log.dirs + the fault target)
#        build/kafka-disk.img          MBR + p1 + p2 (hand-crafted MBR, like M29/M31)
#        build/initramfs-kafka.cpio.gz virtio-blk + ext4 + ext2 stack + switch_root
#      Guest RAM 1280 MiB (Kafka broker -Xmx320m + client -Xmx160m + Scala runtime); the
#      file-backed snapshot (M32) keeps the verify double-buffer off L1 RAM. --

# The RUNTIME + BUILD JDK: the HEADLESS JDK 17 (M32: Kafka needs java.management, which the
# minimal JRE omits). Pin the store path (with nix-build fallback), same discipline as §2k.
KAFKA_JDK="${HYDO_KAFKA_JDK:-}"
if [ -z "$KAFKA_JDK" ]; then
  KAFKA_JDK=/nix/store/vjcrra860yal7fzk1m81ddirrafqrccz-openjdk-headless-17.0.7+7
  if [ ! -x "$KAFKA_JDK/bin/java" ]; then
    P=$(nix build --no-link --print-out-paths "nixpkgs#jdk17_headless" 2>/dev/null | head -1 || true)
    [ -n "$P" ] && [ -x "$P/bin/java" ] && KAFKA_JDK="$P"
    [ -x "$KAFKA_JDK/bin/java" ] || KAFKA_JDK=$(ls -d /nix/store/*openjdk-headless-17.0.7*/ 2>/dev/null | grep -v '\.drv' | head -1 || true)
  fi
fi
if [ -z "$KAFKA_JDK" ] || [ ! -x "$KAFKA_JDK/bin/java" ]; then
  echo "error: no headless JDK 17 found (set HYDO_KAFKA_JDK=/path with bin/java)" >&2; exit 1
fi
# Apache Kafka 3.5.0 (pinned store path with a nix-build fallback).
KP="${HYDO_KAFKA:-}"
if [ -z "$KP" ]; then
  KP=/nix/store/prk07apdgqqjcqyxr5fqww1y8bxpw7z9-apache-kafka-2.13-3.5.0
  if [ ! -d "$KP/libs" ]; then
    P=$(nix build --no-link --print-out-paths "nixpkgs#apacheKafka" 2>/dev/null | head -1 || true)
    [ -n "$P" ] && [ -d "$P/libs" ] && KP="$P"
    [ -d "$KP/libs" ] || KP=$(ls -d /nix/store/*apache-kafka-*3.5.0/ 2>/dev/null | grep -v '\.drv' | head -1 || true)
  fi
fi
if [ -z "$KP" ] || [ ! -d "$KP/libs" ]; then
  echo "error: no apache-kafka 3.5.0 found (set HYDO_KAFKA=/nix/store/...-apache-kafka-...)" >&2; exit 1
fi
KAFKA_NJARS=$(ls "$KP"/libs/*.jar 2>/dev/null | wc -l)
echo "kafka: JDK=$KAFKA_JDK  KP=$KP ($KAFKA_NJARS jars)"

# A FIXED, valid Kafka cluster UUID (base64url of 16 bytes; generated once via
# kafka.tools.StorageTool random-uuid, then BAKED so every build + boot formats the
# store identically -- NOT random-uuid at boot, which would be nondeterministic).
KAFKA_CLUSTER_ID="P0hH8j92QAKKXdQ2D7Aw7A"
KAFKA_NMSG=30

# --- 2l.1 compile KafkaClient.java against kafka-clients (a bare .class -> reproducible) ---
rm -rf build/kafka-app; mkdir -p build/kafka-app
"$KAFKA_JDK/bin/javac" -cp "$KP/libs/*" -d build/kafka-app hvx/KafkaClient.java
cp build/kafka-app/KafkaClient.class build/KafkaClient.class
echo "compiled KafkaClient.java -> KafkaClient.class (via $KAFKA_JDK/bin/javac + kafka-clients)"

# --- 2l.2 the ro-root source tree: busybox + hvx_seedrng + JDK closure + kafka libs +
#     /app/KafkaClient.class + /etc/kafka/{server.properties,log4j.properties} + /sbin/init.
#     The JDK closure (512 MiB) + kafka jars (103 MiB) dominate a rebuild, so they are
#     staged ONCE and PRESERVED across rebuilds (only the small mutable dirs are refreshed);
#     a full `rm -rf build` forces a fresh copy. ---
KAFKA_R=build/rootfs-kafka-root
if [ -x "$KAFKA_R/nix/store/$(basename "$KAFKA_JDK")/bin/java" ] && \
   [ "$(ls "$KAFKA_R"/opt/kafka/libs/*.jar 2>/dev/null | wc -l)" -ge "$KAFKA_NJARS" ]; then
  echo "kafka ro-root: reusing staged JDK closure + $KAFKA_NJARS jars (rm -rf build to force fresh)"
  chmod -R u+w "$KAFKA_R/bin" "$KAFKA_R/sbin" "$KAFKA_R/etc" "$KAFKA_R/app" 2>/dev/null || true
  rm -rf "$KAFKA_R/bin" "$KAFKA_R/sbin" "$KAFKA_R/etc" "$KAFKA_R/app"
else
  chmod -R u+w "$KAFKA_R" 2>/dev/null || true
  rm -rf "$KAFKA_R"
  mkdir -p "$KAFKA_R/opt/kafka/libs" "$KAFKA_R/nix/store"
  cp "$KP"/libs/*.jar "$KAFKA_R/opt/kafka/libs/"      # flat copy of the 105 jars (M32: java -cp "$KP/libs/*")
  echo "staged $(ls "$KAFKA_R"/opt/kafka/libs/*.jar | wc -l) kafka jars onto the ro-root"
  echo "staging JDK closure ($(nix-store -qR "$KAFKA_JDK" | wc -l) store paths) onto the ro-root..."
  for p in $(nix-store -qR "$KAFKA_JDK"); do
    cp -a "$p" "$KAFKA_R/nix/store/" 2>/dev/null || true
  done
  echo "staged JDK closure: $(du -sm "$KAFKA_R/nix/store" 2>/dev/null | cut -f1) MiB under $KAFKA_R/nix/store"
fi
mkdir -p "$KAFKA_R/bin" "$KAFKA_R/sbin" "$KAFKA_R/proc" "$KAFKA_R/sys" "$KAFKA_R/dev" \
         "$KAFKA_R/data" "$KAFKA_R/tmp" "$KAFKA_R/etc/kafka" "$KAFKA_R/app"
cp "$BB" "$KAFKA_R/bin/busybox"; chmod +x "$KAFKA_R/bin/busybox"
cp build/hvx_seedrng "$KAFKA_R/bin/hvx_seedrng"; chmod +x "$KAFKA_R/bin/hvx_seedrng"   # M27 CRNG seeder (§2h)
cp build/kafka-app/KafkaClient.class "$KAFKA_R/app/"

# Bake java path + kafka classpath dir + cluster id + msg count for /sbin/init.
cat > build/rootfs-kafka-root/etc/kafka-env <<EOF
JAVA=$KAFKA_JDK/bin/java
KLIBS=/opt/kafka/libs
KAFKA_CLUSTER_ID=$KAFKA_CLUSTER_ID
KAFKA_NMSG=$KAFKA_NMSG
EOF

# server.properties: minimal single-node KRaft (broker+controller), replication.factor 1,
# log.dirs on the SMALL rw data disk (/data/kafka-logs), small indexes so the log fits, a
# deterministic-friendly small thread count. log.flush from the cmdline (see /sbin/init).
cat > build/rootfs-kafka-root/etc/kafka/server.properties <<'EOF'
process.roles=broker,controller
node.id=1
controller.quorum.voters=1@127.0.0.1:9093
listeners=PLAINTEXT://127.0.0.1:9092,CONTROLLER://127.0.0.1:9093
inter.broker.listener.name=PLAINTEXT
controller.listener.names=CONTROLLER
listener.security.protocol.map=CONTROLLER:PLAINTEXT,PLAINTEXT:PLAINTEXT
advertised.listeners=PLAINTEXT://127.0.0.1:9092
log.dirs=/data/kafka-logs
num.partitions=1
default.replication.factor=1
offsets.topic.replication.factor=1
offsets.topic.num.partitions=1
transaction.state.log.replication.factor=1
transaction.state.log.min.isr=1
auto.create.topics.enable=true
num.network.threads=2
num.io.threads=2
background.threads=2
log.index.size.max.bytes=1048576
log.segment.bytes=16777216
metadata.log.segment.bytes=8388608
group.initial.rebalance.delay.ms=0
EOF

# log4j: broker/client log to STDOUT (the /init redirects it to a /tmp file -> tmpfs, NOT
# the fingerprinted rw disk; only the oracle lines are echoed to the console).
cat > build/rootfs-kafka-root/etc/kafka/log4j.properties <<'EOF'
log4j.rootLogger=WARN, stdout
log4j.appender.stdout=org.apache.log4j.ConsoleAppender
log4j.appender.stdout.layout=org.apache.log4j.PatternLayout
log4j.appender.stdout.layout.ConversionPattern=%p %c %m%n
log4j.logger.kafka.server.KafkaRaftServer=INFO
EOF

# /sbin/init on the Kafka ro-root: seed the CRNG (M27), bring up loopback, mount the ext2
# rw data disk (Kafka's log.dirs), format the KRaft store with the FIXED cluster UUID, run
# a REAL single-broker Kafka (JIT-selectable, SerialGC), then the producer/consumer client.
cat > build/rootfs-kafka-root/sbin/init <<'KINIT'
#!/bin/busybox sh
BB=/bin/busybox
[ -c /dev/null ] || $BB mount -t devtmpfs none /dev 2>/dev/null
$BB mount -t proc none /proc 2>/dev/null
$BB mount -t sysfs none /sys 2>/dev/null
$BB mount -t tmpfs none /tmp 2>/dev/null
. /etc/kafka-env      # JAVA=... KLIBS=/opt/kafka/libs KAFKA_CLUSTER_ID=... KAFKA_NMSG=...
KP=/opt/kafka
CP="$KP/libs/*"
LOG4J="-Dlog4j.configuration=file:/etc/kafka/log4j.properties"
echo ""
echo "=================================================="
echo "  hydo: switched into the ext4 ro-root (M33 Kafka / Rung 5-6)"
echo "  uname: $($BB uname -sr)"
echo "=================================================="
echo "hydo-kafka-mid: real root is live"
# ---- cmdline: JIT config (default c1sync, proven deterministic M32), phase, fsync policy ----
JIT=c1sync; PHASE=solo; FLUSH=off; VERBOSE=0
for tok in $($BB cat /proc/cmdline 2>/dev/null); do
  case "$tok" in
    hydo_jit=*)     JIT="${tok#hydo_jit=}";;
    hydo_phase=*)   PHASE="${tok#hydo_phase=}";;
    hydo_flush=*)   FLUSH="${tok#hydo_flush=}";;
    hydo_verbose=*) VERBOSE="${tok#hydo_verbose=}";;
  esac
done
UP() { $BB cut -d' ' -f1 /proc/uptime 2>/dev/null; }  # guest-seconds (deterministic virtual clock)
case "$JIT" in
  int)        JITFLAGS="-Xint";;
  c1)         JITFLAGS="-XX:TieredStopAtLevel=1";;
  c1sync)     JITFLAGS="-XX:TieredStopAtLevel=1 -XX:CICompilerCount=1 -XX:-BackgroundCompilation";;
  tiered)     JITFLAGS="";;
  tieredsync) JITFLAGS="-XX:-BackgroundCompilation";;
  *)          JIT=c1sync; JITFLAGS="-XX:TieredStopAtLevel=1 -XX:CICompilerCount=1 -XX:-BackgroundCompilation";;
esac
# Common deterministic JVM flags (M31 recipe): SerialGC, no perfdata/container/CDS, fixed tz.
JCOMMON="$JITFLAGS -XX:+UseSerialGC -XX:-UsePerfData -XX:-UseContainerSupport -XX:ActiveProcessorCount=1 -Xshare:off -Djava.security.egd=file:/dev/urandom -Duser.name=hydo -Duser.home=/tmp -Duser.timezone=UTC -Duser.language=en -Duser.country=US -Dfile.encoding=UTF-8 -Djava.io.tmpdir=/tmp"
echo "hydo-kafka: JIT=$JIT phase=$PHASE flush=$FLUSH nmsg=$KAFKA_NMSG"
# ---- M27 CRNG seed so Kafka's SecureRandom/getrandom does not spin on the jitter-free TSC ----
/bin/hvx_seedrng 2>/dev/null || echo "hydo-kafka: hvx_seedrng missing/failed"
# ---- in-kernel loopback (deterministic; the client<->broker path is an in-kernel memcpy) ----
$BB ifconfig lo 127.0.0.1 netmask 255.0.0.0 up 2>/dev/null
$BB ip link set lo up 2>/dev/null
echo "hydo-kafka: loopback up"
# ---- mount the ext4 (journaled) rw data disk = Kafka's log.dirs (the fault target).
#      ext4 WITH a journal, not ext2: under the power-loss fault an un-journaled fs is
#      itself left inconsistent ("inode number already in use") and the broker dies on
#      recovery for a filesystem reason, hiding Kafka's own flush.messages durability. ----
# commit=86400: no periodic jbd2 commit during the run (see the writeback note below).
MERR=$($BB mount -t ext4 -o rw,commit=86400 /dev/vda2 /data 2>&1); MRC=$?
if [ $MRC -ne 0 ]; then
  if [ "$PHASE" = "oracle" ]; then
    # After the power cut the data disk itself no longer mounts: the committed log is
    # unrecoverable -- a durability violation, not a harness error.
    echo "HYDO-ORACLE: VIOLATION data disk unrecoverable (mount failed: $MERR)"
  else
    echo "HYDO-ORACLE: ERROR mount data ($MERR)"
  fi
  echo "hydo-kafka-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"; while true; do $BB sleep 100; done
fi
$BB mkdir -p /data/kafka-logs
echo "hydo-kafka: mounted ext4 /dev/vda2 rw on /data (log.dirs)"
# ---- no BACKGROUND writeback during the run. Otherwise whether an acked-but-not-fsync'd
#      message survives the power cut depends on whether the kernel's periodic writeback
#      (dirty_expire, ~30 s) or ext4's periodic journal commit (5 s) happened to run between
#      the ack and the cut -- i.e. on how much virtual time the run took, which differs
#      between clock configurations. With both off, the only thing that makes data durable
#      is an explicit fsync (Kafka's flush policy) or the explicit sync in the solo phase,
#      which is exactly the variable under test. ----
echo 0 > /proc/sys/vm/dirty_writeback_centisecs 2>/dev/null
echo 8640000 > /proc/sys/vm/dirty_expire_centisecs 2>/dev/null
# ---- append the fsync policy: hydo_flush=on -> flush.messages=1 (fsync every message =
#      the durable config, survives a power loss); off (default) -> Kafka default (acks=all
#      does NOT fsync on a single broker -> the documented durability window). ----
$BB cp /etc/kafka/server.properties /tmp/server.properties
if [ "$FLUSH" = "on" ]; then echo "log.flush.interval.messages=1" >> /tmp/server.properties; fi

start_broker() {
  echo "hydo-kafka: format start (uptime=$(UP)s, cluster.id=$KAFKA_CLUSTER_ID)"
  if [ "$VERBOSE" = 1 ]; then       # stream the format JVM's stdout to the console (bring-up diag)
    $JAVA $JCOMMON -Xms128m -Xmx128m $LOG4J -cp "$CP" kafka.tools.StorageTool format \
          -t "$KAFKA_CLUSTER_ID" -c /tmp/server.properties --ignore-formatted 2>&1; FRC=$?
  else
    $JAVA $JCOMMON -Xms128m -Xmx128m $LOG4J -cp "$CP" kafka.tools.StorageTool format \
          -t "$KAFKA_CLUSTER_ID" -c /tmp/server.properties --ignore-formatted > /tmp/format.log 2>&1; FRC=$?
  fi
  echo "hydo-kafka: format rc=$FRC done (uptime=$(UP)s) -> $($BB grep -a Formatting /tmp/format.log 2>/dev/null | $BB tail -1)"
  [ $FRC -ne 0 ] && { echo "hydo-kafka: format.log tail:"; $BB tail -15 /tmp/format.log 2>/dev/null; }
  # Make the freshly formatted store durable before any message is produced. StorageTool
  # writes bootstrap.checkpoint/meta.properties WITHOUT an fsync (tmp file + rename), so a
  # power cut before writeback leaves bootstrap.checkpoint empty and the broker can never
  # start again ("No FeatureLevelRecord for metadata.version ... bootstrap.checkpoint").
  # That is a real finding, but the checks here are about the MESSAGE flush policy, so the
  # setup is synced like an operator would after provisioning. (In the oracle phase the
  # store already exists and --ignore-formatted writes nothing.)
  [ "$PHASE" != "oracle" ] && $BB sync
  echo "hydo-kafka: launching REAL Kafka broker (jit=$JIT SerialGC -Xmx320m, uptime=$(UP)s)"
  $JAVA $JCOMMON $LOG4J -Xms320m -Xmx320m -cp "$CP" kafka.Kafka /tmp/server.properties \
        > /tmp/broker.log 2>&1 &
  BPID=$!
  n=0
  while [ $n -lt 600 ]; do
    if $BB grep -qa "Kafka Server started" /tmp/broker.log 2>/dev/null; then
      echo "hydo-kafka: broker STARTED after ${n} polls (uptime=$(UP)s)"; return 0
    fi
    $BB kill -0 $BPID 2>/dev/null || { echo "hydo-kafka: broker DIED early (uptime=$(UP)s)"; $BB tail -12 /tmp/broker.log; return 1; }
    if [ "$VERBOSE" = 1 ] && [ $n -gt 0 ] && [ $(( n % 15 )) -eq 0 ]; then
      echo "hydo-kafka: broker still starting (n=$n uptime=$(UP)s): $($BB tail -1 /tmp/broker.log 2>/dev/null)"
    fi
    n=$((n+1)); $BB sleep 1
  done
  echo "hydo-kafka: broker NOT ready after $n polls (uptime=$(UP)s); broker.log tail:"; $BB tail -15 /tmp/broker.log; return 1
}

run_client() {   # $1 = produce | consume | both
  echo "hydo-kafka: client mode=$1 nmsg=$KAFKA_NMSG start (uptime=$(UP)s) over 127.0.0.1:9092"
  $JAVA $JCOMMON $LOG4J -Xms160m -Xmx160m -cp "/app:$CP" KafkaClient \
        127.0.0.1:9092 hydotopic $KAFKA_NMSG "$1" > /tmp/client.log 2>&1
  echo "hydo-kafka: client rc=$? done (uptime=$(UP)s)"
  $BB grep -aE 'HYDO-KAFKA|HYDO-ORACLE' /tmp/client.log 2>/dev/null || \
    { echo "hydo-kafka: no HYDO line; client.log tail:"; $BB tail -12 /tmp/client.log; }
}

case "$PHASE" in
  workload)
    # Part B crash-recover BOOT1: format + produce (acks=all), then FREEZE the broker so no
    # background flush commits the un-fsync'd log before the harness extracts the durable disk.
    start_broker || { echo "HYDO-ORACLE: ERROR broker start"; echo "hydo-kafka-workload-done acked=0"; while true; do $BB sleep 100; done; }
    run_client produce
    # NB: deliberately NO `sync` here -- a sync would issue a virtio FLUSH and commit the
    # volatile write cache, defeating the power-loss window (F2 would then falsely SURVIVE).
    # Freeze the broker at the just-acked instant, un-fsync'd log still in the droppable cache.
    $BB kill -STOP $BPID 2>/dev/null
    echo "hydo-kafka: broker STOPped (durability frozen at the acked instant)"
    echo "hydo-kafka-workload-done"
    while true; do $BB sleep 100; done
    ;;
  oracle)
    # Part B crash-recover BOOT2 (fresh VM, durable disk, no fault): the broker recovers its
    # log from the device, then the consumer re-reads every acked key.
    echo "hydo-kafka: recovering from log"   # branch mid-point: mounted, BEFORE the broker recovers
    start_broker || { echo "HYDO-ORACLE: VIOLATION broker cannot recover (log corrupt: committed data unrecoverable)"; echo "hydo-kafka-marker: workload complete"; while true; do $BB sleep 100; done; }
    run_client consume
    echo "hydo-kafka-marker: workload complete"
    echo "hydo-boot-marker: launching shell on ttyS0"
    exec $BB sh
    ;;
  *)
    # SOLO (Part A): produce (acks=all) + consume + oracle in ONE timeline. The durability
    # invariant holds absent a fault -> HYDO-ORACLE: OK. This is the Part-A determinism boot.
    start_broker || { echo "HYDO-ORACLE: ERROR broker start"; echo "hydo-kafka-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"; while true; do $BB sleep 100; done; }
    run_client both
    $BB sync
    echo "hydo-kafka-marker: workload complete"
    echo "hydo-boot-marker: launching shell on ttyS0"
    exec $BB sh
    ;;
esac
KINIT
chmod +x build/rootfs-kafka-root/sbin/init
find build/rootfs-kafka-root -exec touch --no-dereference -h -d @1600000000 {} + 2>/dev/null || true

# --- 2l.3 the byte-reproducible ext4 ro-root image (p1): holds the JDK closure + kafka jars ---
KAFKA_EXT4_MIB=1024
KAFKA_DATA_MIB=128
: > build/kafka-ext4.img; truncate -s $(( KAFKA_EXT4_MIB * 1024 * 1024 )) build/kafka-ext4.img
# mke2fs -d copies directory entries in the HOST's readdir order. The top-level dir of the
# ro-root keeps the heavy nix/ and opt/ subtrees across builds while bin/ sbin/ etc/ app/
# are deleted and recreated, and on the build box's ext4 a small (linear) directory then
# lists its entries in an order that depends on that deletion history -- the first build
# after a recipe change produced a different image from the next one. Rebuild the
# top-level directory fresh, entries moved in sorted order, so its listing is a pure
# function of its contents.
mv "${KAFKA_R:?}" "${KAFKA_R:?}.reorder"
mkdir "$KAFKA_R"
for d in $(ls -A "${KAFKA_R:?}.reorder" | LC_ALL=C sort); do mv "${KAFKA_R:?}.reorder/$d" "$KAFKA_R/$d"; done
rmdir "${KAFKA_R:?}.reorder"
touch --no-dereference -h -d @1600000000 "$KAFKA_R"
mkext4_rootfs_reproducible build/kafka-ext4.img build/rootfs-kafka-root $KAFKA_EXT4_MIB \
  eeeeeeee-ffff-0000-1111-222233334444 55554444-3333-2222-1111-000011112222
# mkext4_rootfs_reproducible zeroes only i_ctime. The libfaketime clock is NOT frozen (it
# advances in real time), so a build that straddles a wall-clock second also stamps
# i_crtime and the superblock write time one second later -- observed: two builds of
# this image differing in exactly those bytes. Zero every remaining time field (same
# technique as the §2j system disk). Kafka-only, so no other image changes.
"$PY3" - build/kafka-ext4.img <<'PYEOF'
import sys, struct, mmap
f = open(sys.argv[1], "r+b"); mm = mmap.mmap(f.fileno(), 0)
u16 = lambda o: struct.unpack_from("<H", mm, o)[0]
u32 = lambda o: struct.unpack_from("<I", mm, o)[0]
sb = 1024
inodes = u32(sb + 0x00); bs = 1024 << u32(sb + 0x18)
ipg = u32(sb + 0x28); isize = u16(sb + 0x58) or 128
bpg = u32(sb + 0x20)
gdtb = 2 if bs == 1024 else 1
ng = (inodes + ipg - 1) // ipg
# the primary superblock AND every backup copy (sparse_super: groups 1, 3^n, 5^n, 7^n)
for g in range(ng):
    o = sb if g == 0 else g * bpg * bs + (1024 if bs == 1024 else 0)
    if o + 0x3A <= len(mm) and u16(o + 0x38) == 0xEF53:
        for t in (0x2C, 0x30, 0x40, 0x108):         # s_mtime s_wtime s_lastcheck s_mkfs_time
            struct.pack_into("<I", mm, o + t, 0)
for g in range(ng):
    itbl = u32(gdtb * bs + g * 32 + 0x08)
    for i in range(ipg):
        if g * ipg + i >= inodes:
            break
        io = itbl * bs + i * isize
        struct.pack_into("<I", mm, io + 0x14, 0)    # i_dtime (i_ctime already zeroed)
        if isize > 128:                             # *_extra times + i_crtime + i_crtime_extra
            for o in (0x84, 0x88, 0x8C, 0x90, 0x94):
                struct.pack_into("<I", mm, io + o, 0)
mm.flush(); mm.close(); f.close()
PYEOF
echo "built build/kafka-ext4.img ($(stat -c%s build/kafka-ext4.img) bytes, reproducible ext4 Kafka ro-root)"

# --- 2l.4 the ext2 rw DATA disk (p2, 128 MiB, Kafka's log.dirs). A dedicated reproducible
#     mke2fs (pinned tool + libfaketime) + the M30 whole-image time-zero pass (mke2fs on a
#     128 MiB fs takes long enough to straddle a wall-clock second, so freeze ALL times). ---
: > build/kafka-data.img; truncate -s $(( KAFKA_DATA_MIB * 1024 * 1024 )) build/kafka-data.img
# ext4 in the SIMPLE layout (no metadata_csum/64bit/flex_bg/resize_inode, so the flat
# 32-byte group descriptors + un-checksummed inodes below stay parseable) WITH an 8 MiB
# internal journal, fully zeroed at mkfs time (lazy_journal_init=0).
SOURCE_DATE_EPOCH=1600000000 "$FAKETIME_BIN" "$FAKE_TS" "$MKE2FS" -q -F -t ext4 -b 1024 \
  -O ^metadata_csum,^64bit,^flex_bg,^resize_inode,has_journal -J size=8 \
  -U dddd1111-2222-3333-4444-5555eeee6666 \
  -E hash_seed=eeee5555-4444-3333-2222-1111dddd6666,lazy_itable_init=0,lazy_journal_init=0 \
  build/kafka-data.img
"$PY3" - build/kafka-data.img <<'PYEOF'
import sys, struct, mmap
f = open(sys.argv[1], "r+b"); mm = mmap.mmap(f.fileno(), 0)
u16 = lambda o: struct.unpack_from("<H", mm, o)[0]
u32 = lambda o: struct.unpack_from("<I", mm, o)[0]
sb = 1024
inodes = u32(sb + 0x00); bs = 1024 << u32(sb + 0x18)
ipg = u32(sb + 0x28); isize = u16(sb + 0x58) or 128
for o in (0x2C, 0x30, 0x40, 0x108):                 # s_mtime s_wtime s_lastcheck s_mkfs_time
    struct.pack_into("<I", mm, sb + o, 0)
gdtb = 2 if bs == 1024 else 1
ng = (inodes + ipg - 1) // ipg
for g in range(ng):
    itbl = u32(gdtb * bs + g * 32 + 0x08)
    base = g * ipg
    for i in range(ipg):
        if base + i >= inodes:
            break
        io = itbl * bs + i * isize
        for o in (0x08, 0x0C, 0x10, 0x14):          # i_atime i_ctime i_mtime i_dtime
            struct.pack_into("<I", mm, io + o, 0)
        if isize > 128:                             # ext4 large inode: *_extra times + i_crtime
            for o in (0x84, 0x88, 0x8C, 0x90, 0x94):
                struct.pack_into("<I", mm, io + o, 0)
mm.flush(); mm.close(); f.close()
PYEOF
echo "built build/kafka-data.img ($(stat -c%s build/kafka-data.img) bytes, reproducible ext4+journal rw-data, time-normalized)"

# --- 2l.5 assemble the MBR-partitioned disk (hand-crafted MBR, like §2i.4/§2k.5) ---
KP1_LBA=2048
KP1_SECTORS=$(( KAFKA_EXT4_MIB * 1024 * 1024 / 512 ))
KP2_SECTORS=$(( KAFKA_DATA_MIB * 1024 * 1024 / 512 ))
KP2_LBA=$(( KP1_LBA + KP1_SECTORS ))
KDISK_SECTORS=$(( KP2_LBA + KP2_SECTORS ))
: > build/kafka-disk.img
truncate -s $(( KDISK_SECTORS * 512 )) build/kafka-disk.img
"$PY3" - build/kafka-disk.img "$KP1_LBA" "$KP1_SECTORS" "$KP2_LBA" "$KP2_SECTORS" <<'PYEOF'
import sys, struct
img, p1s, p1n, p2s, p2n = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
mbr = bytearray(512)
mbr[0x1B8:0x1BC] = b"\x0d\xd0\xea\x1d"
def part(off, lba, n, ptype=0x83):
    mbr[off] = 0x00
    mbr[off + 1:off + 4] = b"\xfe\xff\xff"
    mbr[off + 4] = ptype
    mbr[off + 5:off + 8] = b"\xfe\xff\xff"
    struct.pack_into("<I", mbr, off + 8, lba)
    struct.pack_into("<I", mbr, off + 12, n)
part(0x1BE, p1s, p1n)
part(0x1CE, p2s, p2n)
mbr[0x1FE] = 0x55; mbr[0x1FF] = 0xAA
with open(img, "r+b") as f:
    f.write(mbr)
PYEOF
dd if=build/kafka-ext4.img of=build/kafka-disk.img bs=512 seek=$KP1_LBA conv=notrunc status=none
dd if=build/kafka-data.img of=build/kafka-disk.img bs=512 seek=$KP2_LBA conv=notrunc status=none
echo "built build/kafka-disk.img ($(stat -c%s build/kafka-disk.img) bytes: MBR + p1 ext4 Kafka ro-root @ LBA $KP1_LBA + p2 ext4+journal rw-data @ LBA $KP2_LBA)"
echo "  sha256(kafka-ext4.img)=$(sha256sum build/kafka-ext4.img | cut -d' ' -f1)"
echo "  sha256(kafka-disk.img)=$(sha256sum build/kafka-disk.img | cut -d' ' -f1)"

# --- 2l.6 the tiny switch_root initramfs (same virtio-blk + ext4 + ext2 module stack as
#     M29/M31; mounts /dev/vda1 ext4 ro,noload and switch_roots). Reuses $ROOTMODS. ---
rm -rf build/initramfs-kafka
mkdir -p build/initramfs-kafka/bin build/initramfs-kafka/proc build/initramfs-kafka/sys \
         build/initramfs-kafka/dev build/initramfs-kafka/newroot build/initramfs-kafka/lib/modules
cp "$BB" build/initramfs-kafka/bin/busybox
chmod +x build/initramfs-kafka/bin/busybox
for m in $ROOTMODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  [ -z "$src" ] && { echo "note: kafka-initramfs module $m.ko.xz not found (built-in?)" >&2; continue; }
  xz -dc "$src" > "build/initramfs-kafka/lib/modules/$m.ko"
done
echo "staged modules for the kafka initramfs: $ROOTMODS"

cat > build/initramfs-kafka/init <<'KRINITRAMFS'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
echo ""
echo "=================================================="
echo "  hydo: initramfs reached (M33 Kafka ext4 ro-root + switch_root)"
echo "=================================================="
ROOT=/dev/vda1
for tok in $(cat /proc/cmdline); do case "$tok" in root=*) ROOT=${tok#root=};; esac; done
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk crc16 crc32c_generic mbcache jbd2 ext4 ext2; do
  [ -f /lib/modules/$m.ko ] || continue
  insmod /lib/modules/$m.ko 2>/dev/null && echo "hydo-kafka-boot: insmod $m ok" || echo "hydo-kafka-boot: insmod $m (skipped/failed)"
done
n=0
while [ ! -b "$ROOT" ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
if [ ! -b "$ROOT" ]; then
  echo "hydo-kafka-boot: root device $ROOT MISSING"
  echo "HYDO-ORACLE: ERROR no root device"
  echo "hydo-kafka-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"
  exec /bin/busybox sh
fi
mkdir -p /newroot
if ! mount -t ext4 -o ro,noload "$ROOT" /newroot 2>/dev/null; then
  echo "hydo-kafka-boot: mount $ROOT (ext4 ro) FAILED"
  echo "HYDO-ORACLE: ERROR mount root"
  echo "hydo-kafka-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"
  exec /bin/busybox sh
fi
echo "hydo-kafka-boot: mounted $ROOT ext4 read-only at /newroot; switching root"
mkdir -p /newroot/dev /newroot/proc /newroot/sys
mount --move /dev /newroot/dev 2>/dev/null || mount -o move /dev /newroot/dev 2>/dev/null || \
  mount -t devtmpfs none /newroot/dev 2>/dev/null || true
exec switch_root /newroot /sbin/init
KRINITRAMFS
chmod +x build/initramfs-kafka/init

reproducible_cpio build/initramfs-kafka build/initramfs-kafka.cpio.gz
echo "built build/initramfs-kafka.cpio.gz ($(stat -c%s build/initramfs-kafka.cpio.gz) bytes)"

# ---- 2m. the SPREAD assets (SPREAD_ON_HVX Phase 1): ONE unmodified `spread` queue broker
#      (a dynamically-linked Rust binary, glibc nix closure) on the `fs` backend, run
#      DETERMINISTICALLY under hvx over the in-kernel 127.0.0.1 loopback, driven by an
#      in-guest static-Go HTTP driver+oracle (spread-drv). Clones the M31 JVM model
#      (§2k): the closure lives on an ext4 READ-ONLY root (EXCLUDED from the snapshot);
#      the fs-backend --data-dir is a small ext2 rw disk (p2, the snapshot/fault target).
#      Byte-reproducible: the spread binary + its ldd-closure store paths + the M29
#      mke2fs+libfaketime+time-zero discipline. Mandatory hvx-isms live in /sbin/init and
#      the run scripts (HVX_VM_X86_64_V2 for the glibc userland, HVX_VM_IDLE_HEARTBEAT for
#      the broker parked in accept/epoll, hvx_seedrng so Rust getrandom() does not hang on
#      the jitter-free CRNG, TOKIO_WORKER_THREADS=1 + randomize_va_space=0).
#        build/spread-drv               the static (CGO_ENABLED=0) Go driver+oracle
#        build/rootfs-spread-root/      ro-root: busybox + hvx_seedrng + /app/spread +
#                                       /app/spread-drv + the spread runtime closure /nix/store
#        build/spread-ext4.img          ext4 ro-root (p1)
#        build/spread-data.img          ext2 rw-data (p2, fs backend --data-dir)
#        build/spread-disk.img          MBR + p1 + p2 (hand-crafted MBR, like M29/M31)
#        build/initramfs-spread.cpio.gz virtio-blk + ext4 + ext2 stack + switch_root

# --- 2m.1 the spread binary + its runtime closure (dynamic glibc nix closure). The binary
#     is NOT itself a store path, so mine the closure from its ELF interp + ldd deps and
#     stage those store paths verbatim; the binary keeps its baked absolute /nix/store
#     interp and resolves libc/libm/libgcc_s via LD_LIBRARY_PATH (empty RPATH). ZERO
#     patching of the binary -> a true black box. ---
SPREAD_BIN="${HYDO_SPREAD_BIN:-$HOME/spread/target/release/spread}"
[ -x "$SPREAD_BIN" ] || { echo "error: spread binary not found at $SPREAD_BIN (set HYDO_SPREAD_BIN)" >&2; exit 1; }
SPREAD_BIN=$(realpath "$SPREAD_BIN")
# ELF interpreter (an absolute /nix/store path baked into the binary). Prefer patchelf, fall
# back to readelf (patchelf is not in the default profile on the box; readelf is).
SPREAD_INTERP=$(patchelf --print-interpreter "$SPREAD_BIN" 2>/dev/null || true)
[ -n "$SPREAD_INTERP" ] || SPREAD_INTERP=$(readelf -l "$SPREAD_BIN" 2>/dev/null | grep -oE '/nix/store/[^]]+' | head -1 || true)
[ -n "$SPREAD_INTERP" ] || { echo "error: could not read spread's ELF interpreter (patchelf/readelf)" >&2; exit 1; }
GLIBC_ROOT=$(printf '%s\n' "$SPREAD_INTERP" | sed -E 's#(/nix/store/[^/]+)/.*#\1#')
# the store roots of the DT_NEEDED libs (ldd resolves them via the nix ld.so).
LIB_ROOTS=$(ldd "$SPREAD_BIN" 2>/dev/null | grep -oE '/nix/store/[^/ ]+' | sed -E 's#(/nix/store/[^/]+).*#\1#' | sort -u)
SPREAD_CLOSURE=$(nix-store -qR "$GLIBC_ROOT" $LIB_ROOTS 2>/dev/null | sort -u)
[ -n "$SPREAD_CLOSURE" ] || { echo "error: spread closure computation returned empty" >&2; exit 1; }
echo "spread: binary=$SPREAD_BIN interp=$SPREAD_INTERP"
echo "spread: runtime closure = $(printf '%s\n' "$SPREAD_CLOSURE" | wc -l) store paths"

# --- 2m.2 build the in-guest driver+oracle STATIC (CGO_ENABLED=0) so it is a self-contained
#     ELF needing nothing from the closure. Pinned Go from nixos-unstable (channel Go is too
#     old); fall back to any go on PATH. ---
GO_BIN="${HYDO_GO:-}"
if [ -z "$GO_BIN" ]; then
  GO_BIN=/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go
  [ -x "$GO_BIN" ] || GO_BIN=$(ls /nix/store/*go-1.2*/bin/go 2>/dev/null | head -1 || true)
  [ -x "$GO_BIN" ] || GO_BIN=$(command -v go || true)
fi
[ -x "$GO_BIN" ] || { echo "error: no go found (set HYDO_GO=/path/to/go)" >&2; exit 1; }
SPREAD_DRV_OUT="$(pwd)/build/spread-drv"
( cd hvx/spread-drv && CGO_ENABLED=0 GOFLAGS=-trimpath "$GO_BIN" build -o "$SPREAD_DRV_OUT" . )
echo "built build/spread-drv ($(stat -c%s build/spread-drv) bytes, static Go) via $GO_BIN"

# --- 2m.3 the ro-root source tree: busybox + hvx_seedrng + /app/{spread,spread-drv} + the
#     spread runtime closure replicated under /nix/store. ---
chmod -R u+w build/rootfs-spread-root 2>/dev/null || true
rm -rf build/rootfs-spread-root
mkdir -p build/rootfs-spread-root/bin build/rootfs-spread-root/sbin build/rootfs-spread-root/proc \
         build/rootfs-spread-root/sys build/rootfs-spread-root/dev build/rootfs-spread-root/data \
         build/rootfs-spread-root/tmp build/rootfs-spread-root/etc build/rootfs-spread-root/app \
         build/rootfs-spread-root/nix/store
cp "$BB" build/rootfs-spread-root/bin/busybox
chmod +x build/rootfs-spread-root/bin/busybox
cp build/hvx_seedrng build/rootfs-spread-root/bin/hvx_seedrng   # M27 CRNG seeder (built in §2g)
chmod +x build/rootfs-spread-root/bin/hvx_seedrng
cp "$SPREAD_BIN" build/rootfs-spread-root/app/spread            # UNMODIFIED spread binary
chmod +x build/rootfs-spread-root/app/spread
cp build/spread-drv build/rootfs-spread-root/app/spread-drv
chmod +x build/rootfs-spread-root/app/spread-drv

echo "staging spread runtime closure onto the ro-root..."
for p in $SPREAD_CLOSURE; do
  cp -a "$p" build/rootfs-spread-root/nix/store/ 2>/dev/null || true
done
SPREAD_CLOSURE_MIB=$(du -sm build/rootfs-spread-root/nix/store 2>/dev/null | cut -f1)
echo "staged spread closure: ${SPREAD_CLOSURE_MIB} MiB under build/rootfs-spread-root/nix/store"

# Bake LD_LIBRARY_PATH (a pure function of the pinned closure) for /sbin/init: every closure
# root's /lib dir, so the empty-RPATH binary's libc/libm/libgcc_s resolve with zero patching.
SPREAD_LDLP=""
for p in $SPREAD_CLOSURE; do [ -d "$p/lib" ] && SPREAD_LDLP="${SPREAD_LDLP}${p}/lib:"; done
{ echo "LD_LIBRARY_PATH=$SPREAD_LDLP"; echo "SPREAD_BIN=/app/spread"; } > build/rootfs-spread-root/etc/spread-env

# /sbin/init on the spread ro-root: seed the CRNG, bring up loopback, mount the ext2 rw data
# disk (the fs backend --data-dir), start ONE spread broker deterministically, then drive it
# with the in-guest static-Go oracle (spread-drv) and print HYDO-ORACLE + the stop marker.
cat > build/rootfs-spread-root/sbin/init <<'SINIT'
#!/bin/busybox sh
BB=/bin/busybox
[ -c /dev/null ] || $BB mount -t devtmpfs none /dev 2>/dev/null
$BB mount -t proc none /proc 2>/dev/null
$BB mount -t sysfs none /sys 2>/dev/null
$BB mount -t tmpfs none /tmp 2>/dev/null
. /etc/spread-env      # sets LD_LIBRARY_PATH, SPREAD_BIN
echo ""
echo "=================================================="
echo "  hydo: switched into the ext4 ro-root (SPREAD / Phase 1)"
echo "  uname: $($BB uname -sr)"
echo "=================================================="
echo "hydo-spread-mid: real root is live"
# hvx-ism 1: credit deterministic entropy so Rust getrandom() returns instead of spinning
# on the jitter-free deterministic TSC (else the broker hangs at startup).
/bin/hvx_seedrng 2>/dev/null || echo "hydo-spread: hvx_seedrng missing/failed"
# hvx-ism 2: bring up the in-kernel loopback (spread listens/serves on 127.0.0.1 only).
$BB ip addr add 127.0.0.1/8 dev lo 2>/dev/null
$BB ip link set lo up 2>/dev/null || $BB ifconfig lo 127.0.0.1 up 2>/dev/null
# mount the ext2 rw data disk = the fs backend --data-dir (the snapshot/fault target).
MERR=$($BB mount -t ext2 -o rw /dev/vda2 /data 2>&1); MRC=$?
if [ $MRC -ne 0 ]; then
  echo "HYDO-ORACLE: VIOLATION mount data ($MERR)"
  echo "hydo-spread-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"; exec $BB sh
fi
echo "hydo-spread: mounted ext2 /dev/vda2 rw on /data"
# hvx-ism 3+4: single tokio worker + no ASLR -> deterministic scheduling/addresses.
export LD_LIBRARY_PATH
export TOKIO_WORKER_THREADS=1
echo 0 > /proc/sys/kernel/randomize_va_space 2>/dev/null || true
echo "hydo-spread: launching ONE broker: $SPREAD_BIN --backend fs --data-dir /data --listen 127.0.0.1:8080"
$SPREAD_BIN --backend fs --data-dir /data --listen 127.0.0.1:8080 > /tmp/spread.log 2>&1 &
SPID=$!
# wait for /healthz (Go client polls; reliable), then drive the single-broker smoke oracle.
/app/spread-drv health 8080
echo "hydo-spread: broker healthy (pid $SPID); driving smoke workload"
/app/spread-drv smoke 8080
echo "hydo-spread: spread.log tail:"; $BB tail -4 /tmp/spread.log 2>/dev/null
$BB sync
$BB umount /data 2>/dev/null
echo "hydo-spread-marker: workload complete"
echo "hydo-boot-marker: launching shell on ttyS0"
exec $BB sh
SINIT
chmod +x build/rootfs-spread-root/sbin/init
find build/rootfs-spread-root -exec touch --no-dereference -h -d @1600000000 {} + 2>/dev/null || true

# --- 2m.4 the byte-reproducible ext4 ro-root image (p1). Sized for the ~104 MiB spread
#     binary + ~42 MiB closure + the driver + slack. Reuses mkext4_rootfs_reproducible. ---
SPREAD_EXT4_MIB=320
SPREAD_DATA_MIB=16
: > build/spread-ext4.img; truncate -s $(( SPREAD_EXT4_MIB * 1024 * 1024 )) build/spread-ext4.img
mkext4_rootfs_reproducible build/spread-ext4.img build/rootfs-spread-root $SPREAD_EXT4_MIB \
  aaaa1111-2222-3333-4444-555566667777 abcdef01-2345-6789-abcd-ef0123456780
echo "built build/spread-ext4.img ($(stat -c%s build/spread-ext4.img) bytes, reproducible ext4 spread ro-root)"

# --- 2m.5 the small ext2 rw DATA disk (p2), same pinned recipe (distinct UUID/hash_seed) ---
mkext2_reproducible build/spread-data.img \
  dddd2222-3333-4444-5555-666677778888 fedcba98-7654-3210-fedc-ba9876543210
echo "built build/spread-data.img ($(stat -c%s build/spread-data.img) bytes, reproducible ext2 rw-data)"

# --- 2m.6 assemble the MBR-partitioned disk (hand-crafted MBR, like M31 §2k.5) ---
SP1_LBA=2048
SP1_SECTORS=$(( SPREAD_EXT4_MIB * 1024 * 1024 / 512 ))
SP2_SECTORS=$(( SPREAD_DATA_MIB * 1024 * 1024 / 512 ))
SP2_LBA=$(( SP1_LBA + SP1_SECTORS ))
SDISK_SECTORS=$(( SP2_LBA + SP2_SECTORS ))
: > build/spread-disk.img
truncate -s $(( SDISK_SECTORS * 512 )) build/spread-disk.img
"$PY3" - build/spread-disk.img "$SP1_LBA" "$SP1_SECTORS" "$SP2_LBA" "$SP2_SECTORS" <<'PYEOF'
import sys, struct
img, p1s, p1n, p2s, p2n = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
mbr = bytearray(512)
mbr[0x1B8:0x1BC] = b"\x0d\xd0\xea\x1d"
def part(off, lba, n, ptype=0x83):
    mbr[off] = 0x00
    mbr[off + 1:off + 4] = b"\xfe\xff\xff"
    mbr[off + 4] = ptype
    mbr[off + 5:off + 8] = b"\xfe\xff\xff"
    struct.pack_into("<I", mbr, off + 8, lba)
    struct.pack_into("<I", mbr, off + 12, n)
part(0x1BE, p1s, p1n)
part(0x1CE, p2s, p2n)
mbr[0x1FE] = 0x55; mbr[0x1FF] = 0xAA
with open(img, "r+b") as f:
    f.write(mbr)
PYEOF
dd if=build/spread-ext4.img of=build/spread-disk.img bs=512 seek=$SP1_LBA conv=notrunc status=none
dd if=build/spread-data.img of=build/spread-disk.img bs=512 seek=$SP2_LBA conv=notrunc status=none
echo "built build/spread-disk.img ($(stat -c%s build/spread-disk.img) bytes: MBR + p1 ext4 spread ro-root @ LBA $SP1_LBA + p2 ext2 rw-data @ LBA $SP2_LBA)"
echo "  sha256(spread-ext4.img)=$(sha256sum build/spread-ext4.img | cut -d' ' -f1)"
echo "  sha256(spread-disk.img)=$(sha256sum build/spread-disk.img | cut -d' ' -f1)"

# --- 2m.7 the tiny switch_root initramfs (same virtio-blk + ext4 + ext2 module stack as
#     M31's; mounts /dev/vda1 ext4 ro,noload and switch_roots). Reuses $ROOTMODS. ---
rm -rf build/initramfs-spread
mkdir -p build/initramfs-spread/bin build/initramfs-spread/proc build/initramfs-spread/sys \
         build/initramfs-spread/dev build/initramfs-spread/newroot build/initramfs-spread/lib/modules
cp "$BB" build/initramfs-spread/bin/busybox
chmod +x build/initramfs-spread/bin/busybox
for m in $ROOTMODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  [ -z "$src" ] && { echo "note: spread-initramfs module $m.ko.xz not found (built-in?)" >&2; continue; }
  xz -dc "$src" > "build/initramfs-spread/lib/modules/$m.ko"
done
echo "staged modules for the spread initramfs: $ROOTMODS"

cat > build/initramfs-spread/init <<'SRINITRAMFS'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
echo ""
echo "=================================================="
echo "  hydo: initramfs reached (SPREAD ext4 ro-root + switch_root)"
echo "=================================================="
ROOT=/dev/vda1
for tok in $(cat /proc/cmdline); do case "$tok" in root=*) ROOT=${tok#root=};; esac; done
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk crc16 crc32c_generic mbcache jbd2 ext4 ext2; do
  [ -f /lib/modules/$m.ko ] || continue
  insmod /lib/modules/$m.ko 2>/dev/null && echo "hydo-spread-boot: insmod $m ok" || echo "hydo-spread-boot: insmod $m (skipped/failed)"
done
n=0
while [ ! -b "$ROOT" ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
if [ ! -b "$ROOT" ]; then
  echo "hydo-spread-boot: root device $ROOT MISSING"
  echo "HYDO-ORACLE: VIOLATION no root device"
  echo "hydo-spread-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"
  exec /bin/busybox sh
fi
mkdir -p /newroot
if ! mount -t ext4 -o ro,noload "$ROOT" /newroot 2>/dev/null; then
  echo "hydo-spread-boot: mount $ROOT (ext4 ro) FAILED"
  echo "HYDO-ORACLE: VIOLATION mount root"
  echo "hydo-spread-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"
  exec /bin/busybox sh
fi
echo "hydo-spread-boot: mounted $ROOT ext4 read-only at /newroot; switching root"
mkdir -p /newroot/dev /newroot/proc /newroot/sys
mount --move /dev /newroot/dev 2>/dev/null || mount -o move /dev /newroot/dev 2>/dev/null || \
  mount -t devtmpfs none /newroot/dev 2>/dev/null || true
exec switch_root /newroot /sbin/init
SRINITRAMFS
chmod +x build/initramfs-spread/init

reproducible_cpio build/initramfs-spread build/initramfs-spread.cpio.gz
echo "built build/initramfs-spread.cpio.gz ($(stat -c%s build/initramfs-spread.cpio.gz) bytes)"

# ---- 2n. the SPREAD PHASE-2 assets (SPREAD_ON_HVX Phase 2): TWO co-located `fs`-backed spread
#      brokers sharing ONE rw data disk (the M30 multi-service launcher shape), driven by the FULL
#      mini-Jepsen consistency oracle (spread-drv's op-program runner) with a broker crash, run
#      DETERMINISTICALLY under hvx. Same ro-root closure model as §2m; DISTINCT rootfs + disk so
#      §2m (Phase 1, fingerprint-verified) is left byte-for-byte untouched.
#        build/rootfs-spread2-root/     ro-root: busybox + hvx_seedrng + /app/{spread,spread-drv}
#        build/spread2-ext4.img         ext4 ro-root (p1)
#        build/spread2-data.img         ext2 rw-data (p2, the SHARED fs backend --data-dir + fault target)
#        build/spread2-disk.img         MBR + p1 + p2
#        build/initramfs-spread2.cpio.gz  virtio-blk + ext4 + ext2 stack + switch_root
# The op-program (piped to the runner by /init): sends/receives/nacks across BOTH brokers plus a
# broker-2 SIGKILL (X 2 -> the driver reads /b2.pid, so /init writes each broker's pid there); the
# oracle asserts at-least-once / FIFO / no-split-brain / no-phantom over the shared log + failover.

# --- 2n.1 recompute spread's runtime closure + build the driver (self-sufficient so build-spread2.sh
#     can eval JUST this block; identical detection to §2m). ---
SP2_BIN="${HYDO_SPREAD_BIN:-$HOME/spread/target/release/spread}"
[ -x "$SP2_BIN" ] || { echo "error: spread binary not found at $SP2_BIN (set HYDO_SPREAD_BIN)" >&2; exit 1; }
SP2_BIN=$(realpath "$SP2_BIN")
SP2_INTERP=$(patchelf --print-interpreter "$SP2_BIN" 2>/dev/null || true)
[ -n "$SP2_INTERP" ] || SP2_INTERP=$(readelf -l "$SP2_BIN" 2>/dev/null | grep -oE '/nix/store/[^]]+' | head -1 || true)
[ -n "$SP2_INTERP" ] || { echo "error: could not read spread's ELF interpreter (patchelf/readelf)" >&2; exit 1; }
SP2_GLIBC_ROOT=$(printf '%s\n' "$SP2_INTERP" | sed -E 's#(/nix/store/[^/]+)/.*#\1#')
SP2_LIB_ROOTS=$(ldd "$SP2_BIN" 2>/dev/null | grep -oE '/nix/store/[^/ ]+' | sed -E 's#(/nix/store/[^/]+).*#\1#' | sort -u)
SP2_CLOSURE=$(nix-store -qR "$SP2_GLIBC_ROOT" $SP2_LIB_ROOTS 2>/dev/null | sort -u)
[ -n "$SP2_CLOSURE" ] || { echo "error: spread closure computation returned empty" >&2; exit 1; }
echo "spread(phase2): binary=$SP2_BIN runtime closure = $(printf '%s\n' "$SP2_CLOSURE" | wc -l) store paths"
GO2_BIN="${HYDO_GO:-}"
if [ -z "$GO2_BIN" ]; then
  GO2_BIN=/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go
  [ -x "$GO2_BIN" ] || GO2_BIN=$(ls /nix/store/*go-1.2*/bin/go 2>/dev/null | head -1 || true)
  [ -x "$GO2_BIN" ] || GO2_BIN=$(command -v go || true)
fi
[ -x "$GO2_BIN" ] || { echo "error: no go found (set HYDO_GO=/path/to/go)" >&2; exit 1; }
SP2_DRV_OUT="$(pwd)/build/spread-drv"
( cd hvx/spread-drv && CGO_ENABLED=0 GOFLAGS=-trimpath "$GO2_BIN" build -o "$SP2_DRV_OUT" . )
echo "built build/spread-drv ($(stat -c%s build/spread-drv) bytes, static Go) via $GO2_BIN"
# Guest-side reader for the SYNCHRONOUS fuzz-input port (Phase 3): static musl, reuses the same
# pinned toolchain as hvx_seedrng ($GCC_MUSL/$MUSL_ROOT are set by the build prelude).
"$GCC_MUSL" -static -O2 -Wall -B "${MUSL_ROOT%/}/lib" hvx/fuzz-read.c -o build/fuzz-read
echo "built build/fuzz-read ($(stat -c%s build/fuzz-read) bytes, static musl)"

# --- 2n.2 the phase-2 ro-root source tree ---
chmod -R u+w build/rootfs-spread2-root 2>/dev/null || true
rm -rf build/rootfs-spread2-root
mkdir -p build/rootfs-spread2-root/bin build/rootfs-spread2-root/sbin build/rootfs-spread2-root/proc \
         build/rootfs-spread2-root/sys build/rootfs-spread2-root/dev build/rootfs-spread2-root/data \
         build/rootfs-spread2-root/tmp build/rootfs-spread2-root/etc build/rootfs-spread2-root/app \
         build/rootfs-spread2-root/nix/store
cp "$BB" build/rootfs-spread2-root/bin/busybox; chmod +x build/rootfs-spread2-root/bin/busybox
cp build/hvx_seedrng build/rootfs-spread2-root/bin/hvx_seedrng; chmod +x build/rootfs-spread2-root/bin/hvx_seedrng
cp "$SP2_BIN" build/rootfs-spread2-root/app/spread; chmod +x build/rootfs-spread2-root/app/spread
cp build/spread-drv build/rootfs-spread2-root/app/spread-drv; chmod +x build/rootfs-spread2-root/app/spread-drv
# The driver's crash op reads /b<i>.pid at the (READ-ONLY) root; symlink those to the runtime
# tmpfs /tmp so /init can actually write the broker pids there (a plain write to the ro-root fails).
ln -sf /tmp/b1.pid build/rootfs-spread2-root/b1.pid
ln -sf /tmp/b2.pid build/rootfs-spread2-root/b2.pid
echo "staging spread runtime closure onto the phase-2 ro-root..."
for p in $SP2_CLOSURE; do cp -a "$p" build/rootfs-spread2-root/nix/store/ 2>/dev/null || true; done
echo "staged spread closure: $(du -sm build/rootfs-spread2-root/nix/store 2>/dev/null | cut -f1) MiB"
SP2_LDLP=""
for p in $SP2_CLOSURE; do [ -d "$p/lib" ] && SP2_LDLP="${SP2_LDLP}${p}/lib:"; done
{ echo "LD_LIBRARY_PATH=$SP2_LDLP"; echo "SPREAD_BIN=/app/spread"; } > build/rootfs-spread2-root/etc/spread-env

# /sbin/init on the phase-2 ro-root: seed CRNG, loopback, mount the SHARED rw data disk, start TWO
# brokers (pids -> /b1.pid,/b2.pid for the driver's crash op), create queue q, run the FULL
# op-program with a broker-2 crash, map the mini-Jepsen verdict, then the stop marker.
cat > build/rootfs-spread2-root/sbin/init <<'S2INIT'
#!/bin/busybox sh
BB=/bin/busybox
[ -c /dev/null ] || $BB mount -t devtmpfs none /dev 2>/dev/null
$BB mount -t proc none /proc 2>/dev/null
$BB mount -t sysfs none /sys 2>/dev/null
$BB mount -t tmpfs none /tmp 2>/dev/null
. /etc/spread-env      # LD_LIBRARY_PATH, SPREAD_BIN
echo ""
echo "=================================================="
echo "  hydo: switched into the ext4 ro-root (SPREAD Phase 2 / two brokers)"
echo "  uname: $($BB uname -sr)"
echo "=================================================="
echo "hydo-spread-mid: real root is live"
/bin/hvx_seedrng 2>/dev/null || echo "hydo-spread: hvx_seedrng missing/failed"
$BB ip addr add 127.0.0.1/8 dev lo 2>/dev/null
$BB ip link set lo up 2>/dev/null || $BB ifconfig lo 127.0.0.1 up 2>/dev/null
MERR=$($BB mount -t ext2 -o rw /dev/vda2 /data 2>&1); MRC=$?
if [ $MRC -ne 0 ]; then
  echo "HYDO-ORACLE: VIOLATION mount data ($MERR)"
  echo "hydo-spread-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"; exec $BB sh
fi
echo "hydo-spread: mounted ext2 /dev/vda2 rw on /data (SHARED by both brokers)"
export LD_LIBRARY_PATH
export TOKIO_WORKER_THREADS=1
echo 0 > /proc/sys/kernel/randomize_va_space 2>/dev/null || true
echo "hydo-spread: launching B1 (127.0.0.1:8080) and B2 (127.0.0.1:8081) on shared /data"
$SPREAD_BIN --backend fs --data-dir /data --listen 127.0.0.1:8080 > /tmp/b1.log 2>&1 &
echo $! > /b1.pid
$SPREAD_BIN --backend fs --data-dir /data --listen 127.0.0.1:8081 > /tmp/b2.log 2>&1 &
echo $! > /b2.pid
echo "hydo-spread: B1 pid $($BB cat /b1.pid) ; B2 pid $($BB cat /b2.pid)"
/app/spread-drv health 8080
/app/spread-drv health 8081
echo "hydo-spread: both brokers healthy; creating queue q + running the op-program"
/app/spread-drv create 8080 q
# The op-program is built dynamically so the broker-crash op (X 2) is selectable via the kernel
# cmdline token spread_crash=0|1 WITHOUT rebuilding the rootfs — one rootfs, two workloads
# (Phase-3 aligned). X 2 SIGKILLs broker 2 (failover test; the driver reads /b2.pid). DEFAULT 0:
# on the fs backend, killing a broker stalls the SURVIVOR (it waits out the dead broker's shared-log
# lease) so the metric-gated drain cannot reach empty within budget — a very long, CPU-busy stall.
# The clean 2-broker consistency + determinism + disk-fault checks all use crash=0; crash=1 is
# opt-in for the failover experiment (see notes/task-phase2 for the stall diagnosis).
SPREAD_CRASH=0
for tok in $($BB cat /proc/cmdline 2>/dev/null); do
  case "$tok" in spread_crash=*) SPREAD_CRASH="${tok#spread_crash=}";; esac
done
echo "hydo-spread: op-program spread_crash=$SPREAD_CRASH"
{
  echo "S 1 g0 m0"
  echo "S 2 g1 m1"
  echo "R 1"
  echo "S 1 g0 m2"
  echo "N 2"
  echo "R 2"
  [ "$SPREAD_CRASH" = "1" ] && echo "X 2"
  echo "S 1 g0 m3"
  echo "R 1"
} > /tmp/prog.txt
/app/spread-drv > /tmp/drv.out 2>&1 < /tmp/prog.txt
echo "---- spread-drv output ----"
$BB cat /tmp/drv.out
echo "---- end spread-drv output ----"
if $BB grep -q '^BUG-ASSERT' /tmp/drv.out; then
  echo "HYDO-ORACLE: VIOLATION $($BB grep -m1 '^BUG-ASSERT' /tmp/drv.out)"
elif $BB grep -q '^spread: consistent' /tmp/drv.out; then
  echo "HYDO-ORACLE: OK consistent 2brokers"
else
  echo "HYDO-ORACLE: VIOLATION driver-inconclusive (no verdict line; possible hang)"
fi
$BB sync
$BB umount /data 2>/dev/null
echo "hydo-spread-marker: workload complete"
echo "hydo-boot-marker: launching shell on ttyS0"
exec $BB sh
S2INIT
chmod +x build/rootfs-spread2-root/sbin/init
find build/rootfs-spread2-root -exec touch --no-dereference -h -d @1600000000 {} + 2>/dev/null || true

# --- 2n.3 the byte-reproducible ext4 ro-root (p1) + ext2 rw-data (p2), distinct UUIDs/hash_seeds ---
SPREAD2_EXT4_MIB=320
SPREAD2_DATA_MIB=16
: > build/spread2-ext4.img; truncate -s $(( SPREAD2_EXT4_MIB * 1024 * 1024 )) build/spread2-ext4.img
mkext4_rootfs_reproducible build/spread2-ext4.img build/rootfs-spread2-root $SPREAD2_EXT4_MIB \
  bbbb2222-3333-4444-5555-666677778888 12345678-9abc-def0-1234-56789abcdef0
echo "built build/spread2-ext4.img ($(stat -c%s build/spread2-ext4.img) bytes, reproducible ext4 phase-2 ro-root)"
mkext2_reproducible build/spread2-data.img \
  eeee3333-4444-5555-6666-777788889999 0fedcba9-8765-4321-0fed-cba987654321
echo "built build/spread2-data.img ($(stat -c%s build/spread2-data.img) bytes, reproducible ext2 rw-data)"

# --- 2n.4 assemble the MBR-partitioned disk ---
S2P1_LBA=2048
S2P1_SECTORS=$(( SPREAD2_EXT4_MIB * 1024 * 1024 / 512 ))
S2P2_SECTORS=$(( SPREAD2_DATA_MIB * 1024 * 1024 / 512 ))
S2P2_LBA=$(( S2P1_LBA + S2P1_SECTORS ))
S2DISK_SECTORS=$(( S2P2_LBA + S2P2_SECTORS ))
: > build/spread2-disk.img
truncate -s $(( S2DISK_SECTORS * 512 )) build/spread2-disk.img
"$PY3" - build/spread2-disk.img "$S2P1_LBA" "$S2P1_SECTORS" "$S2P2_LBA" "$S2P2_SECTORS" <<'PYEOF'
import sys, struct
img, p1s, p1n, p2s, p2n = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
mbr = bytearray(512)
mbr[0x1B8:0x1BC] = b"\x0d\xd0\xea\x1d"
def part(off, lba, n, ptype=0x83):
    mbr[off] = 0x00
    mbr[off + 1:off + 4] = b"\xfe\xff\xff"
    mbr[off + 4] = ptype
    mbr[off + 5:off + 8] = b"\xfe\xff\xff"
    struct.pack_into("<I", mbr, off + 8, lba)
    struct.pack_into("<I", mbr, off + 12, n)
part(0x1BE, p1s, p1n)
part(0x1CE, p2s, p2n)
mbr[0x1FE] = 0x55; mbr[0x1FF] = 0xAA
with open(img, "r+b") as f:
    f.write(mbr)
PYEOF
dd if=build/spread2-ext4.img of=build/spread2-disk.img bs=512 seek=$S2P1_LBA conv=notrunc status=none
dd if=build/spread2-data.img of=build/spread2-disk.img bs=512 seek=$S2P2_LBA conv=notrunc status=none
echo "built build/spread2-disk.img ($(stat -c%s build/spread2-disk.img) bytes: MBR + p1 ext4 ro-root @ LBA $S2P1_LBA + p2 ext2 rw-data @ LBA $S2P2_LBA)"
echo "  sha256(spread2-ext4.img)=$(sha256sum build/spread2-ext4.img | cut -d' ' -f1)"
echo "  sha256(spread2-disk.img)=$(sha256sum build/spread2-disk.img | cut -d' ' -f1)"

# --- 2n.5 the switch_root initramfs (same module stack as §2m) ---
rm -rf build/initramfs-spread2
mkdir -p build/initramfs-spread2/bin build/initramfs-spread2/proc build/initramfs-spread2/sys \
         build/initramfs-spread2/dev build/initramfs-spread2/newroot build/initramfs-spread2/lib/modules
cp "$BB" build/initramfs-spread2/bin/busybox; chmod +x build/initramfs-spread2/bin/busybox
for m in $ROOTMODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  [ -z "$src" ] && { echo "note: spread2-initramfs module $m.ko.xz not found (built-in?)" >&2; continue; }
  xz -dc "$src" > "build/initramfs-spread2/lib/modules/$m.ko"
done
cat > build/initramfs-spread2/init <<'S2RINITRAMFS'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
echo ""
echo "=================================================="
echo "  hydo: initramfs reached (SPREAD Phase 2 ext4 ro-root + switch_root)"
echo "=================================================="
ROOT=/dev/vda1
for tok in $(cat /proc/cmdline); do case "$tok" in root=*) ROOT=${tok#root=};; esac; done
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk crc16 crc32c_generic mbcache jbd2 ext4 ext2; do
  [ -f /lib/modules/$m.ko ] || continue
  insmod /lib/modules/$m.ko 2>/dev/null && echo "hydo-spread-boot: insmod $m ok" || echo "hydo-spread-boot: insmod $m (skipped/failed)"
done
n=0
while [ ! -b "$ROOT" ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
if [ ! -b "$ROOT" ]; then
  echo "HYDO-ORACLE: VIOLATION no root device"
  echo "hydo-spread-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"; exec /bin/busybox sh
fi
mkdir -p /newroot
if ! mount -t ext4 -o ro,noload "$ROOT" /newroot 2>/dev/null; then
  echo "HYDO-ORACLE: VIOLATION mount root"
  echo "hydo-spread-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"; exec /bin/busybox sh
fi
echo "hydo-spread-boot: mounted $ROOT ext4 read-only at /newroot; switching root"
mkdir -p /newroot/dev /newroot/proc /newroot/sys
mount --move /dev /newroot/dev 2>/dev/null || mount -o move /dev /newroot/dev 2>/dev/null || \
  mount -t devtmpfs none /newroot/dev 2>/dev/null || true
exec switch_root /newroot /sbin/init
S2RINITRAMFS
chmod +x build/initramfs-spread2/init
reproducible_cpio build/initramfs-spread2 build/initramfs-spread2.cpio.gz
echo "built build/initramfs-spread2.cpio.gz ($(stat -c%s build/initramfs-spread2.cpio.gz) bytes)"

# ---- 2o. the SPREAD PHASE-3 SEARCH assets (SPREAD_ON_HVX Phase 3): the two-broker guest driven by
#      the autonomous serial-fed... no, SYNCHRONOUS-fuzz-input-port snapshot-restore search loop.
#      DISTINCT rootfs + disk (spread3-*) so §2n (Phase 2, fingerprint 7b576827407c0644) is untouched.
#      Uses the UNCHANGED driver (spread-drv) + a guest reader (fuzz-read) for the fuzz-input port; the
#      search /init prints SPREAD-SNAPSHOT-READY (the host's snapshot boundary), then per restore
#      fuzz-read pulls the host-staged program from ports 0x510/0x511 and the driver runs it.
#        build/rootfs-spread3-root/     ro-root: busybox + hvx_seedrng + fuzz-read + /app/{spread,spread-drv}
#        build/spread3-{ext4,data,disk}.img + build/initramfs-spread3.cpio.gz
SP3_BIN="${HYDO_SPREAD_BIN:-$HOME/spread/target/release/spread}"
[ -x "$SP3_BIN" ] || { echo "error: spread binary not found at $SP3_BIN" >&2; exit 1; }
SP3_BIN=$(realpath "$SP3_BIN")
SP3_INTERP=$(patchelf --print-interpreter "$SP3_BIN" 2>/dev/null || true)
[ -n "$SP3_INTERP" ] || SP3_INTERP=$(readelf -l "$SP3_BIN" 2>/dev/null | grep -oE '/nix/store/[^]]+' | head -1 || true)
SP3_GLIBC_ROOT=$(printf '%s\n' "$SP3_INTERP" | sed -E 's#(/nix/store/[^/]+)/.*#\1#')
SP3_LIB_ROOTS=$(ldd "$SP3_BIN" 2>/dev/null | grep -oE '/nix/store/[^/ ]+' | sed -E 's#(/nix/store/[^/]+).*#\1#' | sort -u)
SP3_CLOSURE=$(nix-store -qR "$SP3_GLIBC_ROOT" $SP3_LIB_ROOTS 2>/dev/null | sort -u)
[ -n "$SP3_CLOSURE" ] || { echo "error: spread closure empty" >&2; exit 1; }
GO3_BIN="${HYDO_GO:-}"
if [ -z "$GO3_BIN" ]; then
  GO3_BIN=/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go
  [ -x "$GO3_BIN" ] || GO3_BIN=$(ls /nix/store/*go-1.2*/bin/go 2>/dev/null | head -1 || true)
  [ -x "$GO3_BIN" ] || GO3_BIN=$(command -v go || true)
fi
[ -x "$GO3_BIN" ] || { echo "error: no go found" >&2; exit 1; }
SP3_DRV_OUT="$(pwd)/build/spread-drv"
( cd hvx/spread-drv && CGO_ENABLED=0 GOFLAGS=-trimpath "$GO3_BIN" build -o "$SP3_DRV_OUT" . )
# fuzz-read (guest reader for the synchronous port) — static musl, same toolchain as hvx_seedrng.
"$GCC_MUSL" -static -O2 -Wall -B "${MUSL_ROOT%/}/lib" hvx/fuzz-read.c -o build/fuzz-read
echo "built build/fuzz-read ($(stat -c%s build/fuzz-read) bytes) for the search rootfs"

chmod -R u+w build/rootfs-spread3-root 2>/dev/null || true
rm -rf build/rootfs-spread3-root
mkdir -p build/rootfs-spread3-root/bin build/rootfs-spread3-root/sbin build/rootfs-spread3-root/proc \
         build/rootfs-spread3-root/sys build/rootfs-spread3-root/dev build/rootfs-spread3-root/data \
         build/rootfs-spread3-root/tmp build/rootfs-spread3-root/etc build/rootfs-spread3-root/app \
         build/rootfs-spread3-root/nix/store
cp "$BB" build/rootfs-spread3-root/bin/busybox; chmod +x build/rootfs-spread3-root/bin/busybox
cp build/hvx_seedrng build/rootfs-spread3-root/bin/hvx_seedrng; chmod +x build/rootfs-spread3-root/bin/hvx_seedrng
cp build/fuzz-read build/rootfs-spread3-root/bin/fuzz-read; chmod +x build/rootfs-spread3-root/bin/fuzz-read
cp "$SP3_BIN" build/rootfs-spread3-root/app/spread; chmod +x build/rootfs-spread3-root/app/spread
cp build/spread-drv build/rootfs-spread3-root/app/spread-drv; chmod +x build/rootfs-spread3-root/app/spread-drv
echo "staging spread runtime closure onto the phase-3 ro-root..."
for p in $SP3_CLOSURE; do cp -a "$p" build/rootfs-spread3-root/nix/store/ 2>/dev/null || true; done
echo "staged spread closure: $(du -sm build/rootfs-spread3-root/nix/store 2>/dev/null | cut -f1) MiB"
SP3_LDLP=""
for p in $SP3_CLOSURE; do [ -d "$p/lib" ] && SP3_LDLP="${SP3_LDLP}${p}/lib:"; done
{ echo "LD_LIBRARY_PATH=$SP3_LDLP"; echo "SPREAD_BIN=/app/spread"; } > build/rootfs-spread3-root/etc/spread-env

cat > build/rootfs-spread3-root/sbin/init <<'S3INIT'
#!/bin/busybox sh
BB=/bin/busybox
[ -c /dev/null ] || $BB mount -t devtmpfs none /dev 2>/dev/null
$BB mount -t proc none /proc 2>/dev/null
$BB mount -t sysfs none /sys 2>/dev/null
$BB mount -t tmpfs none /tmp 2>/dev/null
. /etc/spread-env
echo ""
echo "=================================================="
echo "  hydo: switched into the ext4 ro-root (SPREAD Phase 3 / SEARCH loop)"
echo "  uname: $($BB uname -sr)"
echo "=================================================="
echo "hydo-spread-mid: real root is live"
/bin/hvx_seedrng 2>/dev/null || echo "hydo-spread: hvx_seedrng missing/failed"
$BB ip addr add 127.0.0.1/8 dev lo 2>/dev/null
$BB ip link set lo up 2>/dev/null || $BB ifconfig lo 127.0.0.1 up 2>/dev/null
MERR=$($BB mount -t ext2 -o rw /dev/vda2 /data 2>&1); MRC=$?
if [ $MRC -ne 0 ]; then
  echo "HYDO-ORACLE: VIOLATION mount data ($MERR)"
  echo "hydo-spread-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"; exec $BB sh
fi
echo "hydo-spread: mounted ext2 /dev/vda2 rw on /data (SHARED by both brokers)"
export LD_LIBRARY_PATH
export TOKIO_WORKER_THREADS=1
echo 0 > /proc/sys/kernel/randomize_va_space 2>/dev/null || true
# hydo-16 split test: spread_mode=fuzzonly boots real Linux + JUST fuzz-read (NO brokers, NO driver,
# NO oracle) -> real Linux + one busy-poller process. fuzz-read prints FUZZREADY (the snapshot
# boundary) + spins on the port; per restore the host stages a program, fuzz-read reads it, we echo it
# (FUZZONLY-READ) + SPREAD-RUN-DONE. Decides: real-Linux+fuzz-read RESUMES a deep snapshot (byte
# echoed) -> bug is the brokers/driver; or HANGS (idle-HLT) -> real-Linux+fuzz-read scheduling.
SPREAD_FMODE=full
for tok in $($BB cat /proc/cmdline 2>/dev/null); do
  case "$tok" in spread_mode=*) SPREAD_FMODE="${tok#spread_mode=}";; esac
done
if [ "$SPREAD_FMODE" = "fuzzonly" ]; then
  echo "hydo-spread: FUZZONLY mode (no brokers/driver; just fuzz-read + echo)"
  /bin/fuzz-read > /tmp/prog.txt
  echo "FUZZONLY-READ=[$($BB cat /tmp/prog.txt)]"
  echo "SPREAD-RUN-DONE"
  $BB halt -f 2>/dev/null; $BB poweroff -f 2>/dev/null; $BB reboot -f 2>/dev/null; exec $BB sh
fi
echo "hydo-spread: launching B1 (127.0.0.1:8080) and B2 (127.0.0.1:8081) on shared /data"
$SPREAD_BIN --backend fs --data-dir /data --listen 127.0.0.1:8080 > /tmp/b1.log 2>&1 &
echo $! > /b1.pid
$SPREAD_BIN --backend fs --data-dir /data --listen 127.0.0.1:8081 > /tmp/b2.log 2>&1 &
echo $! > /b2.pid
/app/spread-drv health 8080
/app/spread-drv health 8081
echo "hydo-spread: both brokers healthy; creating queue q"
/app/spread-drv create 8080 q
# Phase-3 SEARCH loop: per snapshot-restore, fuzz-read pulls a host-staged op-program from the
# SYNCHRONOUS fuzz-input port (0x510 data / 0x511 status), and the UNCHANGED driver runs it. The
# host snapshots at SPREAD-SNAPSHOT-READY (fuzz-read then busy-spins on the status port so HVX_RUN
# keeps returning with the idle heartbeat ON), then stages [u32 len][program] via HVX_SET_FUZZ_INPUT
# per restore. Purely synchronous port I/O -> no idle -> byte-deterministic; the SPREAD-RUN-DONE
# marker (printed by /init after the driver) ends each run for the host loop.
# fuzz-read prints FUZZREADY (the snapshot boundary) to the console right before it spins on the
# port, so the host snapshots the guest at a clean busy point (loaded + about to read the port).
/bin/fuzz-read > /tmp/prog.txt
echo "FUZZ-GOT $($BB wc -c < /tmp/prog.txt)B"
/app/spread-drv healthz 8080; echo "HEALTH8080-rc=$?"
/app/spread-drv < /tmp/prog.txt > /dev/console 2>&1
echo "SPREAD-RUN-DONE"
# DETERMINISTIC STOP: halt right after DONE so the run ends at a FIXED instruction (kernel halt ->
# HLT with IF=0 -> the module returns EXIT_HLT, no idle-heartbeat ticks). The host runs until that
# HLT and fingerprints THERE, before any variable idle-tail timekeeping (jiffies) advances -- the
# sound fix for the non-deterministic idle-tail fingerprint (RAM-diff showed only an 8-byte jiffies
# delta; console + rw-disk were already identical).
$BB halt -f 2>/dev/null; $BB poweroff -f 2>/dev/null; $BB reboot -f 2>/dev/null; exec $BB sh
S3INIT
chmod +x build/rootfs-spread3-root/sbin/init
ln -sf /tmp/b1.pid build/rootfs-spread3-root/b1.pid
ln -sf /tmp/b2.pid build/rootfs-spread3-root/b2.pid
find build/rootfs-spread3-root -exec touch --no-dereference -h -d @1600000000 {} + 2>/dev/null || true

SPREAD3_EXT4_MIB=320
: > build/spread3-ext4.img; truncate -s $(( SPREAD3_EXT4_MIB * 1024 * 1024 )) build/spread3-ext4.img
mkext4_rootfs_reproducible build/spread3-ext4.img build/rootfs-spread3-root $SPREAD3_EXT4_MIB \
  cccc3333-4444-5555-6666-777788889999 abcdef01-2345-6789-abcd-ef0123456781
echo "built build/spread3-ext4.img ($(stat -c%s build/spread3-ext4.img) bytes)"
mkext2_reproducible build/spread3-data.img \
  ffff4444-5555-6666-7777-888899990000 fedcba98-7654-3210-fedc-ba9876543211
echo "built build/spread3-data.img ($(stat -c%s build/spread3-data.img) bytes)"
S3P1_LBA=2048
S3P1_SECTORS=$(( SPREAD3_EXT4_MIB * 1024 * 1024 / 512 ))
S3P2_SECTORS=$(( 16 * 1024 * 1024 / 512 ))
S3P2_LBA=$(( S3P1_LBA + S3P1_SECTORS ))
S3DISK_SECTORS=$(( S3P2_LBA + S3P2_SECTORS ))
: > build/spread3-disk.img
truncate -s $(( S3DISK_SECTORS * 512 )) build/spread3-disk.img
"$PY3" - build/spread3-disk.img "$S3P1_LBA" "$S3P1_SECTORS" "$S3P2_LBA" "$S3P2_SECTORS" <<'PYEOF'
import sys, struct
img, p1s, p1n, p2s, p2n = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
mbr = bytearray(512)
mbr[0x1B8:0x1BC] = b"\x0d\xd0\xea\x1d"
def part(off, lba, n, ptype=0x83):
    mbr[off] = 0x00; mbr[off+1:off+4] = b"\xfe\xff\xff"; mbr[off+4] = ptype; mbr[off+5:off+8] = b"\xfe\xff\xff"
    struct.pack_into("<I", mbr, off+8, lba); struct.pack_into("<I", mbr, off+12, n)
part(0x1BE, p1s, p1n); part(0x1CE, p2s, p2n)
mbr[0x1FE] = 0x55; mbr[0x1FF] = 0xAA
open(img, "r+b").write(mbr)
PYEOF
dd if=build/spread3-ext4.img of=build/spread3-disk.img bs=512 seek=$S3P1_LBA conv=notrunc status=none
dd if=build/spread3-data.img of=build/spread3-disk.img bs=512 seek=$S3P2_LBA conv=notrunc status=none
echo "built build/spread3-disk.img ($(stat -c%s build/spread3-disk.img) bytes: MBR + p1 ro-root + p2 rw-data)"
echo "  sha256(spread3-disk.img)=$(sha256sum build/spread3-disk.img | cut -d' ' -f1)"
rm -rf build/initramfs-spread3
mkdir -p build/initramfs-spread3/bin build/initramfs-spread3/proc build/initramfs-spread3/sys \
         build/initramfs-spread3/dev build/initramfs-spread3/newroot build/initramfs-spread3/lib/modules
cp "$BB" build/initramfs-spread3/bin/busybox; chmod +x build/initramfs-spread3/bin/busybox
for m in $ROOTMODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1 || true)
  [ -z "$src" ] && continue
  xz -dc "$src" > "build/initramfs-spread3/lib/modules/$m.ko"
done
cat > build/initramfs-spread3/init <<'S3RINITRAMFS'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mount -t devtmpfs none /dev 2>/dev/null
echo ""
echo "  hydo: initramfs reached (SPREAD Phase 3 SEARCH ext4 ro-root + switch_root)"
ROOT=/dev/vda1
for tok in $(cat /proc/cmdline); do case "$tok" in root=*) ROOT=${tok#root=};; esac; done
for m in virtio virtio_ring virtio_pci_legacy_dev virtio_pci_modern_dev virtio_pci virtio_blk crc16 crc32c_generic mbcache jbd2 ext4 ext2; do
  [ -f /lib/modules/$m.ko ] || continue
  insmod /lib/modules/$m.ko 2>/dev/null && echo "hydo-spread-boot: insmod $m ok" || echo "hydo-spread-boot: insmod $m (skipped/failed)"
done
n=0
while [ ! -b "$ROOT" ] && [ $n -lt 30 ]; do n=$((n+1)); sleep 1; done
[ -b "$ROOT" ] || { echo "HYDO-ORACLE: VIOLATION no root device"; echo "hydo-spread-marker: workload complete"; exec /bin/busybox sh; }
mkdir -p /newroot
mount -t ext4 -o ro,noload "$ROOT" /newroot 2>/dev/null || { echo "HYDO-ORACLE: VIOLATION mount root"; echo "hydo-spread-marker: workload complete"; exec /bin/busybox sh; }
echo "hydo-spread-boot: mounted $ROOT ext4 read-only at /newroot; switching root"
mkdir -p /newroot/dev /newroot/proc /newroot/sys
mount --move /dev /newroot/dev 2>/dev/null || mount -o move /dev /newroot/dev 2>/dev/null || \
  mount -t devtmpfs none /newroot/dev 2>/dev/null || true
exec switch_root /newroot /sbin/init
S3RINITRAMFS
chmod +x build/initramfs-spread3/init
reproducible_cpio build/initramfs-spread3 build/initramfs-spread3.cpio.gz
echo "built build/initramfs-spread3.cpio.gz ($(stat -c%s build/initramfs-spread3.cpio.gz) bytes)"
