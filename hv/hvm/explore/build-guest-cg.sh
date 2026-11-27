#!/bin/sh
# Build the Mac engine's guest kernels of images built at the fingerprint epoch 3 and later
# (docs/design/guest-cgroups.md) on any x86_64 Linux machine, without Nix:
#   NETNS=0: Image-explore-cg, Image-explore's list (build-guest.sh) plus hvm-cgroups.config;
#   NETNS=1: Image-explore-netns-cg, Image-explore-netns's list (build-guest-netns.sh) plus
#            hvm-cgroups.config.
# PROCIO=1 (Milestone 376, the fingerprint epoch 7) appends hvm-procio.config too, so the guest
# has /proc/<pid>/io: Image-explore-io (NETNS=0) or Image-explore-netns-io (NETNS=1).
# The source, the patches and the compiler are build-guest-netns.sh's: linux-6.6.35.tar.xz from
# cdn.kernel.org and the Arm GNU Toolchain 13.3.Rel1, both checked by sha256 (docs/BUILD-PINS.md,
# "Mac guest kernel for service networks"). build-guest.sh takes its compiler from the Nix flake
# registry, which is not pinned, so the default kernel with cgroups is built here too.
#
#   WORK=<scratch dir> NETNS=0|1 [PROCIO=1] [JOBS=6] sh hv/hvm/explore/build-guest-cg.sh
#     -> $OUT/Image-explore-cg or $OUT/Image-explore-netns-cg (with PROCIO=1 Image-explore-io or
#        Image-explore-netns-io), and its .config
#
# Byte-reproducible as build-guest-netns.sh: two builds in different directories compare equal.
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
LX=$HERE/../linux
WORK=${WORK:-$PWD}
OUT=${OUT:-$WORK/out}
DL=${DL:-$WORK/dl}
JOBS=${JOBS:-6}
NETNS=${NETNS:?set NETNS to 0 (Image-explore-cg) or 1 (Image-explore-netns-cg)}
KVER=6.6.35
KSRC_SHA=fce3ee728712ed063aa8c14a8756c8ff8c7a46ba3827f61d2b04a73c7cf5dd9e
KSRC_URL=https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KVER.tar.xz
TC_NAME=arm-gnu-toolchain-13.3.rel1-x86_64-aarch64-none-linux-gnu
TC_SHA=322f0b4482fc0d9fa0bb468134841f08d8c554c54ff5aa29a13a7a24bf7e1eb5
TC_URL=https://developer.arm.com/-/media/Files/downloads/gnu/13.3.rel1/binrel/$TC_NAME.tar.xz
if [ "$NETNS" = 1 ]; then NAME=Image-explore-netns-cg; else NAME=Image-explore-cg; fi
PROCIO=${PROCIO:-0}
if [ "$PROCIO" = 1 ]; then NAME=${NAME%-cg}-io; fi
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
B=$WORK/$NAME.build
rm -rf "$B" && mkdir "$B" && tar xf "$DL/linux-$KVER.tar.xz" -C "$B"
cat "$LX/hvm.config" "$LX/hvm-virtio.config" "$HERE/../services/svc.config" "$HERE/hvm-explore.config" > "$B/allconfig"
echo CONFIG_ARM64_HVM_IDLE_HVC=y >> "$B/allconfig"        # as build-guest.sh's default
if [ "$NETNS" = 1 ]; then cat "$HERE/hvm-netns.config" >> "$B/allconfig"; fi
cat "$HERE/hvm-cgroups.config" >> "$B/allconfig"
if [ "$PROCIO" = 1 ]; then cat "$HERE/hvm-procio.config" >> "$B/allconfig"; fi
python3 "$LX/apply-cntpct.py" "$B/linux-$KVER"
python3 "$HERE/apply-hvcidle.py" "$B/linux-$KVER"
cd "$B/linux-$KVER"
K="make ARCH=arm64 CROSS_COMPILE=$CROSS"
$K allnoconfig KCONFIG_ALLCONFIG="$B/allconfig" >/dev/null
# everything Image-explore must have (allnoconfig drops an option silently when a dependency is
# missing), then the service networks' options, then the cgroup lines (each =y line must be y,
# each =n line must not be)
NEED="BLOCK BLK_DEV_LOOP EXT4_FS VIRTIO_MMIO VIRTIO_BLK MSDOS_PARTITION SYSVIPC FILE_LOCKING NET_SCH_PRIO
      NET_SCH_NETEM NET_CLS_U32 ARM_ARCH_TIMER_FORCE_CNTPCT ARM64_USE_LSE_ATOMICS ARM64_HVM_IDLE_HVC"
NOT="MODULES RANDOMIZE_BASE"
if [ "$NETNS" = 1 ]; then
  NEED="$NEED NAMESPACES NET_NS VETH BRIDGE LLC STP IPV6 IP_ADVANCED_ROUTER IP_MULTIPLE_TABLES NETFILTER
        NETFILTER_ADVANCED NETFILTER_NETLINK NETFILTER_NETLINK_QUEUE NETFILTER_XTABLES
        NETFILTER_XT_TARGET_NFQUEUE IP_NF_IPTABLES IP_NF_FILTER"
  NOT="$NOT BRIDGE_NETFILTER BRIDGE_IGMP_SNOOPING NF_CONNTRACK"
else
  NOT="$NOT NET_NS VETH BRIDGE IPV6 NETFILTER"
fi
NEED="$NEED $(sed -n 's/^CONFIG_\([A-Z0-9_]*\)=y$/\1/p' "$HERE/hvm-cgroups.config")"
NOT="$NOT $(sed -n 's/^CONFIG_\([A-Z0-9_]*\)=n$/\1/p' "$HERE/hvm-cgroups.config")"
if [ "$PROCIO" = 1 ]; then
  NEED="$NEED $(sed -n 's/^CONFIG_\([A-Z0-9_]*\)=y$/\1/p' "$HERE/hvm-procio.config")"
  NOT="$NOT $(sed -n 's/^CONFIG_\([A-Z0-9_]*\)=n$/\1/p' "$HERE/hvm-procio.config")"
fi
for c in $NEED; do
  grep -q "^CONFIG_$c=y" .config || { echo "missing CONFIG_$c" >&2; exit 1; }
done
for c in $NOT; do
  ! grep -q "^CONFIG_$c=y" .config || { echo "unexpected CONFIG_$c=y" >&2; exit 1; }
done
cp .config "$OUT/$NAME.config"
$K KBUILD_BUILD_TIMESTAMP=2026-01-01 KBUILD_BUILD_USER=hydo KBUILD_BUILD_HOST=hydo KBUILD_BUILD_VERSION=1 \
   -j"$JOBS" Image >/dev/null
cp arch/arm64/boot/Image "$OUT/$NAME"
cd "$OUT" && sha256sum "$NAME" "$NAME.config"
