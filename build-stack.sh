#!/usr/bin/env bash
# build-stack.sh — builder for the four-service stack guest (Milestone 35): UNMODIFIED
# MySQL 8.4 + Kafka 3.5 (KRaft) + Redis 7.2 behind a small gRPC front door (hvx/stack/,
# Go), all co-located over 127.0.0.1 in one guest. Same model as the Kafka and MySQL
# guests: the binary closures live on an ext4 READ-ONLY root (p1, excluded from the
# snapshot); MySQL's datadir, Kafka's log.dirs and Redis's AOF share one small
# journaled-ext4 rw disk (p2 = the fault target). The switch_root initramfs is the
# generic build/initramfs-kafka.cpio.gz. The prelude (sections 1-5) is build-kafka.sh's.
# Run on the build box:  ./dev.sh "bash ./build-stack.sh"  (the first run downloads the
# Go modules pinned by hvx/stack/go.sum into build/stack-gomod).
set -euo pipefail
# Output name prefix: images go to build/${STACK_OUT}-{ext4,data,disk}.img (default "stack").
# Lets two lines of work build their own guest image without overwriting each other's.
STACK_OUT="${STACK_OUT:-stack}"
# Source directory of the Go front door (default hvx/stack). The explorer builds from a
# pinned copy (hvx/explore/stack-src) so its image does not change while hvx/stack is edited.
STACK_GO_SRC="${STACK_GO_SRC:-hvx/stack}"
# Go linker flags for the front door. The default strips symbols. A coverage image
# (Milestone 41) keeps symbols and links the text at a fixed unique address so an Intel PT
# trace can be filtered to this one program, e.g. STACK_GO_LDFLAGS="-T 0x2000001000 -buildid=".
STACK_GO_LDFLAGS="${STACK_GO_LDFLAGS:--s -w -buildid=}"
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
bpg = u32(sb + 0x20)
gdtb = 2 if bs == 1024 else 1
ng = (inodes + ipg - 1) // ipg
# Superblock time fields in the primary AND every backup copy: mke2fs stamps them from the
# advancing faked clock too, so a build that crosses a second boundary differed (M39).
for g in range(ng):
    o = sb if g == 0 else g * bpg * bs + (1024 if bs == 1024 else 0)
    if o + 0x3A <= len(mm) and u16(o + 0x38) == 0xEF53:
        for t in (0x2C, 0x30, 0x40, 0x108):   # s_mtime s_wtime s_lastcheck s_mkfs_time
            struct.pack_into("<I", mm, o + t, 0)
for g in range(ng):
    itbl = u32(gdtb * bs + g * 32 + 0x08)
    base = g * ipg
    for i in range(ipg):
        if base + i >= inodes:
            break
        struct.pack_into("<I", mm, itbl * bs + i * isize + 0x0C, 0)
        if isize > 128:   # large inode: *_extra times + i_crtime -- mke2fs -d stamps the
                          # creation time from the (advancing) faked clock, so it differs
                          # whenever a build crosses a second boundary
            for o in (0x84, 0x88, 0x8C, 0x90, 0x94):
                struct.pack_into("<I", mm, itbl * bs + i * isize + o, 0)
mm.flush(); mm.close(); f.close()
PYEOF
}

# --- 6. the four-service stack guest ---
MYSQL="${HYDO_MYSQL:-/nix/store/aw6ljbiwgds7yw8x2jc3340dm46s7i2k-mysql-8.4.10}"
JDK="${HYDO_KAFKA_JDK:-/nix/store/vjcrra860yal7fzk1m81ddirrafqrccz-openjdk-headless-17.0.7+7}"
KP="${HYDO_KAFKA:-/nix/store/prk07apdgqqjcqyxr5fqww1y8bxpw7z9-apache-kafka-2.13-3.5.0}"
REDIS_OUT="${HYDO_REDIS_OUT:-/nix/store/ky1xq6xa88qhwwagnh4anm4sjl3c7vyk-redis-static-x86_64-unknown-linux-musl-7.2.4}"
GO="${HYDO_GO:-/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go}"
[ -x "$GO" ] || GO=/nix/store/xrgn6m0c5vbz06467q06zdwnk6qqdl7z-go-1.26.5/bin/go
for need in "$MYSQL/bin/mysqld" "$JDK/bin/java" "$REDIS_OUT/bin/redis-server" "$GO"; do
  [ -x "$need" ] || { echo "error: missing $need (pinned store path; set the HYDO_* override)" >&2; exit 1; }
done
[ -d "$KP/libs" ] || { echo "error: missing $KP/libs (set HYDO_KAFKA)" >&2; exit 1; }
STACK_NORDERS=30
# iproute2 (for `tc ... netem` in the network-fault scenarios, M36): a pinned glibc build;
# its closure is staged onto the ro-root beside the MySQL/JDK closures.
IPROUTE2="${HYDO_IPROUTE2:-/nix/store/dx4fsbxs5v82l9lzmjkpkhx5ygqcwm69-iproute2-6.8.0}"
[ -x "$IPROUTE2/bin/tc" ] || { echo "error: no iproute2 at $IPROUTE2 (set HYDO_IPROUTE2)" >&2; exit 1; }
NETMODS="sch_prio sch_netem cls_u32"
STACK_KAFKA_CLUSTER_ID="P0hH8j92QAKKXdQ2D7Aw7A"

