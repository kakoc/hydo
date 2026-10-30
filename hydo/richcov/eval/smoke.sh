#!/bin/bash
# smoke.sh <image> <outdir> <schedule>...  : serve mode, whole-system PT, norandmaps
IMG=$1; OUT=$2; shift 2
S=~/hydo/build/${IMG%-disk.img}-syms
mkdir -p $OUT/consoles
{ i=0; for s in "$@"; do echo "RUN t$i $s"; i=$((i+1)); done; echo QUIT; } | \
~/bin/hvx-run sudo env HVX_SNAP_DIR=/var/tmp/hvx-snap HVX_EXPLORE_CMDLINE_EXTRA=norandmaps \
  HVX_EXPLORE_PT_RANGES=40001000-20006da571,5555562d2000-555558ba8e6d HVX_EXPLORE_PT_MIB=${PTMIB:-512} \
  HVX_EXPLORE_PTMULTI="$HOME/hydo-rich/hvx/richcov/ptmulti hydostack=$S/hydostack redis=$S/redis-server mysqld=$S/mysqld@0x555555554000" \
  $HOME/hydo-rich/hvx/hvx_explore serve ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz ~/hydo/build/$IMG $OUT/consoles
