#!/bin/sh
# Build the SECOND arm64 guest kernel, Image-explore-netns (Milestone 99), on any x86_64 Linux
# machine, without Nix: the source, patches and configuration of Image-explore (build-guest.sh,
# HVC idle) plus hvm-netns.config (network namespaces, veth, bridge, IPv6, policy routing,
# iptables + NFQUEUE). hydo-build --arch arm64 picks it only for manifests with network: bridge,
# addr: or faults: [freeze]; every other arm64 image keeps Image-explore and its fingerprints.
#
#   WORK=<scratch dir> [JOBS=6] sh hv/hvm/explore/build-guest-netns.sh     -> $OUT/Image-explore-netns
#
# Pinned inputs (docs/BUILD-PINS.md, "Mac guest kernel for service networks"), downloaded into
# $DL (default $WORK/dl) if missing and checked by sha256 before use:
#   - linux-6.6.35.tar.xz from cdn.kernel.org (the same tarball as nixpkgs#linux_6_6.src);
#   - the Arm GNU Toolchain 13.3.Rel1, x86_64 host, aarch64-none-linux-gnu target, from
#     developer.arm.com. The build uses nothing else from the host but make, flex, bison, bc,
#     perl, python3 and a host gcc (for the kernel's own build tools).
# Byte-reproducible: fixed KBUILD_BUILD_* values as build-guest.sh, the same compiler, and no
# path of the build directory reaches the Image (two builds in different directories compare
# equal; checked in M99).
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
LX=$HERE/../linux
WORK=${WORK:-$PWD}
OUT=${OUT:-$WORK/out}
DL=${DL:-$WORK/dl}
JOBS=${JOBS:-6}
KVER=6.6.35
KSRC_SHA=fce3ee728712ed063aa8c14a8756c8ff8c7a46ba3827f61d2b04a73c7cf5dd9e
KSRC_URL=https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KVER.tar.xz
TC_NAME=arm-gnu-toolchain-13.3.rel1-x86_64-aarch64-none-linux-gnu
TC_SHA=322f0b4482fc0d9fa0bb468134841f08d8c554c54ff5aa29a13a7a24bf7e1eb5
TC_URL=https://developer.arm.com/-/media/Files/downloads/gnu/13.3.rel1/binrel/$TC_NAME.tar.xz
mkdir -p "$OUT" "$DL"
get() {   # <url> <file> <sha256>
  if [ ! -f "$DL/$2" ]; then curl -sSLf -o "$DL/$2.part" "$1"; mv "$DL/$2.part" "$DL/$2"; fi
  echo "$3  $DL/$2" | sha256sum -c --quiet - || { echo "sha256 mismatch: $DL/$2" >&2; exit 1; }
}
get "$KSRC_URL" linux-$KVER.tar.xz $KSRC_SHA
get "$TC_URL" $TC_NAME.tar.xz $TC_SHA
cd "$WORK"
[ -x "$WORK/$TC_NAME/bin/aarch64-none-linux-gnu-gcc" ] || tar xf "$DL/$TC_NAME.tar.xz"
CROSS=$WORK/$TC_NAME/bin/aarch64-none-linux-gnu-
rm -rf netns && mkdir netns && tar xf "$DL/linux-$KVER.tar.xz" -C netns
cat "$LX/hvm.config" "$LX/hvm-virtio.config" "$HERE/../services/svc.config" "$HERE/hvm-explore.config" > netns/allconfig
echo CONFIG_ARM64_HVM_IDLE_HVC=y >> netns/allconfig        # as build-guest.sh's default
cat "$HERE/hvm-netns.config" >> netns/allconfig
python3 "$LX/apply-cntpct.py" netns/linux-$KVER
python3 "$HERE/apply-hvcidle.py" netns/linux-$KVER
cd netns/linux-$KVER
K="make ARCH=arm64 CROSS_COMPILE=$CROSS"
$K allnoconfig KCONFIG_ALLCONFIG="$WORK/netns/allconfig" >/dev/null
# everything Image-explore must have, then everything this kernel adds (allnoconfig drops an
# option silently when a dependency is missing)
for c in BLOCK BLK_DEV_LOOP EXT4_FS VIRTIO_MMIO VIRTIO_BLK MSDOS_PARTITION SYSVIPC FILE_LOCKING NET_SCH_PRIO \
         NET_SCH_NETEM NET_CLS_U32 ARM_ARCH_TIMER_FORCE_CNTPCT ARM64_USE_LSE_ATOMICS ARM64_HVM_IDLE_HVC \
         NAMESPACES NET_NS VETH BRIDGE LLC STP IPV6 IP_ADVANCED_ROUTER IP_MULTIPLE_TABLES NETFILTER \
         NETFILTER_ADVANCED NETFILTER_NETLINK NETFILTER_NETLINK_QUEUE NETFILTER_XTABLES \
         NETFILTER_XT_TARGET_NFQUEUE IP_NF_IPTABLES IP_NF_FILTER; do
  grep -q "^CONFIG_$c=y" .config || { echo "missing CONFIG_$c" >&2; exit 1; }
done
for c in MODULES BRIDGE_NETFILTER BRIDGE_IGMP_SNOOPING NF_CONNTRACK RANDOMIZE_BASE; do
  ! grep -q "^CONFIG_$c=y" .config || { echo "unexpected CONFIG_$c=y" >&2; exit 1; }
done
cp .config "$OUT/Image-explore-netns.config"
$K KBUILD_BUILD_TIMESTAMP=2026-01-01 KBUILD_BUILD_USER=hydo KBUILD_BUILD_HOST=hydo KBUILD_BUILD_VERSION=1 \
   -j"$JOBS" Image >/dev/null
cp arch/arm64/boot/Image "$OUT/Image-explore-netns"
cd "$OUT" && sha256sum Image-explore-netns Image-explore-netns.config
