#!/bin/sh
# Build the arm64 guest for hvm.c on a Linux box with nix (the build machine):
# Linux 6.6.35 + the arch_timer.cntpct change + hvm.config, and a busybox initramfs.
# Outputs in $OUT (default ./out): Image, initramfs.cpio.gz, initramfs-spin.cpio.gz, SHA256SUMS.  Byte-reproducible
# (pinned sources, fixed build timestamp/user/host, sorted cpio with fixed mtimes, gzip -n).
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=${WORK:-$PWD}
OUT=${OUT:-$WORK/out}
KSRC_TAR=/nix/store/czg3kwyz7v5i192myvg62i66n83s0940-linux-6.6.35.tar.xz          # nixpkgs#linux_6_6.src
BUSYBOX=/nix/store/zbc5fx2yszji1a4aw2ff46m0p99c6yxb-busybox-static-aarch64-unknown-linux-musl-1.36.1
[ -f "$KSRC_TAR" ] || KSRC_TAR=$(nix build --no-link --print-out-paths nixpkgs#linux_6_6.src)
[ -x "$BUSYBOX/bin/busybox" ] || BUSYBOX=$(nix build --no-link --print-out-paths --max-jobs 0 --system aarch64-linux nixpkgs#pkgsStatic.busybox)
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
"
cd "$OUT" && sha256sum Image initramfs.cpio.gz initramfs-spin.cpio.gz | tee SHA256SUMS
