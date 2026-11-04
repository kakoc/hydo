#!/bin/sh
# Build the service guests for the Mac VMM (hydo-hvm) on a Linux machine with nix (the build machine):
#   - Image-svc:            the M48 kernel (Linux 6.6.35 + FORCE_CNTPCT) with ../linux/hvm.config +
#                           ../linux/hvm-virtio.config (virtio-mmio disk/net, ext4) + svc.config
#   - redis.cpio.gz:        busybox + redis 7.2.4 (aarch64 nixpkgs, only the files the loader opens)
#   - miniflux.cpio.gz:     busybox + PostgreSQL 15.7 + miniflux 2.1.3 + curl (same)
#   - redis-disk.img, pg-disk.img: empty ext4 data disks (64 / 256 MiB) for /dev/vda
# All binaries are the aarch64-linux builds from the nixpkgs binary cache (pinned store paths below).
# Byte-reproducible: pinned inputs, fixed kernel build identity, sorted cpio with fixed mtimes, gzip -n.
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
LINUXDIR=$HERE/../linux
WORK=${WORK:-$PWD}
OUT=${OUT:-$WORK/out}
KSRC_TAR=/nix/store/czg3kwyz7v5i192myvg62i66n83s0940-linux-6.6.35.tar.xz          # nixpkgs#linux_6_6.src
BUSYBOX=/nix/store/zbc5fx2yszji1a4aw2ff46m0p99c6yxb-busybox-static-aarch64-unknown-linux-musl-1.36.1
REDIS=/nix/store/svzgg47md9362vjrq5x9k1djln2pkihj-redis-7.2.4                   # nixpkgs#redis, aarch64-linux
PG=/nix/store/9ad5x3mzk8433w2fycpbpznc0qyfd2ky-postgresql-15.7                  # nixpkgs#postgresql_15
PGLIB=/nix/store/xrwji1vd9fgc003ws8yp907k7pmfndfy-postgresql-15.7-lib
TZDATA=/nix/store/rllwcf5hb2zv1m00k792s8k81nixas77-tzdata-2024a
MINIFLUX=/nix/store/nzv23fam1q1wqp1a3lav8nh5pp8i67cc-miniflux-2.1.3             # nixpkgs#miniflux
CURL=/nix/store/vmnmr9qd2liycl18frma90mrzbzsr0lm-curl-8.7.1-bin                 # nixpkgs#curl.bin
E2FS=/nix/store/24s0phlx9kly7x7qb8gal3mmn51ndmlx-e2fsprogs-1.47.0-bin          # the pinned mke2fs of build-linux.sh
for p in "$KSRC_TAR" "$BUSYBOX" "$REDIS" "$PG" "$PGLIB" "$TZDATA" "$MINIFLUX" "$CURL" "$E2FS"; do
  [ -e "$p" ] || { echo "missing $p: nix build --max-jobs 0 --system aarch64-linux <attr>" >&2; exit 1; }
done
TOOLS="nixpkgs#pkgsCross.aarch64-multiplatform.buildPackages.gcc nixpkgs#gnumake nixpkgs#flex nixpkgs#bison nixpkgs#bc nixpkgs#perl nixpkgs#gcc nixpkgs#python3 nixpkgs#cpio nixpkgs#gzip"
mkdir -p "$OUT"
cd "$WORK"

# ---- kernel ----
if [ ! -f "$OUT/Image-svc" ]; then
  rm -rf linux-6.6.35 && tar xf "$KSRC_TAR"
  cat "$LINUXDIR/hvm.config" "$LINUXDIR/hvm-virtio.config" "$HERE/svc.config" > kernel.config
  nix shell $TOOLS --command sh -c "
    set -e
    python3 '$LINUXDIR/apply-cntpct.py' linux-6.6.35
    cd linux-6.6.35
    make ARCH=arm64 CROSS_COMPILE=aarch64-unknown-linux-gnu- allnoconfig KCONFIG_ALLCONFIG=$WORK/kernel.config >/dev/null
    for o in SYSVIPC ADVISE_SYSCALLS FILE_LOCKING ARM_ARCH_TIMER_FORCE_CNTPCT ARM64_USE_LSE_ATOMICS VIRTIO_MMIO VIRTIO_BLK EXT4_FS; do grep -q \"^CONFIG_\$o=y\" .config || { echo \"kernel config: CONFIG_\$o not set\" >&2; exit 1; }; done
    make ARCH=arm64 CROSS_COMPILE=aarch64-unknown-linux-gnu- KBUILD_BUILD_TIMESTAMP=2026-01-01 KBUILD_BUILD_USER=hydo KBUILD_BUILD_HOST=hydo KBUILD_BUILD_VERSION=1 -j16 Image >/dev/null
    cp arch/arm64/boot/Image '$OUT/Image-svc'
  "
fi

