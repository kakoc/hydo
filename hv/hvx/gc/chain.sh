#!/bin/bash
# M138, on the bare-metal test machine: the proofs in one chain, one VM of this session at a time
# (the explorations use one worker). Results under /var/tmp/m138 (port/, xp/), progress in
# /var/tmp/m138/chain.log. Steps: port (the host-call port on the Redis and the stack images),
# xp-redis and xp-stack (an exploration with fresh VMs, with in-place reuse and its full check,
# with reuse alone; engine_equiv between the first and each other).
#   chain.sh [step ...]   (default: port xp-redis xp-stack)
set -u
G=$(cd "$(dirname "$0")" && pwd)
T=$(cd "$G/../../.." && pwd)
M=/var/tmp/m138
STEPS=${*:-port xp-redis xp-stack}
log() { echo "$(date +%H:%M:%S) $*" >> $M/chain.log; }
has() { [[ " $STEPS " == *" $1 "* ]]; }
if has port; then
  mkdir -p $M/port
  for l in redis stack; do
    bash "$G/port-runs.sh" $M/pfx $M/l-$l.txt $M/port/$l > $M/port/$l.txt 2>&1
    log "port $l: $(grep -c ': RESULT' $M/port/$l.txt) runs"
  done
fi
EQ=$T/target/release/examples/engine_equiv
xp3() {  # <name> <explorer args...>
  local name=$1; shift
  mkdir -p $M/xp
  bash "$G/xp.sh" $M/xp/$name-fresh -- "$@";                                     log "xp $name fresh: $(tail -1 $M/xp/$name-fresh.log)"
  bash "$G/xp.sh" $M/xp/$name-check HYDO_HVX_REUSE=1 HYDO_HVX_CHECK_RESET=1 -- "$@"; log "xp $name reuse+check: $(tail -1 $M/xp/$name-check.log)"
  bash "$G/xp.sh" $M/xp/$name-reuse HYDO_HVX_REUSE=1 -- "$@";                    log "xp $name reuse: $(tail -1 $M/xp/$name-reuse.log)"
  for k in check reuse; do
    "$EQ" $M/xp/$name-fresh $M/xp/$name-$k > $M/xp/$name-eq-$k.txt 2>&1
    log "equiv $name fresh/$k: exit=$? $(tail -2 $M/xp/$name-eq-$k.txt | tr '\n' ' ')"
  done
}
has xp-redis && xp3 redis --image $HOME/c3-work/img/redis_hydo/b1/explore-ob-redis-disk.img \
  --manifest hydo/onboard/examples/redis/hydo.yaml --strategy swarm --seeds 1-40 --workers 1
has xp-stack && xp3 stack --image explore-disk.img --strategy random --seeds 1-10 --workers 1
log CHAIN-END
