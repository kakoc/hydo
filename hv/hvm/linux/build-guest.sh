#!/bin/sh
# Build the arm64 guest for the Mac VMM (hydo-hvm; hvm.c before M147) on a Linux box with nix (the build machine):
# Linux 6.6.35 + the arch_timer.cntpct change + hvm.config, and a busybox initramfs.
# Outputs in $OUT (default ./out): Image, initramfs.cpio.gz, initramfs-spin.cpio.gz, and for the virtio devices
# Image-virtio (hvm.config + hvm-virtio.config), initramfs-disk.cpio.gz, initramfs-net.cpio.gz, disk-ext4.img,
# net-rx.bin; SHA256SUMS.  Byte-reproducible
# (pinned sources, fixed build timestamp/user/host, sorted cpio with fixed mtimes, gzip -n).
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=${WORK:-$PWD}
OUT=${OUT:-$WORK/out}
KSRC_TAR=/nix/store/czg3kwyz7v5i192myvg62i66n83s0940-linux-6.6.35.tar.xz          # nixpkgs#linux_6_6.src
BUSYBOX=/nix/store/zbc5fx2yszji1a4aw2ff46m0p99c6yxb-busybox-static-aarch64-unknown-linux-musl-1.36.1
[ -f "$KSRC_TAR" ] || KSRC_TAR=$(nix build --no-link --print-out-paths nixpkgs#linux_6_6.src)
[ -x "$BUSYBOX/bin/busybox" ] || BUSYBOX=$(nix build --no-link --print-out-paths --max-jobs 0 --system aarch64-linux nixpkgs#pkgsStatic.busybox)
E2FS=/nix/store/24s0phlx9kly7x7qb8gal3mmn51ndmlx-e2fsprogs-1.47.0-bin          # the pinned mke2fs of build-linux.sh
[ -x "$E2FS/bin/mke2fs" ] || E2FS=$(dirname "$(dirname "$(ls /nix/store/*e2fsprogs-1.47*-bin/bin/mke2fs | head -1)")")
TOOLS="nixpkgs#pkgsCross.aarch64-multiplatform.buildPackages.gcc nixpkgs#gnumake nixpkgs#flex nixpkgs#bison nixpkgs#bc nixpkgs#perl nixpkgs#gcc nixpkgs#python3 nixpkgs#cpio nixpkgs#gzip"
mkdir -p "$OUT"
cd "$WORK"
[ -d linux-6.6.35 ] || tar xf "$KSRC_TAR"
tar xf "$KSRC_TAR" linux-6.6.35/drivers/clocksource/arm_arch_timer.c linux-6.6.35/drivers/clocksource/Kconfig   # pristine before patching
nix shell $TOOLS --command sh -c "
  set -e
  python3 '$HERE/apply-cntpct.py' linux-6.6.35
  cd linux-6.6.35
  make ARCH=arm64 CROSS_COMPILE=aarch64-unknown-linux-gnu- allnoconfig KCONFIG_ALLCONFIG='$HERE/hvm.config' >/dev/null
  make ARCH=arm64 CROSS_COMPILE=aarch64-unknown-linux-gnu- KBUILD_BUILD_TIMESTAMP=2026-01-01 KBUILD_BUILD_USER=hydo KBUILD_BUILD_HOST=hydo KBUILD_BUILD_VERSION=1 -j16 Image >/dev/null
  cp arch/arm64/boot/Image '$OUT/Image'
  cd '$WORK'
  # the virtio kernel: same source + patch, in its own tree
  rm -rf virtio && mkdir virtio && tar xf '$KSRC_TAR' -C virtio
  python3 '$HERE/apply-cntpct.py' virtio/linux-6.6.35
  cat '$HERE/hvm.config' '$HERE/hvm-virtio.config' > virtio/hvm-all.config
  cd virtio/linux-6.6.35
  make ARCH=arm64 CROSS_COMPILE=aarch64-unknown-linux-gnu- allnoconfig KCONFIG_ALLCONFIG='$WORK/virtio/hvm-all.config' >/dev/null
  for o in BLOCK VIRTIO_MMIO VIRTIO_BLK EXT4_FS VIRTIO_NET; do grep -q \"^CONFIG_\$o=y\" .config || { echo \"missing CONFIG_\$o\"; exit 1; }; done
  make ARCH=arm64 CROSS_COMPILE=aarch64-unknown-linux-gnu- KBUILD_BUILD_TIMESTAMP=2026-01-01 KBUILD_BUILD_USER=hydo KBUILD_BUILD_HOST=hydo KBUILD_BUILD_VERSION=1 -j16 Image >/dev/null
  cp arch/arm64/boot/Image '$OUT/Image-virtio'
  cd '$WORK'
  rm -rf rootfs && mkdir -p rootfs/bin rootfs/proc rootfs/sys rootfs/dev rootfs/tmp
  cp '$BUSYBOX/bin/busybox' rootfs/bin/busybox
  for a in sh mount echo cat uname date sleep ls dmesg seq md5sum sync poweroff; do ln -s busybox rootfs/bin/\$a; done
  cp '$HERE/init' rootfs/init && chmod 755 rootfs/init
  aarch64-unknown-linux-gnu-gcc -static -nostdlib -O2 -o rootfs/bin/cntvct '$HERE/cntvct.c'
  find rootfs -exec touch -h -d @1600000000 {} +
  (cd rootfs && find . | LC_ALL=C sort | cpio -o -H newc --reproducible --quiet) | gzip -n -9 > '$OUT/initramfs.cpio.gz'
  # variant for the spin-livelock demonstration: /bin/spinwait busy-waits for a timer signal
  aarch64-unknown-linux-gnu-gcc -static -nostdlib -fno-pie -no-pie -mgeneral-regs-only -O2 -o rootfs/bin/spinwait '$HERE/spinwait.c'
  sed -i 's#^sleep 1#/bin/spinwait\nsleep 1#' rootfs/init
  find rootfs -exec touch -h -d @1600000000 {} +
  (cd rootfs && find . | LC_ALL=C sort | cpio -o -H newc --reproducible --quiet) | gzip -n -9 > '$OUT/initramfs-spin.cpio.gz'
  # virtio test initramfs images: /init-disk (records on ext4 /dev/vda) and /init-net (eth0 + ARP)
  rm -f rootfs/bin/spinwait
  for a in mkdir umount ip; do ln -sf busybox rootfs/bin/\$a; done
  aarch64-unknown-linux-gnu-gcc -static -nostdlib -ffreestanding -fno-builtin -fno-tree-loop-distribute-patterns -fno-pie -no-pie -mgeneral-regs-only -O2 -o rootfs/bin/recs '$HERE/recs.c'
  cp '$HERE/init-disk' rootfs/init && chmod 755 rootfs/init
  find rootfs -exec touch -h -d @1600000000 {} +
  (cd rootfs && find . | LC_ALL=C sort | cpio -o -H newc --reproducible --quiet) | gzip -n -9 > '$OUT/initramfs-disk.cpio.gz'
  cp '$HERE/init-net' rootfs/init && chmod 755 rootfs/init
  find rootfs -exec touch -h -d @1600000000 {} +
  (cd rootfs && find . | LC_ALL=C sort | cpio -o -H newc --reproducible --quiet) | gzip -n -9 > '$OUT/initramfs-net.cpio.gz'
  # an empty ext4 data disk, 32 MiB: fixed UUID and hash seed, every timestamp faked to one instant
  rm -f '$OUT/disk-ext4.img'
  E2FSPROGS_FAKE_TIME=1600000000 '$E2FS/bin/mke2fs' -q -F -t ext4 -b 4096 -m 0 -U 6879646f-6876-6d00-0000-000000000001 \
    -E hash_seed=6879646f-6876-6d00-0000-000000000002,lazy_itable_init=0,lazy_journal_init=0 '$OUT/disk-ext4.img' 32M
  python3 '$HERE/mkarp.py' '$OUT/net-rx.bin' 3
"
cd "$OUT" && sha256sum Image initramfs.cpio.gz initramfs-spin.cpio.gz Image-virtio initramfs-disk.cpio.gz initramfs-net.cpio.gz disk-ext4.img net-rx.bin | tee SHA256SUMS
