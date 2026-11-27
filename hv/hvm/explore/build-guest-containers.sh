#!/bin/sh
# Build the Mac engine's guest kernel variant `containers`, Image-explore-containers, on any x86_64
# Linux machine, without Nix (docs/design/k3s-in-guest.md, Milestone 256). An image asks for it
# with `guest.kernel: containers`. Its list is Image-explore-netns-cg's (build-guest-cg.sh NETNS=1:
# the service networks' options and hvm-cgroups.config) without the options that
# hvm-containers.config names, then hvm-containers.config. PROCIO=1 (Milestone 376, the
# fingerprint epoch 7) appends hvm-procio.config too: Image-explore-containers-io, with
# /proc/<pid>/io.
# The source, the patches and the compiler are build-guest-cg.sh's: linux-6.6.35.tar.xz from
# cdn.kernel.org and the Arm GNU Toolchain 13.3.Rel1, both checked by sha256 (docs/BUILD-PINS.md,
# "Mac guest kernel for service networks").
#
#   WORK=<scratch dir> [PROCIO=1] [JOBS=6] sh hv/hvm/explore/build-guest-containers.sh
#     -> $OUT/Image-explore-containers (with PROCIO=1 Image-explore-containers-io) and its .config
#
# Byte-reproducible as build-guest-cg.sh: two builds in different directories compare equal.
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
NAME=Image-explore-containers
LIST=$HERE/hvm-containers.config
PROCIO=${PROCIO:-0}
if [ "$PROCIO" = 1 ]; then NAME=$NAME-io; fi
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
# the options this variant sets, one name per line
sed -n 's/^\(CONFIG_[A-Z0-9_]*\)=.*$/\1/p' "$LIST" > "$B/names"
cat "$LX/hvm.config" "$LX/hvm-virtio.config" "$HERE/../services/svc.config" "$HERE/hvm-explore.config" > "$B/base"
echo CONFIG_ARM64_HVM_IDLE_HVC=y >> "$B/base"        # as build-guest.sh's default
cat "$HERE/hvm-netns.config" "$HERE/hvm-cgroups.config" >> "$B/base"
grep -v -F -w -f "$B/names" "$B/base" > "$B/allconfig"
cat "$LIST" >> "$B/allconfig"
if [ "$PROCIO" = 1 ]; then cat "$HERE/hvm-procio.config" >> "$B/allconfig"; fi
python3 "$LX/apply-cntpct.py" "$B/linux-$KVER"
python3 "$HERE/apply-hvcidle.py" "$B/linux-$KVER"
cd "$B/linux-$KVER"
K="make ARCH=arm64 CROSS_COMPILE=$CROSS"
$K allnoconfig KCONFIG_ALLCONFIG="$B/allconfig" >/dev/null
# every =y line of the list must be y and every =n line must not be (allnoconfig drops an option
# silently when a dependency is missing), with Image-explore's and the service networks' own
# checks of build-guest-cg.sh
NEED="BLOCK BLK_DEV_LOOP EXT4_FS VIRTIO_MMIO VIRTIO_BLK MSDOS_PARTITION SYSVIPC FILE_LOCKING NET_SCH_PRIO
      NET_SCH_NETEM NET_CLS_U32 ARM_ARCH_TIMER_FORCE_CNTPCT ARM64_USE_LSE_ATOMICS ARM64_HVM_IDLE_HVC
      NAMESPACES NET_NS VETH BRIDGE LLC STP IPV6 IP_ADVANCED_ROUTER IP_MULTIPLE_TABLES NETFILTER
      NETFILTER_ADVANCED NETFILTER_NETLINK NETFILTER_NETLINK_QUEUE NETFILTER_XTABLES
      NETFILTER_XT_TARGET_NFQUEUE IP_NF_IPTABLES IP_NF_FILTER CGROUPS CGROUP_SCHED FAIR_GROUP_SCHED
      CFS_BANDWIDTH"
NOT="MODULES RANDOMIZE_BASE BRIDGE_IGMP_SNOOPING CGROUP_FREEZER BLK_CGROUP RT_GROUP_SCHED"
NEED="$NEED $(sed -n 's/^CONFIG_\([A-Z0-9_]*\)=y$/\1/p' "$LIST")"
NOT="$NOT $(sed -n 's/^CONFIG_\([A-Z0-9_]*\)=n$/\1/p' "$LIST")"
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
[ -n "${CONFIG_ONLY:-}" ] && { echo "$OUT/$NAME.config (CONFIG_ONLY: not built)"; exit 0; }
$K KBUILD_BUILD_TIMESTAMP=2026-01-01 KBUILD_BUILD_USER=hydo KBUILD_BUILD_HOST=hydo KBUILD_BUILD_VERSION=1 \
   -j"$JOBS" Image >/dev/null
cp arch/arm64/boot/Image "$OUT/$NAME"
cd "$OUT" && sha256sum "$NAME" "$NAME.config"
