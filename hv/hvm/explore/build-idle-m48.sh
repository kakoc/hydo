#!/bin/sh
# Build the M48 kernel configuration (../linux/hvm.config) plus CONFIG_ARM64_HVM_IDLE_HVC
# (apply-hvcidle.py) on the Linux build box: Image-idle, for the spin observer
# (../spin/hvm-spin.c) and the M48-style boots. The M48 Image itself is unchanged
# (../linux/build-guest.sh). Output: $OUT/Image-idle (default ./out). Byte-reproducible.
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
{ cat "$LX/hvm.config"; echo CONFIG_ARM64_HVM_IDLE_HVC=y; } > hvm-idle.allconfig
nix shell $TOOLS --command sh -c "
  set -e
  python3 '$LX/apply-cntpct.py' linux-6.6.35
  python3 '$HERE/apply-hvcidle.py' linux-6.6.35
  cd linux-6.6.35
  make ARCH=arm64 CROSS_COMPILE=aarch64-unknown-linux-gnu- allnoconfig KCONFIG_ALLCONFIG='$WORK/hvm-idle.allconfig' >/dev/null
  grep -q '^CONFIG_ARM64_HVM_IDLE_HVC=y' .config || { echo 'missing CONFIG_ARM64_HVM_IDLE_HVC'; exit 1; }
  make ARCH=arm64 CROSS_COMPILE=aarch64-unknown-linux-gnu- KBUILD_BUILD_TIMESTAMP=2026-01-01 KBUILD_BUILD_USER=hydo KBUILD_BUILD_HOST=hydo KBUILD_BUILD_VERSION=1 -j16 Image >/dev/null
  cp arch/arm64/boot/Image '$OUT/Image-idle'
"
cd "$OUT" && sha256sum Image-idle
