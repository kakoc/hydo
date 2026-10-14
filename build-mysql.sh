#!/usr/bin/env bash
# build-mysql.sh — builder for the MySQL flagship guest (Milestone 34): an UNMODIFIED MySQL
# 8.4 server + the stock `mysql` client, co-located over 127.0.0.1, InnoDB on a journaled
# ext4 data disk. Same model as the Kafka guest (build-linux.sh §2l): the big binary
# closure lives on an ext4 READ-ONLY root (p1, excluded from the snapshot), the data
# directory on a small rw disk (p2 = the fault target). The switch_root initramfs is the
# Kafka one (build/initramfs-kafka.cpio.gz, built by build-kafka.sh) -- it is generic.
# The prelude (sections 1-5) is the same pinned, byte-reproducible one build-kafka.sh uses.
# Run on the build box:  ./dev.sh "bash ./build-mysql.sh"
set -euo pipefail
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

# --- 4. build hvx_seedrng (static musl), staged onto the ro-root below ---
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
"$GCC_MUSL" -static -O2 -Wall -B "${MUSL_ROOT%/}/lib" hvx/hvx_seedrng.c -o build/hvx_seedrng
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
  SOURCE_DATE_EPOCH=1600000000 "$FAKETIME_BIN" "$FAKE_TS" "$MKE2FS" -q -F -t ext2 -b 1024 \
    -U "$uuid" \
    -E hash_seed="$seed",lazy_itable_init=0,lazy_journal_init=0 \
    "$out"
}
mkext4_rootfs_reproducible() {   # $1=out $2=srcdir $3=size_MiB $4=uuid $5=hash_seed
  local out="$1" src="$2" mib="$3" uuid="$4" seed="$5"
  : > "$out"; truncate -s $(( mib * 1024 * 1024 )) "$out"
  SOURCE_DATE_EPOCH=1600000000 "$FAKETIME_BIN" "$FAKE_TS" "$MKE2FS" -q -F -t ext4 -b 4096 \
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

# --- 6. the MySQL guest ---
MYSQL="${HYDO_MYSQL:-/nix/store/aw6ljbiwgds7yw8x2jc3340dm46s7i2k-mysql-8.4.10}"
[ -x "$MYSQL/bin/mysqld" ] || { echo "error: no MySQL at $MYSQL (set HYDO_MYSQL=/nix/store/...-mysql-8.4.x)" >&2; exit 1; }
MYSQL_NKEYS=30
echo "mysql: $MYSQL ($("$MYSQL/bin/mysqld" --version 2>/dev/null))"

# --- 6.1 the ro-root tree. The closure (~780 MiB) is staged once and reused. ---
MR=build/rootfs-mysql-root
if [ -x "$MR$MYSQL/bin/mysqld" ]; then
  echo "mysql ro-root: reusing the staged closure (remove $MR to force fresh)"
  chmod -R u+w "${MR:?}/bin" "${MR:?}/sbin" "${MR:?}/etc" 2>/dev/null || true
  rm -rf "${MR:?}/bin" "${MR:?}/sbin" "${MR:?}/etc"
else
  chmod -R u+w "${MR:?}" 2>/dev/null || true
  rm -rf "${MR:?}"; mkdir -p "$MR/nix/store"
  echo "staging the MySQL closure ($(nix-store -qR "$MYSQL" | wc -l) store paths) onto the ro-root..."
  for p in $(nix-store -qR "$MYSQL"); do cp -a "$p" "$MR/nix/store/" 2>/dev/null || true; done
  echo "staged: $(du -sm "$MR/nix/store" | cut -f1) MiB"
fi
mkdir -p "$MR/bin" "$MR/sbin" "$MR/proc" "$MR/sys" "$MR/dev" "$MR/data" "$MR/tmp" "$MR/etc"
cp "$BB" "$MR/bin/busybox"; chmod +x "$MR/bin/busybox"
cp build/hvx_seedrng "$MR/bin/hvx_seedrng"; chmod +x "$MR/bin/hvx_seedrng"
cat > "$MR/etc/mysql-env" <<ENV
MYSQL=$MYSQL
NKEYS=$MYSQL_NKEYS
ENV
printf '127.0.0.1 localhost\n' > "$MR/etc/hosts"
printf 'root:x:0:0:root:/tmp:/bin/sh\n' > "$MR/etc/passwd"
printf 'root:x:0:\n' > "$MR/etc/group"

# my.cnf: a small, single-purpose InnoDB server. Durability knobs left at their defaults
# (doublewrite on, fsync flush method); the ONE knob under test,
# innodb_flush_log_at_trx_commit, is appended by /sbin/init from the kernel cmdline.
cat > "$MR/etc/my.cnf" <<'CNF'
[mysqld]
user=root
datadir=/data/mysql
socket=/tmp/mysql.sock
pid-file=/tmp/mysqld.pid
log-error=/tmp/mysqld.err
bind-address=127.0.0.1
port=3306
mysqlx=OFF
skip-name-resolve
skip-log-bin
performance_schema=OFF
auto_generate_certs=OFF
caching_sha2_password_auto_generate_rsa_keys=OFF
innodb_buffer_pool_size=64M
innodb_redo_log_capacity=8M
innodb_use_native_aio=OFF
innodb_flush_method=fsync
innodb_read_io_threads=1
innodb_write_io_threads=1
innodb_page_cleaners=1
innodb_purge_threads=1
max_connections=8
table_open_cache=64
default-time-zone=+00:00
init-file=/etc/mysql-init.sql
CNF
# A TCP-reachable account (root@localhost does not match 127.0.0.1 under skip-name-resolve).
cat > "$MR/etc/mysql-init.sql" <<'SQL'
CREATE USER IF NOT EXISTS 'hydo'@'%' IDENTIFIED BY '';
GRANT ALL PRIVILEGES ON *.* TO 'hydo'@'%';
SQL

cat > "$MR/sbin/init" <<'MINIT'
#!/bin/busybox sh
BB=/bin/busybox
[ -c /dev/null ] || $BB mount -t devtmpfs none /dev 2>/dev/null
$BB mount -t proc none /proc 2>/dev/null
$BB mount -t sysfs none /sys 2>/dev/null
$BB mount -t tmpfs none /tmp 2>/dev/null
. /etc/mysql-env      # MYSQL=/nix/store/...-mysql-8.4.x  NKEYS=30
export HOME=/tmp TZ=UTC LANG=C
echo ""
echo "=================================================="
echo "  hydo: switched into the ext4 ro-root (M34 MySQL)"
echo "  uname: $($BB uname -sr)"
echo "=================================================="
echo "hydo-mysql-mid: real root is live"
PHASE=solo; FLUSH=off
for tok in $($BB cat /proc/cmdline 2>/dev/null); do
  case "$tok" in
    hydo_phase=*) PHASE="${tok#hydo_phase=}";;
    hydo_flush=*) FLUSH="${tok#hydo_flush=}";;
  esac
