#!/bin/sh
# M116 experiment on the bare-metal machine: gc_hvx (hvx_explore + timed serial input, see gc_hvx.c) on the
# onboarded Redis image. One configuration = one serve process with the given request lines on
# stdin (RUN and BRANCH), plus optionally a `one` run from a fresh boot. Every VM goes through
# ~/bin/hvx-run (shared module lock), one at a time, and waits while /var/tmp/hydo-QUIET exists.
#   run-metal.sh <out dir> <name> <one-schedule or -> "<request lines>" [GC_*=... ...]
set -u
OUT=$1; NAME=$2; ONE=$3; REQ=$4; shift 4
B=${BIN:-$HOME/hydo-gc/hv/guest-control-exp/hvx/gc_hvx}
K=$HOME/hydo/build/bzImage; I=$HOME/hydo/build/initramfs-kafka.cpio.gz
IMG=${IMG:-$HOME/hydo/build/explore-ob-redis-nq-disk.img}
d=$OUT/$NAME; mkdir -p "$d/snap"
quiet() { n=0; while [ -e /var/tmp/hydo-QUIET ] && [ $n -lt 1800 ]; do sleep 2; n=$((n + 2)); done; }
quiet
printf '%b\nQUIT\n' "$REQ" | timeout 1200 ~/bin/hvx-run sudo env HVX_SNAP_DIR="$d/snap" HVX_SNAP_MALLOC=1 HVX_EXPLORE_WALL_SECS=180 ${GC_NONL:+GC_NONL=1} "$@" \
  "$B" serve "$K" "$I" "$IMG" "$d" > "$d/serve.out" 2> "$d/serve.err"
if [ "$ONE" != - ]; then
  quiet
  timeout 1200 ~/bin/hvx-run sudo env HVX_SNAP_DIR="$d/snap" HVX_SNAP_MALLOC=1 HVX_EXPLORE_WALL_SECS=180 ${GC_NONL:+GC_NONL=1} "$@" \
    "$B" one "$K" "$I" "$IMG" "$d" "$ONE" > "$d/one.out" 2> "$d/one.err"
fi
sudo chown -R "$(id -u)" "$d" 2>/dev/null
echo "== $NAME $*"
grep -h '^RESULT\|^BRANCH' "$d/serve.out" "$d/one.out" 2>/dev/null | sed 's/ secs=[0-9.]*//' | cut -c1-150
grep -h '^GC ' "$d/serve.err" | head -12
