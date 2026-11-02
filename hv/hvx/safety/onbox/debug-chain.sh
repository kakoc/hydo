#!/bin/bash
# M140, ON THE BOX under the debug kernel (onbox.sh): the checks of the main module's M138 code,
# one VM at a time, every VM through ~/bin/hvx-run, against /dev/hvx (this tree's hvx.c loaded
# by `onbox.sh module`). deploy-metal's suite runs from the Mac before this
# (HVX_METAL_TREE=hydo-dbg hv/hvx/deploy-metal.sh). Results in /var/tmp/m140a, progress in
# /var/tmp/m140a/chain.log; after each step `onbox.sh scan <step>` saves dmesg's reports.
#   debug-chain.sh [step ...]   (default: edge off on port xp rmmod)
#   edge  hvx_m138_edge_test (the port, the dirty log and the reset at their edges)
#   off   side-suite.sh off lockstep agent    (M118's and M121's checks, every feature off)
#   on    side-suite.sh on                    (the pinned reproducers x3 in one serve with
#         reuse, its check and the port; the lock-step and agent boot checks)
#   port  port-runs.sh on M138's Redis and stack images with agent 0.3.0 (serial and port, twice
#         each, and a transcript replay), compared with M138's results on the normal kernel
#   xp    the Redis swarm exploration of M138 (seeds 1-40) with reuse and its check, compared
#         with M138's fresh exploration by engine_equiv; and the agent 0.3.0 image explored over
#         the port with fresh VMs and with reuse and its check (seeds 1-20), compared
#   rmmod unload hvx, scan kmemleak and dmesg again
set -u
H=$(cd "$(dirname "$0")/../.." && pwd)	# hv/hvx
T=$(cd "$H/../.." && pwd)
G=$H/gc
O=/var/tmp/m140a
M=/var/tmp/m138
STEPS=${*:-edge off on port xp rmmod}
mkdir -p $O
log() { echo "$(date +%H:%M:%S) $*" | tee -a $O/chain.log; }
has() { [[ " $STEPS " == *" $1 "* ]]; }
scan() { bash "$H/safety/onbox/onbox.sh" scan "$1" > $O/scan-$1.txt 2>&1; log "scan $1: $(grep -E '^(kmemleak|kernel log):' $O/scan-$1.txt | tr '\n' ' ')"; }
export SUITE_OUT=$O HVX_DEV=/dev/hvx
log "chain start: $(uname -r), hvx $(cat /sys/module/hvx/srcversion 2>/dev/null)"
if has edge; then
  timeout 600 ~/bin/hvx-run sudo env HVX_DEV=/dev/hvx $O/hvx_m138_edge_test > $O/edge.txt 2>&1
  log "edge: rc=$? $(tail -1 $O/edge.txt)"; scan edge
fi
if has off; then
  bash "$G/side-suite.sh" off lockstep agent > $O/suite-off.txt 2>&1
  log "suite off: $(tail -1 $O/suite-off.txt)"; scan suite-off
fi
if has on; then
  bash "$G/side-suite.sh" on > $O/suite-on.txt 2>&1
  log "suite on: $(tail -1 $O/suite-on.txt)"; scan suite-on
fi
if has port; then
  P=$O/pfx; mkdir -p $P/bin $P/libexec
  cp -f "$T/target/release/hydo" "$T/target/release/hydo-hvx" $P/bin/
  for l in guest module tools; do ln -sfn ~/hydo-rf-work/prefix-rf/$l $P/$l; done
  for l in bin faketime; do ln -sfn ~/hydo-rf-work/prefix-rf/libexec/$l $P/libexec/$l; done
  ln -sfn ~/bin/hvx-run $P/libexec/hvx-run; ln -sfn "$T/hv" $P/hv; ln -sfn "$T/hydo" $P/hydo
  mkdir -p $O/port
  for l in redis stack; do
    bash "$G/port-runs.sh" $P $M/l-$l.txt $O/port/$l > $O/port/$l.txt 2>&1
    # the same line as M138's on the normal kernel?
    n=$(grep -c ': RESULT' $O/port/$l.txt); same=0; diff=""
    while IFS= read -r line; do
      k=${line%%: *}; ref=$(grep -F "$k: " $M/port/$l.txt | head -1)
      if [ "$line" = "$ref" ]; then same=$((same+1)); else diff="$diff [$k]"; fi
    done < <(grep ': RESULT' $O/port/$l.txt)
    log "port $l: $n runs, $same identical to M138's normal-kernel lines${diff:+, different:$diff}"
  done
  scan port
fi
if has xp; then
  EQ=$T/target/release/examples/engine_equiv
  mkdir -p $O/xp
  bash "$G/xp.sh" $O/xp/redis-check HVX_DEV=/dev/hvx HYDO_HVX_REUSE=1 HYDO_HVX_CHECK_RESET=1 -- \
    --image $HOME/c3-work/img/redis_hydo/b1/explore-ob-redis-disk.img \
    --manifest hydo/onboard/examples/redis/hydo.yaml --strategy swarm --seeds 1-40 --workers 1
  log "xp redis reuse+check: $(tail -1 $O/xp/redis-check.log)"
  "$EQ" $M/xp/redis-fresh $O/xp/redis-check > $O/xp/redis-eq-m138.txt 2>&1
  log "equiv M138 normal-kernel fresh / debug-kernel reuse+check: exit=$? $(tail -2 $O/xp/redis-eq-m138.txt | tr '\n' ' ')"
  for k in fresh check; do
    e=(HVX_DEV=/dev/hvx HYDO_HVX_HOSTCALL=1); [ $k = check ] && e+=(HYDO_HVX_REUSE=1 HYDO_HVX_CHECK_RESET=1)
    bash "$G/xp.sh" $O/xp/a3-$k "${e[@]}" -- --image $M/img/redis-a3-disk.img \
      --manifest hydo/onboard/examples/redis/hydo.yaml --strategy swarm --seeds 1-20 --workers 1
    log "xp a3 port $k: $(tail -1 $O/xp/a3-$k.log)"
  done
  "$EQ" $O/xp/a3-fresh $O/xp/a3-check > $O/xp/a3-eq.txt 2>&1
  log "equiv a3 port fresh / reuse+check: exit=$? $(tail -2 $O/xp/a3-eq.txt | tr '\n' ' ')"
  scan xp
fi
if has rmmod; then
  flock -x -w 1800 ~/.hvx-module.lock sudo rmmod hvx; log "rmmod: rc=$? $(lsmod | grep -c '^hvx ') hvx modules left"
  sleep 5; scan rmmod
fi
log CHAIN-END
