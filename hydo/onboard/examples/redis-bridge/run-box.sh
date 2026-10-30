#!/bin/sh
# M95 helper for the bare-metal test machine: run ONE schedule as a fresh boot and keep its output.
#   run-box.sh <tag> <image> <ram-mib> "<schedule>"     (output: $OUT/<tag>.{out,err}, $OUT/<tag>/)
# Waits (at most 30 min) while /var/tmp/hydo-QUIET exists; every VM goes through ~/bin/hvx-run.
# The caller starts at most two at a time. The pid is in $OUT/<tag>.pid while it runs.
set -u
tag=$1 img=$2 ram=$3 sched=$4
OUT=${OUT:-/var/tmp/m95-netns}
T=${HYDO_TREE:-$HOME/hydo-netns}
mkdir -p "$OUT/$tag"
echo $$ > "$OUT/$tag.pid"
n=0
while [ -e /var/tmp/hydo-QUIET ] && [ $n -lt 180 ]; do sleep 10; n=$((n + 1)); done
[ -e /var/tmp/hydo-QUIET ] && { echo "quiet file still there after 30 min" > "$OUT/$tag.err"; rm -f "$OUT/$tag.pid"; exit 3; }
~/bin/hvx-run sudo "$T/hvx/hvx_explore" --ram-mib "$ram" one ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz \
  "$img" "$OUT/$tag" "$sched" > "$OUT/$tag.out" 2> "$OUT/$tag.err"
echo "rc=$?" >> "$OUT/$tag.out"
rm -f "$OUT/$tag.pid"