# ---- root trees ----
copy_store() {   # <root> <path>...: copy store paths (whole) into the root tree, keeping their paths
  r=$1; shift
  for p in "$@"; do [ -e "$r$p" ] && continue; mkdir -p "$r$(dirname "$p")"; cp -a "$p" "$r$p"; done
}
copy_files() {   # <root> <file>...: copy individual files/symlinks, keeping their paths
  r=$1; shift
  for f in "$@"; do [ -e "$r$f" ] || [ -L "$r$f" ] && continue; mkdir -p "$r$(dirname "$f")"; chmod u+w "$r$(dirname "$f")" 2>/dev/null; cp -a "$f" "$r$f"; done
}
base_root() {    # <root> <init>
  rm -rf "$1"; mkdir -p "$1/bin" "$1/proc" "$1/sys" "$1/dev" "$1/tmp" "$1/etc" "$1/data"
  cp "$BUSYBOX/bin/busybox" "$1/bin/busybox"; ln -s busybox "$1/bin/sh"
  printf 'root:x:0:0:root:/root:/bin/sh\npostgres:x:70:70:postgres:/tmp:/bin/sh\n' > "$1/etc/passwd"
  printf 'root:x:0:\npostgres:x:70:\n' > "$1/etc/group"
  printf '127.0.0.1 localhost\n' > "$1/etc/hosts"
  cp "$2" "$1/init"; chmod 755 "$1/init"
}
pack() {         # <root> <out.cpio.gz>
  chmod -R u+w "$1"
  find "$1" -exec touch -h -d @1600000000 {} +
  (cd "$1" && find . | LC_ALL=C sort | nix shell nixpkgs#cpio --command cpio -o -H newc --reproducible --quiet) | gzip -n -9 > "$2"
}
# the shared-library closure: hydo-elfdeps (hydo/crates/hydo-tools), a static x86_64 program; build it
# with `cargo build --release -p hydo-tools --no-default-features --bin hydo-elfdeps --target x86_64-unknown-linux-musl`
# (on any machine, the Mac included), copy it here, and name it with HYDO_ELFDEPS if it is not on PATH
ELFDEPS=${HYDO_ELFDEPS:-hydo-elfdeps}
command -v "$ELFDEPS" >/dev/null 2>&1 || { echo "no $ELFDEPS (set HYDO_ELFDEPS; see the comment above)" >&2; exit 1; }
deps() { "$ELFDEPS" "$@"; }

# redis
R=$WORK/root-redis
base_root "$R" "$HERE/init-redis"
sed -i "s#@REDIS@#$REDIS#" "$R/init"
copy_files "$R" "$REDIS/bin/redis-server" "$REDIS/bin/redis-cli" $(deps "$REDIS/bin/redis-server" "$REDIS/bin/redis-cli")
cp -a "$(readlink -f "$REDIS/bin/redis-server")" "$R$(readlink -f "$REDIS/bin/redis-server")" 2>/dev/null || true
cat > "$R/etc/redis.conf" <<'EOF'
port 6379
bind 127.0.0.1
dir /data/redis
save ""
appendonly yes
appendfsync always
hz 1
dynamic-hz no
EOF
pack "$R" "$OUT/redis.cpio.gz"

# miniflux + postgres
M=$WORK/root-miniflux
base_root "$M" "$HERE/init-miniflux"
sed -i "s#@PG@#$PG#; s#@CURL@#$CURL#" "$M/init"
copy_store "$M" "$PG" "$PGLIB" "$TZDATA" "$CURL"
rm -f "$M$PGLIB"/lib/llvmjit* ; rm -rf "$M$PGLIB"/lib/bitcode "$M$PG"/lib/llvmjit* "$M$PG"/lib/bitcode
ELFS=$(find "$M$PG" "$M$PGLIB" "$M$CURL" -type f \( -perm -u+x -o -name '*.so*' \) | sed "s#^$M##")
copy_files "$M" $(deps $ELFS)
cp "$MINIFLUX/bin/miniflux" "$M/bin/miniflux"
copy_files "$M" $(deps "$MINIFLUX/bin/miniflux")
for r in $(nix-store -q --references "$MINIFLUX"); do case $r in *glibc*) ;; *) copy_store "$M" "$r";; esac; done
pack "$M" "$OUT/miniflux.cpio.gz"

# empty ext4 data disks, every time field fixed (this e2fsprogs honours E2FSPROGS_FAKE_TIME; VIRTIO.md)
mkdisk() {
  rm -f "$OUT/$1"
  E2FSPROGS_FAKE_TIME=1600000000 "$E2FS/bin/mke2fs" -q -F -t ext4 -b 4096 -m 0 -U "6879646f-7376-6300-0000-00000000000$3" \
    -E "hash_seed=6879646f-7376-6300-0000-00000000010$3,lazy_itable_init=0,lazy_journal_init=0" "$OUT/$1" "$2"
}
mkdisk redis-disk.img 64M 1
mkdisk pg-disk.img 256M 2

cd "$OUT" && sha256sum Image-svc redis.cpio.gz miniflux.cpio.gz redis-disk.img pg-disk.img | tee SHA256SUMS
du -sh "$R" "$M"