done
UP() { $BB cut -d' ' -f1 /proc/uptime 2>/dev/null; }
# hydo_flush=on  -> innodb_flush_log_at_trx_commit=1 (the default: redo fsync'd at every
#                   COMMIT -> an acked transaction survives a power loss)
# hydo_flush=off -> =2 (redo WRITTEN at commit but fsync'd about once a second -> the
#                   documented window: acked transactions can be lost on power loss)
if [ "$FLUSH" = "on" ]; then TRX=1; else TRX=2; fi
echo "hydo-mysql: phase=$PHASE flush=$FLUSH (innodb_flush_log_at_trx_commit=$TRX) nkeys=$NKEYS"
/bin/hvx_seedrng 2>/dev/null || echo "hydo-mysql: hvx_seedrng missing/failed"
$BB ifconfig lo 127.0.0.1 netmask 255.0.0.0 up 2>/dev/null
$BB ip link set lo up 2>/dev/null
echo "hydo-mysql: loopback up"
MERR=$($BB mount -t ext4 -o rw /dev/vda2 /data 2>&1); MRC=$?
if [ $MRC -ne 0 ]; then
  echo "HYDO-ORACLE: ERROR mount data ($MERR)"
  echo "hydo-mysql-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"; while true; do $BB sleep 100; done