# --- 6.1 the gRPC front door + client + oracle: one static Go binary. Byte-reproducible:
#     pinned Go toolchain, dependency versions pinned by hvx/stack/go.sum, CGO off,
#     -trimpath, empty build id. Modules are cached under build/stack-gomod. ---
mkdir -p build/stack-gomod build/stack-gocache
TOP="$PWD"
( cd "$STACK_GO_SRC" && \
  GOPATH="$TOP/build/stack-gopath" GOMODCACHE="$TOP/build/stack-gomod" \
  GOCACHE="$TOP/build/stack-gocache" GOFLAGS=-mod=readonly GOTOOLCHAIN=local \
  CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
  "$GO" build -trimpath -buildvcs=false -ldflags="$STACK_GO_LDFLAGS" -o "$TOP/build/${STACK_OUT}-hydostack" . )
echo "built build/${STACK_OUT}-hydostack ($(stat -c%s build/${STACK_OUT}-hydostack) bytes, static Go, sha256 $(sha256sum build/${STACK_OUT}-hydostack | cut -c1-16))"

# --- 6.2 the ro-root tree: MySQL + JDK closures, Kafka jars, static redis + hydostack.
#     The closures are staged once and reused; only the small mutable dirs are rebuilt. ---
R=build/rootfs-${STACK_OUT}-root
if [ -x "$R$MYSQL/bin/mysqld" ] && [ -x "$R$JDK/bin/java" ] && [ -d "$R/opt/kafka/libs" ]; then
  echo "stack ro-root: reusing the staged closures (remove $R to force fresh)"
  chmod -R u+w "${R:?}/bin" "${R:?}/sbin" "${R:?}/etc" 2>/dev/null || true
  rm -rf "${R:?}/bin" "${R:?}/sbin" "${R:?}/etc"
