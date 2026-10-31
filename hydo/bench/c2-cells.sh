#!/usr/bin/env bash
# hydo/bench/c2-cells.sh -- the C2 benchmark comparison (Milestone 129, docs/C2-BENCH.md): the
# same hydo-bench steps on the shell images and on the agent images of hydo/bench/manifest, one
# step at a time, each step resumable (a step with a done marker is skipped).
#
#   c2-cells.sh <work dir> <out dir> <plan file>
#
# <work dir>/shell and <work dir>/agent hold the images under hydo-bench's names (made by
# build-bench-manifest.sh), plus bzImage and initramfs-kafka.cpio.gz. Each line of <plan file> is
# `<step name> <hydo-bench arguments without --out>`; the step runs for kind=shell, then
# kind=agent, into <out dir>/<kind>/<step name>. Waits while /var/tmp/hydo-QUIET exists (the
# pool also honours it between runs). Every step has a timeout. pid file: <out dir>/cells.pid.
set -u
work=$1; out=$2; plan=$3
B=${HYDO_BENCH_BIN:-$HOME/hydo-c2b/target/release/hydo-bench}
mkdir -p "$out"
echo $$ > "$out/cells.pid"
export HYDO_QUIET_FILE=/var/tmp/hydo-QUIET HYDO_M59_SUFFIX=-pin-kr
export HYDO_HVX_EXPLORE=${HYDO_HVX_EXPLORE:-$HOME/hydo-c2b/hv/hvx/hvx_explore}
unset HYDO_EXPLORE_CACHE
log() { echo "$(date -u +%FT%TZ) $*" >> "$out/progress"; }
while read -r name args; do
  case "$name" in ''|'#'*) continue;; esac
  for kind in shell agent; do
    d=$out/$kind/$name
    [ -f "$d.done" ] && continue
    n=0; while [ -e /var/tmp/hydo-QUIET ] && [ $n -lt 720 ]; do sleep 10; n=$((n + 1)); done
    rm -rf "$d"; mkdir -p "$out/$kind"
    log "start $kind $name"
    # shellcheck disable=SC2086
    HYDO_BUILD=$work/$kind timeout -s KILL ${STEP_TIMEOUT:-10800} "$B" $args --out "$d" > "$d.out" 2> "$d.err"
    rc=$?
    log "done $kind $name rc=$rc"
    [ $rc = 0 ] && touch "$d.done"
  done
done < "$plan"
log "all done"
touch "$out/cells.done"