fi
echo "hydo-mysql: mounted ext4 /dev/vda2 rw on /data (datadir)"
$BB cp /etc/my.cnf /tmp/my.cnf
echo "innodb_flush_log_at_trx_commit=$TRX" >> /tmp/my.cnf
SQL() { "$MYSQL/bin/mysql" --no-defaults --protocol=TCP -h127.0.0.1 -P3306 -uhydo --ssl-mode=DISABLED -N -B "$@"; }

start_server() {
  if [ ! -d /data/mysql/mysql ]; then
    echo "hydo-mysql: initializing the data directory (uptime=$(UP)s)"
    "$MYSQL/bin/mysqld" --defaults-file=/tmp/my.cnf --initialize-insecure > /tmp/init.log 2>&1; IRC=$?
    echo "hydo-mysql: initialize rc=$IRC (uptime=$(UP)s)"
    [ $IRC -ne 0 ] && { $BB tail -12 /tmp/init.log; $BB tail -12 /tmp/mysqld.err 2>/dev/null; return 1; }
  fi
  echo "hydo-mysql: launching REAL mysqld (uptime=$(UP)s)"
  "$MYSQL/bin/mysqld" --defaults-file=/tmp/my.cnf > /tmp/mysqld.out 2>&1 &
  MPID=$!
  n=0
  while [ $n -lt 600 ]; do
    if SQL -e "SELECT 1" > /dev/null 2>/tmp/ping.err; then
      echo "hydo-mysql: server READY after ${n} polls (uptime=$(UP)s)"; return 0
    fi
    $BB kill -0 $MPID 2>/dev/null || { echo "hydo-mysql: server DIED early (uptime=$(UP)s)"; $BB tail -15 /tmp/mysqld.err 2>/dev/null; $BB tail -5 /tmp/mysqld.out 2>/dev/null; return 1; }
    n=$((n+1)); $BB sleep 1
  done
  echo "hydo-mysql: server NOT ready after $n polls (uptime=$(UP)s)"; $BB tail -15 /tmp/mysqld.err 2>/dev/null; $BB tail -3 /tmp/ping.err 2>/dev/null; return 1
}

make_schema() {
  SQL -e "CREATE DATABASE IF NOT EXISTS hydo; CREATE TABLE IF NOT EXISTS hydo.t (k INT PRIMARY KEY, v BIGINT NOT NULL, pad CHAR(200) NOT NULL) ENGINE=InnoDB" 2>&1
  # Make the SCHEMA durable on its own (sync -> a device FLUSH), so that the fault window
  # below contains only the row commits under test, not the table's existence.
  $BB sync
  echo "hydo-mysql: schema created + synced (uptime=$(UP)s)"
}

commit_rows() {   # NKEYS single-statement autocommit transactions over TCP loopback
  k=0; : > /tmp/rows.sql
  while [ $k -lt $NKEYS ]; do
    echo "INSERT INTO hydo.t VALUES ($k, $((100000 + k)), REPEAT('x',200));" >> /tmp/rows.sql
    k=$((k+1))
  done
  echo "SELECT COUNT(*) FROM hydo.t;" >> /tmp/rows.sql
  ACKED=$(SQL < /tmp/rows.sql 2>/tmp/client.err); CRC=$?
  echo "hydo-mysql: client rc=$CRC (uptime=$(UP)s)"
  echo "HYDO-MYSQL: committed acked=${ACKED:-0}/$NKEYS autocommit"
  [ $CRC -ne 0 ] && $BB tail -5 /tmp/client.err
}