else
  chmod -R u+w "${R:?}" 2>/dev/null || true
  rm -rf "${R:?}"; mkdir -p "$R/nix/store" "$R/opt/kafka/libs"
  for p in $( (nix-store -qR "$MYSQL"; nix-store -qR "$JDK") | sort -u); do
    cp -a "$p" "$R/nix/store/" 2>/dev/null || true
  done
  cp "$KP"/libs/*.jar "$R/opt/kafka/libs/"
  echo "staged closures: $(du -sm "$R/nix/store" | cut -f1) MiB + $(ls "$R"/opt/kafka/libs/*.jar | wc -l) kafka jars"
fi
# iproute2 closure (idempotent: only paths not already staged are copied)
for p in $(nix-store -qR "$IPROUTE2"); do
  [ -e "$R$p" ] || cp -a "$p" "$R/nix/store/"
done
mkdir -p "$R/bin" "$R/sbin" "$R/proc" "$R/sys" "$R/dev" "$R/data" "$R/tmp" "$R/etc/kafka"
cp "$BB" "$R/bin/busybox"; chmod +x "$R/bin/busybox"
# traffic-control kernel modules for the loopback network faults (M36)
chmod -R u+w "$R/lib" 2>/dev/null || true
rm -rf "${R:?}/lib/modules"; mkdir -p "$R/lib/modules"
for m in $NETMODS; do
  src=$(find "$MODROOT" -name "$m.ko.xz" | head -1)
  [ -n "$src" ] || { echo "error: module $m.ko.xz not found under $MODROOT" >&2; exit 1; }
  xz -dc "$src" > "$R/lib/modules/$m.ko"
done
cp build/hvx_seedrng "$R/bin/hvx_seedrng"
cp "$REDIS_OUT/bin/redis-server" "$R/bin/redis-server"
cp "$REDIS_OUT/bin/redis-cli" "$R/bin/redis-cli"
cp build/${STACK_OUT}-hydostack "$R/bin/hydostack"
chmod 0755 "$R/bin/hvx_seedrng" "$R/bin/redis-server" "$R/bin/redis-cli" "$R/bin/hydostack"
cat > "$R/etc/stack-env" <<ENV
MYSQL=$MYSQL
TC=$IPROUTE2/bin/tc
JAVA=$JDK/bin/java
NORDERS=$STACK_NORDERS
KAFKA_CLUSTER_ID=$STACK_KAFKA_CLUSTER_ID
ENV
printf '127.0.0.1 localhost\n' > "$R/etc/hosts"
printf 'root:x:0:0:root:/tmp:/bin/sh\n' > "$R/etc/passwd"
printf 'root:x:0:\n' > "$R/etc/group"

# MySQL: same small single-purpose InnoDB server as build-mysql.sh; innodb_flush_log_at_
# trx_commit is appended by /sbin/init. Smaller buffer pool: three servers share 1 GiB.
cat > "$R/etc/my.cnf" <<'CNF'
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
log-bin=binlog
sync_binlog=1
server-id=1
performance_schema=OFF
auto_generate_certs=OFF
caching_sha2_password_auto_generate_rsa_keys=OFF
innodb_buffer_pool_size=32M
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
cat > "$R/etc/mysql-init.sql" <<'SQL'
CREATE USER IF NOT EXISTS 'hydo'@'%' IDENTIFIED BY '';
GRANT ALL PRIVILEGES ON *.* TO 'hydo'@'%';
SQL

# Kafka: the §2l single-node KRaft config, log.dirs on the data disk.
cat > "$R/etc/kafka/server.properties" <<'EOF'
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
log.retention.ms=-1
EOF
# log.retention.ms=-1: the orders topic is a record of every order and the oracle checks
# every ACKed order is in it, so nothing may expire. With the default (7 days, judged by
# record timestamps against the wall clock) a forward clock step of more than 7 days makes
# Kafka roll the segment and delete every older order at its next retention check
# (hvx/cutflush/KAFKA-CLOCK.md).
cat > "$R/etc/kafka/log4j.properties" <<'EOF'
log4j.rootLogger=WARN, stdout
log4j.appender.stdout=org.apache.log4j.ConsoleAppender
log4j.appender.stdout.layout=org.apache.log4j.PatternLayout
log4j.appender.stdout.layout.ConversionPattern=%p %c %m%n
log4j.logger.kafka.server.KafkaRaftServer=INFO
EOF
# Diagnostic images only (STACK_KAFKA_DIAG=1; default images are byte-unchanged): Kafka's
# log layer at INFO (segment rolls, deletions, log-start-offset moves), LogManager at DEBUG
# (every retention check), wall-clock timestamps on every line. hvx/cutflush/KAFKA-CLOCK.md.
if [ "${STACK_KAFKA_DIAG:-0}" = 1 ]; then
  cat > "$R/etc/kafka/log4j.properties" <<'EOF'
log4j.rootLogger=WARN, stdout
log4j.appender.stdout=org.apache.log4j.ConsoleAppender
log4j.appender.stdout.layout=org.apache.log4j.PatternLayout
log4j.appender.stdout.layout.ConversionPattern=%d{ISO8601} %p %c %m%n
log4j.logger.kafka.server.KafkaRaftServer=INFO
log4j.logger.kafka.log=INFO
log4j.logger.kafka.log.LogManager=DEBUG
log4j.logger.org.apache.kafka.storage=INFO
EOF
fi
# Extra broker settings (STACK_KAFKA_EXTRA, "key=value" lines; empty by default, so the
# default images are byte-unchanged).
if [ -n "${STACK_KAFKA_EXTRA:-}" ]; then printf '%s\n' "$STACK_KAFKA_EXTRA" >> "$R/etc/kafka/server.properties"; fi

cat > "$R/sbin/init" <<'SINIT'
#!/bin/busybox sh
BB=/bin/busybox
[ -c /dev/null ] || $BB mount -t devtmpfs none /dev 2>/dev/null
$BB mount -t proc none /proc 2>/dev/null
$BB mount -t sysfs none /sys 2>/dev/null
$BB mount -t tmpfs none /tmp 2>/dev/null
. /etc/stack-env      # MYSQL JAVA NORDERS KAFKA_CLUSTER_ID
export HOME=/tmp TZ=UTC LANG=C GOMAXPROCS=1
echo ""
echo "=================================================="
echo "  hydo: switched into the ext4 ro-root (M35 four-service stack)"
echo "  uname: $($BB uname -sr)"
echo "=================================================="
echo "hydo-stack-mid: real root is live"
PHASE=solo; FLUSH=on; KILL=""; NET=""
for tok in $($BB cat /proc/cmdline 2>/dev/null); do
  case "$tok" in
    hydo_phase=*) PHASE="${tok#hydo_phase=}";;
    hydo_flush=*) FLUSH="${tok#hydo_flush=}";;
    hydo_kill=*)  KILL="${tok#hydo_kill=}";;    # <mysql|kafka|redis>@<k>   (M36)
    hydo_net=*)   NET="${tok#hydo_net=}";;      # <mysql|kafka|redis>:<netem args, commas for spaces>
  esac
done
UP() { $BB cut -d' ' -f1 /proc/uptime 2>/dev/null; }
# hydo_flush=on  -> every store durable before the ACK: MySQL innodb_flush_log_at_trx_commit=1,
#                   Kafka log.flush.interval.messages=1, Redis appendfsync always.
# hydo_flush=off -> ONE relaxed knob: MySQL innodb_flush_log_at_trx_commit=2 (Kafka and
#                   Redis stay durable), so a power cut can leave Kafka holding orders that
#                   MySQL lost: a cross-service disagreement.
TRX=1; [ "$FLUSH" = "off" ] && TRX=2
echo "hydo-stack: phase=$PHASE flush=$FLUSH (mysql trx_commit=$TRX, kafka flush.messages=1, redis appendfsync=always) norders=$NORDERS"
/bin/hvx_seedrng 2>/dev/null || echo "hydo-stack: hvx_seedrng missing/failed"
$BB ifconfig lo 127.0.0.1 netmask 255.0.0.0 up 2>/dev/null
$BB ip link set lo up 2>/dev/null
echo "hydo-stack: loopback up"
die_oracle() {   # $1 = verdict line; the stop markers follow so the harness can finish
  echo "$1"
  [ "$PHASE" = "workload" ] && echo "hydo-stack-workload-done"
  echo "hydo-stack-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"
  while true; do $BB sleep 100; done
}
MERR=$($BB mount -t ext4 -o rw /dev/vda2 /data 2>&1) || {
  if [ "$PHASE" = "oracle" ]; then die_oracle "HYDO-ORACLE: VIOLATION data disk unmountable ($MERR)"; fi
  die_oracle "HYDO-ORACLE: ERROR mount data ($MERR)"
}
$BB mkdir -p /data/redis /data/kafka-logs
echo "hydo-stack: mounted ext4 /dev/vda2 rw on /data"

# ---- Redis ----
$BB cat > /tmp/redis.conf <<EOF
port 6379
bind 127.0.0.1
protected-mode no
dir /data/redis
save ""
appendonly yes
appendfsync always
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
start_redis() {
  /bin/redis-server /tmp/redis.conf > /tmp/redis.out 2>&1 &
  RPID=$!
  n=0
  while [ $n -lt 60 ]; do
    /bin/redis-cli -p 6379 ping 2>/dev/null | $BB grep -q PONG && { echo "hydo-stack: redis ready after $n polls (uptime=$(UP)s)"; return 0; }
    $BB kill -0 $RPID 2>/dev/null || { echo "hydo-stack: redis DIED"; $BB tail -8 /tmp/redis.log; return 1; }
    n=$((n+1)); $BB sleep 1
  done
  echo "hydo-stack: redis NOT ready"; $BB tail -8 /tmp/redis.log; return 1
}

# ---- MySQL ----
$BB cp /etc/my.cnf /tmp/my.cnf
echo "innodb_flush_log_at_trx_commit=$TRX" >> /tmp/my.cnf
[ "$TRX" = 1 ] || echo "sync_binlog=0" >> /tmp/my.cnf   # relaxed: both logs, MySQL's documented fast setting
SQL() { "$MYSQL/bin/mysql" --no-defaults --protocol=TCP -h127.0.0.1 -P3306 -uhydo --ssl-mode=DISABLED -N -B "$@"; }
start_mysql() {
  if [ ! -d /data/mysql/mysql ]; then
    echo "hydo-stack: initializing the MySQL data directory (uptime=$(UP)s)"
    "$MYSQL/bin/mysqld" --defaults-file=/tmp/my.cnf --initialize-insecure > /tmp/init.log 2>&1 || \
      { echo "hydo-stack: mysqld --initialize failed"; $BB tail -10 /tmp/mysqld.err; return 1; }
  fi
  "$MYSQL/bin/mysqld" --defaults-file=/tmp/my.cnf > /tmp/mysqld.out 2>&1 &
  MPID=$!
  n=0
  while [ $n -lt 600 ]; do
    SQL -e "SELECT 1" > /dev/null 2>&1 && { echo "hydo-stack: mysql ready after $n polls (uptime=$(UP)s)"; return 0; }
    $BB kill -0 $MPID 2>/dev/null || { echo "hydo-stack: mysqld DIED"; $BB tail -10 /tmp/mysqld.err; return 1; }
    n=$((n+1)); $BB sleep 1
  done
  echo "hydo-stack: mysql NOT ready"; $BB tail -10 /tmp/mysqld.err; return 1
}

# ---- Kafka (§2l recipe: headless JDK, deterministic c1sync JIT, SerialGC, fixed heap) ----
CP="/opt/kafka/libs/*"
LOG4J="-Dlog4j.configuration=file:/etc/kafka/log4j.properties"
JCOMMON="-XX:TieredStopAtLevel=1 -XX:CICompilerCount=1 -XX:-BackgroundCompilation -XX:+UseSerialGC -XX:-UsePerfData -XX:-UseContainerSupport -XX:ActiveProcessorCount=1 -Xshare:off -Djava.security.egd=file:/dev/urandom -Duser.name=hydo -Duser.home=/tmp -Duser.timezone=UTC -Duser.language=en -Duser.country=US -Dfile.encoding=UTF-8 -Djava.io.tmpdir=/tmp"
$BB cp /etc/kafka/server.properties /tmp/server.properties
echo "log.flush.interval.messages=1" >> /tmp/server.properties
start_kafka() {
  $JAVA $JCOMMON -Xms96m -Xmx96m $LOG4J -cp "$CP" kafka.tools.StorageTool format \
        -t "$KAFKA_CLUSTER_ID" -c /tmp/server.properties --ignore-formatted > /tmp/format.log 2>&1 || \
    { echo "hydo-stack: kafka storage format failed"; $BB tail -8 /tmp/format.log; return 1; }
  $JAVA $JCOMMON $LOG4J -Xms256m -Xmx256m -cp "$CP" kafka.Kafka /tmp/server.properties > /tmp/broker.log 2>&1 &
  KPID=$!
  n=0
  while [ $n -lt 600 ]; do
    $BB grep -qa "Kafka Server started" /tmp/broker.log 2>/dev/null && { echo "hydo-stack: kafka ready after $n polls (uptime=$(UP)s)"; return 0; }
    $BB kill -0 $KPID 2>/dev/null || { echo "hydo-stack: kafka broker DIED"; $BB tail -10 /tmp/broker.log; return 1; }
    n=$((n+1)); $BB sleep 1
  done
  echo "hydo-stack: kafka NOT ready"; $BB tail -10 /tmp/broker.log; return 1
}

# ---- the gRPC front door ----
start_front() {
  /bin/hydostack server > /tmp/front.log 2>&1 &
  FPID=$!
  n=0
  while [ $n -lt 120 ]; do
    $BB grep -qa "server listening" /tmp/front.log 2>/dev/null && { echo "hydo-stack: gRPC front door ready after $n polls (uptime=$(UP)s)"; return 0; }
    $BB kill -0 $FPID 2>/dev/null || { echo "hydo-stack: front door DIED"; $BB tail -6 /tmp/front.log; return 1; }
    n=$((n+1)); $BB sleep 1
  done
  echo "hydo-stack: front door NOT ready"; $BB tail -6 /tmp/front.log; return 1
}

start_all() {   # $1 = verdict to print if a store cannot come up
  start_redis || die_oracle "$1 (redis)"
  start_mysql || die_oracle "$1 (mysql)"
  start_kafka || die_oracle "$1 (kafka)"
}

svc_port() { case "$1" in mysql) echo 3306;; kafka) echo 9092;; redis) echo 6379;; esac; }

# ---- M36 network faults: tc netem on lo, only for traffic TO one service's port. netem's
#      randomness comes from the kernel CRNG (seeded deterministically by hvx_seedrng) and
#      its delays from the virtual clock, so the same config replays bit-for-bit. ----
net_fault_on() {   # $1 = <svc>:<netem args with commas>
  svc=${1%%:*}; args=$(echo "${1#*:}" | $BB tr ',' ' '); port=$(svc_port $svc)
  for m in sch_prio sch_netem cls_u32; do $BB insmod /lib/modules/$m.ko 2>/dev/null; done
  $TC qdisc add dev lo root handle 1: prio bands 4 priomap 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 && \
  $TC qdisc add dev lo parent 1:4 handle 40: netem $args && \
  $TC filter add dev lo parent 1:0 protocol ip prio 1 u32 match ip dport $port 0xffff flowid 1:4 || \
    die_oracle "HYDO-ORACLE: ERROR tc setup failed"
  echo "hydo-stack: NET FAULT on traffic to $svc (port $port): netem $args"
}
# net_heal <tc> <netem args in force>: lift the netem fault on lo WITHOUT dropping the packets
# still waiting in its delay queue. A plain `tc qdisc del` frees them (kernel 6.6:
# qdisc_destroy -> netem_reset -> tfifo_reset), so TCP had to retransmit them after a timeout
# (about 1 s) the schedule never asked for. Instead:
#  1. `tc qdisc change` the netem to no delay/loss/duplicate/corruption and a 100 Gbit rate.
#     netem_change keeps the queue; queued packets keep their send times, and in rate mode a
#     new packet is sent right after the last queued one, so the order is kept.
#  2. wait (guest time, deterministic) until the netem queue is empty, at most the largest
#     delay in force (delay + 4 x jitter) + 500 ms;
#  3. remove the filters (new packets bypass netem), wait until a packet still in its few-us
#     rate window has left (at most 50 ms), then delete the root qdisc.
# Prints one line: "queued=<packets at the heal> drained_ms=<ms> left=<packets dropped>".
# A copy of the function in hvx/onboard/hydo-init.sh; keep the two in sync.
net_qlen() {   # <tc>: packets in the netem queue (handle 40:), empty if there is none
  $1 -s qdisc show dev lo | $BB awk '/^qdisc netem 40:/ { f = 1 } f && $1 == "backlog" { sub(/p$/, "", $3); print $3 + 0; exit }'
}
net_heal() {
  local tc=$1 bound q0 n t w
  bound=$(echo "$2" | $BB tr ',' ' ' | $BB awk '
    function ms(v,  x) { x = v + 0; if (v ~ /us/) return x / 1000; if (v ~ /ms/) return x
                         if (v ~ /s/) return x * 1000; return x / 1000 }
    { m = 0; for (i = 1; i <= NF; i++) if ($i == "delay") { m = ms($(i + 1))
        if ($(i + 2) ~ /^[0-9.]+(us|usec|usecs|ms|msec|msecs|s|sec|secs)?$/) m += 4 * ms($(i + 2)) }
      printf "%d\n", m + 500 }')
  q0=$(net_qlen $tc); n=${q0:-0}; t=0
  $tc qdisc change dev lo parent 1:4 handle 40: netem rate 100gbit corrupt 0% 2>/dev/null || n=0
  while [ "${n:-0}" -gt 0 ] && [ $t -lt $bound ]; do $BB sleep 0.01; t=$((t + 10)); n=$(net_qlen $tc); done
  $tc filter del dev lo parent 1:0 2>/dev/null
  n=$(net_qlen $tc); w=0
  while [ "${n:-0}" -gt 0 ] && [ $w -lt 50 ]; do $BB sleep 0.01; w=$((w + 10)); n=$(net_qlen $tc); done
  $tc qdisc del dev lo root 2>/dev/null
  echo "queued=${q0:-0} drained_ms=$t left=${n:-0}"
}

restart_svc() {
  case "$1" in
    mysql) start_mysql;; kafka) start_kafka;; redis) start_redis;; front) start_front;;
  esac || die_oracle "HYDO-ORACLE: ERROR $1 restart failed"
}
svc_pid() { case "$1" in mysql) echo $MPID;; kafka) echo $KPID;; redis) echo $RPID;; front) echo $FPID;; esac; }

client() {   # $@ = place arguments; ACKed ids accumulate in /tmp/acked, every attempt's
             # outcome ("<id> ack|failed|unknown", M39) in /tmp/outcomes
  HYDO_ACKED=/tmp/acked HYDO_OUTCOMES=/tmp/outcomes /bin/hydostack place "$@" >> /tmp/client.log 2>&1
  echo "hydo-stack: client $* rc=$? (uptime=$(UP)s)"
}

failed_ids() {   # M39: ids whose LAST outcome was a definite failure -> /tmp/failed
  $BB awk '{ o[$1] = $2 } END { for (i in o) if (o[i] == "failed") print i }' /tmp/outcomes 2>/dev/null | $BB sort -n > /tmp/failed
}

retry_unacked() {   # M37: the client re-places every un-ACKed order with the SAME id
  HYDO_OUTCOMES=/tmp/outcomes /bin/hydostack retry $NORDERS /tmp/acked >> /tmp/client.log 2>&1
  echo "hydo-stack: client retry rc=$? (uptime=$(UP)s)"
}

place_orders() {
  start_front || die_oracle "HYDO-ORACLE: ERROR front door"
  # Provisioning is NOT part of the window under test: make it durable on its own (sync ->
  # a device FLUSH) -- the MySQL data directory, Kafka's storage format (StorageTool does
  # not fsync bootstrap.checkpoint), the schema and the topic. Only the orders placed below
  # are exposed to the power cut.
  $BB sync
  echo "hydo-stack: provisioning synced (uptime=$(UP)s)"
  echo "HYDO-SETUP-COMPLETE"   # hvx/ASSERTIONS.md lifecycle: faults may begin here
  : > /tmp/acked; : > /tmp/outcomes; : > /tmp/client.log
  if [ -n "$NET" ]; then net_fault_on "$NET"; fi
  if [ -n "$KILL" ]; then
    # M36 single-service crash: the guest itself kills one service at a fixed point in the
    # order stream (after K orders), places a few orders while it is down, restarts it, and
    # finishes. Nothing here depends on host timing.
    ksvc=${KILL%@*}; kat=${KILL#*@}; kdown=$((kat + 3))
    echo "hydo-stack: client placing orders 0..$((kat - 1)) (uptime=$(UP)s)"
    client 0 $kat
    $BB kill -9 $(svc_pid $ksvc) 2>/dev/null
    echo "hydo-stack: KILLED $ksvc (SIGKILL) after $kat orders (uptime=$(UP)s)"
    client $kat $kdown
    echo "hydo-stack: restarting $ksvc (uptime=$(UP)s)"
    restart_svc $ksvc
    client $kdown $NORDERS
    retry_unacked
    $BB grep -a "HYDO-STACK\|failed" /tmp/client.log
  elif [ -n "$NET" ]; then
    echo "hydo-stack: client placing $NORDERS orders over gRPC (uptime=$(UP)s)"
    client 0 $NORDERS
    retry_unacked
    # Network faults are transient: heal the network, then the client retries whatever is
    # still un-ACKed (with the same ids) -- every order must end up ACKed exactly once.
    echo "hydo-stack: network healed: $(net_heal "$TC" "${NET#*:}") (uptime=$(UP)s)"
    retry_unacked
    $BB grep -a "HYDO-STACK\|failed" /tmp/client.log
  else
    echo "hydo-stack: client placing $NORDERS orders over gRPC (uptime=$(UP)s)"
    /bin/hydostack place $NORDERS > /tmp/client.log 2>&1
    echo "hydo-stack: client rc=$? done (uptime=$(UP)s)"
    $BB grep -a "HYDO-STACK" /tmp/client.log || { echo "hydo-stack: client.log tail:"; $BB tail -6 /tmp/client.log; }
  fi
}

run_oracle() {
  # With a fault injected, only the ACKed orders must be consistent; the plain runs keep
  # the original check of every order.
  if [ -n "$KILL$NET" ]; then
    $TC qdisc del dev lo root 2>/dev/null   # the oracle itself reads over a clean network
    failed_ids
    /bin/hydostack oracle $NORDERS /tmp/acked /tmp/failed > /tmp/oracle.log 2>&1
  else
    /bin/hydostack oracle $NORDERS > /tmp/oracle.log 2>&1
  fi
  $BB grep -a "HYDO-ORACLE" /tmp/oracle.log || { echo "HYDO-ORACLE: ERROR oracle produced no verdict"; $BB tail -6 /tmp/oracle.log; }
}

case "$PHASE" in
  workload)
    start_all "HYDO-ORACLE: ERROR server start"
    place_orders
    echo "hydo-stack: redis AOF before the cut: $($BB ls -l /data/redis /data/redis/appendonlydir 2>&1 | $BB tr '\n' ' ')"
    $BB tail -6 /tmp/redis.log 2>/dev/null
    # NO sync: it would FLUSH the volatile write cache and close the window under test.
    $BB kill -STOP $FPID $KPID $MPID $RPID 2>/dev/null
    echo "hydo-stack: all servers STOPped (durability frozen at the acked instant)"
    echo "hydo-stack-workload-done"
    while true; do $BB sleep 100; done
    ;;
  oracle)
    echo "hydo-stack: recovering from log"   # branch mid-point: before any store recovers
    start_all "HYDO-ORACLE: VIOLATION store cannot recover (committed data unrecoverable)"
    echo "hydo-stack: redis AOF after recovery: $($BB ls -l /data/redis/appendonlydir 2>&1 | $BB tr '\n' ' ')"
    $BB grep -a "AOF\|DB loaded\|keys" /tmp/redis.log 2>/dev/null | $BB tail -4
    run_oracle
    echo "hydo-stack-marker: workload complete"
    echo "hydo-boot-marker: launching shell on ttyS0"
    exec $BB sh
    ;;
  *)
    start_all "HYDO-ORACLE: ERROR server start"
    place_orders
    run_oracle
    $BB sync
    echo "hydo-stack-marker: workload complete"
    echo "hydo-boot-marker: launching shell on ttyS0"
    exec $BB sh
    ;;
esac
SINIT
chmod +x "$R/sbin/init"
# The explorer images (STACK_OUT=explore* or richcov-*, the whole-system coverage variants) add a command-driven "serve" mode to /sbin/init:
# hvx/explore/stack-explore.sh is installed as /etc/stack-explore.sh and hooked in at two
# points. Every other image keeps the init above byte-for-byte.
case "$STACK_OUT" in explore*|richcov-*) EXPLORE_IMAGE=1;; *) EXPLORE_IMAGE=0;; esac
if [ "$EXPLORE_IMAGE" = 1 ]; then
  cp hvx/explore/stack-explore.sh "$R/etc/stack-explore.sh"
  if [ "${STACK_KAFKA_DIAG:-0}" = 1 ]; then   # diagnostic: dump Kafka's log decisions after the verdict
    "$PY3" - "$R/etc/stack-explore.sh" <<'PYEOF'
import sys
p = sys.argv[1]; s = open(p).read()
a = '  echo "hydo-stack-marker: workload complete"\n  echo "hydo-boot-marker: launching shell on ttyS0"\n  exec $BB sh\n}\n'
assert s.count(a) == 1
d = r'''  echo "hydo-kdiag: uptime=$(UP) realtime=$($BB date +%s)"
  $BB ls -l /data/kafka-logs/orders-0 2>&1 | $BB sed 's/^/hydo-kdiag: ls: /'
  $BB grep -a "kafka\.log\|storage" /tmp/broker.log | $BB grep -a -v "Checking for dirty logs\|Checking if flush is needed\|__cluster_metadata" | $BB sed 's/^/hydo-kdiag: /'
'''
s = s.replace(a, d + a)
open(p, "w").write(s)
PYEOF
  fi
  if [ -n "${STACK_ORACLE_WAIT:-}" ]; then   # diagnostic: let N s of guest time pass before the oracle reads
    "$PY3" - "$R/etc/stack-explore.sh" "$STACK_ORACLE_WAIT" <<'PYEOF'
import sys
p, n = sys.argv[1], int(sys.argv[2]); s = open(p).read()
a = '  /bin/hydostack oracle $NORDERS /tmp/acked /tmp/failed > /tmp/oracle.log 2>&1\n'
assert s.count(a) == 1
s = s.replace(a, '  echo "hydo-kdiag: waiting %ds before the oracle (uptime=$(UP))"; $BB sleep %d\n' % (n, n) + a)
open(p, "w").write(s)
PYEOF
  fi
  "$PY3" - "$R/sbin/init" <<'PYEOF'
import sys
p = sys.argv[1]; s = open(p).read()
a = 'echo "hydo-stack: loopback up"\n'
b = 'case "$PHASE" in\n'
assert s.count(a) == 1 and s.count(b) == 1
s = s.replace(a, a + 'EXPLORE=""; [ "$PHASE" = "serve" ] && { . /etc/stack-explore.sh; explore_s0; }\n')
s = s.replace(b, '[ -n "$EXPLORE" ] && explore_main\n' + b)
open(p, "w").write(s)
PYEOF
fi
find "$R" -exec touch --no-dereference -h -d @1600000000 {} + 2>/dev/null || true

# --- 6.3 images: ext4 ro-root (p1) + journaled ext4 data disk (p2) behind a hand-made MBR ---
STACK_EXT4_MIB=1792
STACK_DATA_MIB=512
mkext4_rootfs_reproducible build/${STACK_OUT}-ext4.img "$R" $STACK_EXT4_MIB \
  aaaa3535-bbbb-cccc-dddd-eeeeffff0035 35353535-1111-2222-3333-444455556666
echo "built build/${STACK_OUT}-ext4.img ($(stat -c%s build/${STACK_OUT}-ext4.img) bytes)"

: > build/${STACK_OUT}-data.img; truncate -s $(( STACK_DATA_MIB * 1024 * 1024 )) build/${STACK_OUT}-data.img
SOURCE_DATE_EPOCH=1600000000 "$FAKETIME_BIN" "$FAKE_TS" "$MKE2FS" -q -F -t ext4 -b 4096 \
  -O ^metadata_csum,^64bit,^flex_bg,^resize_inode,has_journal -J size=16 \
  -U 35350000-2222-3333-4444-5555eeee0035 \
  -E hash_seed=35355555-4444-3333-2222-1111dddd0035,lazy_itable_init=0,lazy_journal_init=0 \
  build/${STACK_OUT}-data.img
"$PY3" - build/${STACK_OUT}-data.img <<'PYEOF'
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
        if isize > 128:                             # *_extra times + i_crtime
            for o in (0x84, 0x88, 0x8C, 0x90, 0x94):
                struct.pack_into("<I", mm, io + o, 0)
mm.flush(); mm.close(); f.close()
PYEOF
echo "built build/${STACK_OUT}-data.img ($(stat -c%s build/${STACK_OUT}-data.img) bytes, journaled ext4, time-normalized)"

P1_LBA=2048
P1_SECTORS=$(( STACK_EXT4_MIB * 1024 * 1024 / 512 ))
P2_SECTORS=$(( STACK_DATA_MIB * 1024 * 1024 / 512 ))
P2_LBA=$(( P1_LBA + P1_SECTORS ))
: > build/${STACK_OUT}-disk.img; truncate -s $(( (P2_LBA + P2_SECTORS) * 512 )) build/${STACK_OUT}-disk.img
"$PY3" - build/${STACK_OUT}-disk.img "$P1_LBA" "$P1_SECTORS" "$P2_LBA" "$P2_SECTORS" <<'PYEOF'
import sys, struct
img, p1s, p1n, p2s, p2n = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
mbr = bytearray(512)
mbr[0x1B8:0x1BC] = b"\x35\xd0\xea\x1d"
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
dd if=build/${STACK_OUT}-ext4.img of=build/${STACK_OUT}-disk.img bs=512 seek=$P1_LBA conv=notrunc status=none
dd if=build/${STACK_OUT}-data.img of=build/${STACK_OUT}-disk.img bs=512 seek=$P2_LBA conv=notrunc status=none
echo "built build/${STACK_OUT}-disk.img ($(stat -c%s build/${STACK_OUT}-disk.img) bytes: MBR + p1 ext4 ro-root @ LBA $P1_LBA + p2 journaled ext4 data @ LBA $P2_LBA)"
echo "  sha256(hydostack)=$(sha256sum build/${STACK_OUT}-hydostack | cut -d' ' -f1)"
echo "  sha256(${STACK_OUT}-ext4.img)=$(sha256sum build/${STACK_OUT}-ext4.img | cut -d' ' -f1)"
echo "  sha256(${STACK_OUT}-data.img)=$(sha256sum build/${STACK_OUT}-data.img | cut -d' ' -f1)"
echo "  sha256(${STACK_OUT}-disk.img)=$(sha256sum build/${STACK_OUT}-disk.img | cut -d' ' -f1)"
echo ">> build-stack.sh: DONE"
