#!/bin/sh
# Build the arm64 kernel for manifest-built guests (hvx/onboard/hydo-build --arch arm64) on the
# build machine: the same Linux 6.6.35 + arch_timer.cntpct change as
# ../linux/build-guest.sh, with ../linux/hvm.config + ../linux/hvm-virtio.config (virtio-mmio disk
# and network, ext4) + ../services/svc.config (the syscalls real services need) + hvm-explore.config
# (the loop driver, for a manifest image run without a disk). Output: $OUT/Image-explore (default
# ./out). Byte-reproducible like the M48 kernel.
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
LX=$HERE/../linux
WORK=${WORK:-$PWD}
OUT=${OUT:-$WORK/out}
KSRC_TAR=/nix/store/czg3kwyz7v5i192myvg62i66n83s0940-linux-6.6.35.tar.xz          # nixpkgs#linux_6_6.src
[ -f "$KSRC_TAR" ] || KSRC_TAR=$(nix build --no-link --print-out-paths nixpkgs#linux_6_6.src)
TOOLS="nixpkgs#pkgsCross.aarch64-multiplatform.buildPackages.gcc nixpkgs#gnumake nixpkgs#flex nixpkgs#bison nixpkgs#bc nixpkgs#perl nixpkgs#gcc nixpkgs#python3"
mkdir -p "$OUT"
cd "$WORK"
rm -rf linux-6.6.35 && tar xf "$KSRC_TAR"
cat "$LX/hvm.config" "$LX/hvm-virtio.config" "$HERE/../services/svc.config" "$HERE/hvm-explore.config" > hvm-explore.allconfig
nix shell $TOOLS --command sh -c "
  set -e
  python3 '$LX/apply-cntpct.py' linux-6.6.35
  cd linux-6.6.35
  make ARCH=arm64 CROSS_COMPILE=aarch64-unknown-linux-gnu- allnoconfig KCONFIG_ALLCONFIG='$WORK/hvm-explore.allconfig' >/dev/null
  for c in BLOCK BLK_DEV_LOOP EXT4_FS VIRTIO_MMIO VIRTIO_BLK MSDOS_PARTITION SYSVIPC FILE_LOCKING ARM_ARCH_TIMER_FORCE_CNTPCT ARM64_USE_LSE_ATOMICS; do grep -q \"^CONFIG_\$c=y\" .config || { echo \"missing CONFIG_\$c\"; exit 1; }; done
  make ARCH=arm64 CROSS_COMPILE=aarch64-unknown-linux-gnu- KBUILD_BUILD_TIMESTAMP=2026-01-01 KBUILD_BUILD_USER=hydo KBUILD_BUILD_HOST=hydo KBUILD_BUILD_VERSION=1 -j16 Image >/dev/null
  cp arch/arm64/boot/Image '$OUT/Image-explore'
"
cd "$OUT" && sha256sum Image-explore