oracle() {        # every acked row must read back with its committed value
  SQL -e "SELECT k, v FROM hydo.t ORDER BY k" > /tmp/rows.out 2>/tmp/oracle.err; ORC=$?
  if [ $ORC -ne 0 ]; then
    echo "HYDO-ORACLE: VIOLATION table unreadable ($($BB head -1 /tmp/oracle.err))"; return
  fi
  k=0
  while [ $k -lt $NKEYS ]; do
    got=$($BB awk -v k=$k '$1==k {print $2}' /tmp/rows.out)
    if [ "$got" != "$((100000 + k))" ]; then
      echo "HYDO-ORACLE: VIOLATION key=$k expected=$((100000 + k)) got=${got:-MISSING}"; return
    fi
    k=$((k+1))
  done
  echo "HYDO-ORACLE: OK keys=$NKEYS durable"
}

case "$PHASE" in
  workload)
    start_server || { echo "HYDO-ORACLE: ERROR server start"; echo "hydo-mysql-workload-done acked=0"; while true; do $BB sleep 100; done; }
    make_schema
    commit_rows
    # NO sync here: it would FLUSH the volatile write cache and close the window under test.
    $BB kill -STOP $MPID 2>/dev/null
    echo "hydo-mysql: mysqld STOPped (durability frozen at the acked instant)"
    echo "hydo-mysql-workload-done"
    while true; do $BB sleep 100; done
    ;;
  oracle)
    echo "hydo-mysql: recovering from log"      # branch mid-point: before InnoDB crash recovery
    start_server || { echo "HYDO-ORACLE: VIOLATION server cannot recover (committed data unrecoverable)"; echo "hydo-mysql-marker: workload complete"; while true; do $BB sleep 100; done; }
    oracle
    echo "hydo-mysql-marker: workload complete"
    echo "hydo-boot-marker: launching shell on ttyS0"
    exec $BB sh
    ;;
  *)
    start_server || { echo "HYDO-ORACLE: ERROR server start"; echo "hydo-mysql-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"; while true; do $BB sleep 100; done; }
    make_schema
    commit_rows
    oracle
    $BB sync
    echo "hydo-mysql-marker: workload complete"
    echo "hydo-boot-marker: launching shell on ttyS0"
    exec $BB sh
    ;;
esac
MINIT
chmod +x "$MR/sbin/init"
find "$MR" -exec touch --no-dereference -h -d @1600000000 {} + 2>/dev/null || true

# --- 6.2 images: ext4 ro-root (p1) + journaled ext4 data disk (p2) behind a hand-made MBR ---
MYSQL_EXT4_MIB=1280
MYSQL_DATA_MIB=256

# zero_ext_times <image> <all|nonsource>: freeze every time field mke2fs writes. The
# libfaketime clock is NOT frozen (it advances in real time), so a build that straddles a
# wall-clock second stamps different values. Observed for the ro-root: two builds 1.7 s
# apart differed in exactly 1736 bytes, every one of them an inode i_crtime (mkext4_
# rootfs_reproducible zeroes only i_ctime). Zeroes s_mtime/s_wtime/s_lastcheck/
# s_mkfs_time in the primary superblock AND every backup copy, and per inode i_dtime, the
# *_extra fields and i_crtime/i_crtime_extra. "all" also zeroes i_atime/i_ctime/i_mtime
# (the data disk); "nonsource" keeps atime/mtime, which mke2fs -d copies from the source
# tree (already frozen by touch) and are meaningful on the ro-root.
zero_ext_times() {
  "$PY3" - "$1" "$2" <<'PYEOF'
import sys, struct, mmap
f = open(sys.argv[1], "r+b"); mm = mmap.mmap(f.fileno(), 0)
every = sys.argv[2] == "all"
u16 = lambda o: struct.unpack_from("<H", mm, o)[0]
u32 = lambda o: struct.unpack_from("<I", mm, o)[0]
sb = 1024
inodes = u32(sb + 0x00); bs = 1024 << u32(sb + 0x18)
ipg = u32(sb + 0x28); isize = u16(sb + 0x58) or 128
bpg = u32(sb + 0x20)
gdtb = 2 if bs == 1024 else 1
ng = (inodes + ipg - 1) // ipg
for g in range(ng):                                 # primary + backup superblocks
    o = sb if g == 0 else g * bpg * bs + (1024 if bs == 1024 else 0)
    if o + 0x3A <= len(mm) and u16(o + 0x38) == 0xEF53:
        for t in (0x2C, 0x30, 0x40, 0x108):         # s_mtime s_wtime s_lastcheck s_mkfs_time
            struct.pack_into("<I", mm, o + t, 0)
fields = ((0x08, 0x0C, 0x10, 0x14) if every else (0x0C, 0x14))   # a/c/m/dtime | c/dtime
for g in range(ng):
    itbl = u32(gdtb * bs + g * 32 + 0x08)
    for i in range(ipg):
        if g * ipg + i >= inodes:
            break
        io = itbl * bs + i * isize
        for o in fields:
            struct.pack_into("<I", mm, io + o, 0)
        if isize > 128:                             # *_extra times + i_crtime + i_crtime_extra
            for o in (0x84, 0x88, 0x8C, 0x90, 0x94):
                struct.pack_into("<I", mm, io + o, 0)
mm.flush(); mm.close(); f.close()
PYEOF
}

