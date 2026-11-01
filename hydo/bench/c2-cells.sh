#!/usr/bin/env bash
# hydo/bench/c2-cells.sh -- the same hydo-bench steps on several kinds of benchmark image, one
# step at a time, each step resumable (a step with a done marker is skipped). Written for the C2
# comparison (Milestone 129, docs/C2-BENCH.md: shell against agent images of hydo/bench/manifest);
# since Milestone 135 the images of both kinds sit in one directory under their own names.
#
#   c2-cells.sh <image dir> <out dir> <plan file>
#
# <image dir> holds the images build-bench-manifest.sh made (explore-ob-bench-<stem>-disk.img and
# explore-ob-bench-<stem>-agent-disk.img), plus bzImage and initramfs-kafka.cpio.gz. Each line of
# <plan file> is `<step name> <hydo-bench arguments without --out and --kind>`; the step runs for
# every kind in $KINDS (default "shell agent"; `stack` runs the build-stack.sh images in
# $STACK_BUILD, default ~/hydo/build), into <out dir>/<kind>/<step name>. Waits while
# /var/tmp/hydo-QUIET exists (the pool also honours it between runs). Every step has a timeout.
# pid file: <out dir>/cells.pid. $HYDO_TREE (default ~/hydo) is the tree whose hydo-bench runs; the
# run engine is the Rust one (in hydo-bench's process when it can open /dev/hvx, otherwise that
# tree's target/release/hydo-hvx; $HYDO_HVX_EXPLORE names another engine program; M137).
set -u
imgs=$1; out=$2; plan=$3
T=${HYDO_TREE:-$HOME/hydo}
B=${HYDO_BENCH_BIN:-$T/target/release/hydo-bench}
mkdir -p "$out"
echo $$ > "$out/cells.pid"
export HYDO_QUIET_FILE=/var/tmp/hydo-QUIET HYDO_M59_SUFFIX=-pin-kr
export HYDO_HVX_EXPLORE=${HYDO_HVX_EXPLORE:-$T/target/release/hydo-hvx}
unset HYDO_EXPLORE_CACHE
log() { echo "$(date -u +%FT%TZ) $*" >> "$out/progress"; }
while read -r name args; do
  case "$name" in ''|'#'*) continue;; esac
  for kind in ${KINDS:-shell agent}; do
    d=$out/$kind/$name
    [ -f "$d.done" ] && continue
    n=0; while [ -e /var/tmp/hydo-QUIET ] && [ $n -lt 720 ]; do sleep 10; n=$((n + 1)); done
    # a stack-sized guest needs about 6 GiB: wait (at most 4 h) until that much memory is free
    # (the box ran out of memory with seven such VMs, 2026-11-01); run steps with --workers 1
    n=0; while [ "$(awk '/^MemAvailable:/ {print int($2 / 1024)}' /proc/meminfo)" -lt "${MIN_FREE_MIB:-6144}" ] && [ $n -lt 480 ]; do sleep 30; n=$((n + 1)); done
    rm -rf "$d"; mkdir -p "$out/$kind"
    dir=$imgs; [ "$kind" = stack ] && dir=${STACK_BUILD:-$HOME/hydo/build}
    log "start $kind $name"
    # shellcheck disable=SC2086
    HYDO_BUILD=$dir timeout -s KILL ${STEP_TIMEOUT:-10800} "$B" $args --kind "$kind" --out "$d" > "$d.out" 2> "$d.err"
    rc=$?
    log "done $kind $name rc=$rc"
    [ $rc = 0 ] && touch "$d.done"
  done
done < "$plan"
log "all done"
touch "$out/cells.done"
