#!/bin/bash
# hvx_explore with burst/margin tokens on the side module (hvx/FASTTURN.md). On the box.
# A 2-vCPU schedule with qb= run twice from a fresh boot (one) must give one fingerprint;
# the same with qm=160 must give it too (the margin changes nothing); qb= on a 1-vCPU
# engine must be an ERROR.
set -u
O=/var/tmp/fastturn
B=~/hydo/build
IMG=${IMG:-explore-binlog-ret-xr-nq-disk.img}
cd $O
mkdir -p smoke
one() {	# <tag> <engine args...> -- <schedule>
  local tag=$1; shift
  local args=()
  while [ "$1" != "--" ]; do args+=("$1"); shift; done; shift
  timeout 1800 ~/bin/hvx-run sudo env HVX_DEV=/dev/hvxft HVX_SNAP_DIR=$O/snap ./hvx_explore "${args[@]}" one \
    $B/bzImage $B/initramfs-kafka.cpio.gz $B/$IMG smoke/$tag "$1" 2>&1 | grep -a "^RESULT" | sed "s/^/$tag: /"
}
S="disk=none dseed=0 qb=20000:100:250 qbs=5 | run"
one b1 --vcpus 2 -- "$S"
one b2 --vcpus 2 -- "$S"
one b3 --vcpus 2 -- "disk=none dseed=0 qb=20000:100:250 qbs=5 qm=160 | run"
one u1 -- "$S"