mkext4_rootfs_reproducible build/mysql-ext4.img "$MR" $MYSQL_EXT4_MIB \
  aaaa3434-bbbb-cccc-dddd-eeeeffff0034 34343434-1111-2222-3333-444455556666
zero_ext_times build/mysql-ext4.img nonsource
echo "built build/mysql-ext4.img ($(stat -c%s build/mysql-ext4.img) bytes, time-normalized)"

: > build/mysql-data.img; truncate -s $(( MYSQL_DATA_MIB * 1024 * 1024 )) build/mysql-data.img
SOURCE_DATE_EPOCH=1600000000 "$FAKETIME_BIN" "$FAKE_TS" "$MKE2FS" -q -F -t ext4 -b 4096 \
  -O ^metadata_csum,^64bit,^flex_bg,^resize_inode,has_journal -J size=16 \
  -U 34340000-2222-3333-4444-5555eeee0034 \
  -E hash_seed=34345555-4444-3333-2222-1111dddd0034,lazy_itable_init=0,lazy_journal_init=0 \
  build/mysql-data.img
zero_ext_times build/mysql-data.img all
echo "built build/mysql-data.img ($(stat -c%s build/mysql-data.img) bytes, journaled ext4, time-normalized)"

P1_LBA=2048
P1_SECTORS=$(( MYSQL_EXT4_MIB * 1024 * 1024 / 512 ))
P2_SECTORS=$(( MYSQL_DATA_MIB * 1024 * 1024 / 512 ))
P2_LBA=$(( P1_LBA + P1_SECTORS ))
: > build/mysql-disk.img; truncate -s $(( (P2_LBA + P2_SECTORS) * 512 )) build/mysql-disk.img
"$PY3" - build/mysql-disk.img "$P1_LBA" "$P1_SECTORS" "$P2_LBA" "$P2_SECTORS" <<'PYEOF'
import sys, struct
img, p1s, p1n, p2s, p2n = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
mbr = bytearray(512)
mbr[0x1B8:0x1BC] = b"\x34\xd0\xea\x1d"
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
dd if=build/mysql-ext4.img of=build/mysql-disk.img bs=512 seek=$P1_LBA conv=notrunc status=none
dd if=build/mysql-data.img of=build/mysql-disk.img bs=512 seek=$P2_LBA conv=notrunc status=none
echo "built build/mysql-disk.img ($(stat -c%s build/mysql-disk.img) bytes: MBR + p1 ext4 ro-root @ LBA $P1_LBA + p2 journaled ext4 data @ LBA $P2_LBA)"
echo "  sha256(mysql-ext4.img)=$(sha256sum build/mysql-ext4.img | cut -d' ' -f1)"
echo "  sha256(mysql-data.img)=$(sha256sum build/mysql-data.img | cut -d' ' -f1)"
echo "  sha256(mysql-disk.img)=$(sha256sum build/mysql-disk.img | cut -d' ' -f1)"
echo ">> build-mysql.sh: DONE"
