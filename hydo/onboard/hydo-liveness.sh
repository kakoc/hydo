# ---------------------------------------------------------------- liveness (hydo-liveness.sh)
# Bounded-liveness invariants (`type: eventually` in hydo.yaml; hvx/onboard/MANIFEST.md).
# hydo-build inserts this file, plus renamed copies _live_orig_<fn> of the four init functions
# wrapped below, into /sbin/init just before "main" -- ONLY for a manifest that has an
# `eventually` invariant. Every other image keeps hydo-init.sh byte for byte.
#
# An eventually invariant is a check command polled from an ANCHOR until it exits 0 or its
# deadline (`within:` seconds of guest time, /proc/uptime -- deterministic under hvx) passes:
#   heal   fault injection has ended: the workload's last step has run (or, after a power cut,
#          the recovery boot starts). Everything the init then does to heal -- lift network
#          faults, end pauses, restart killed services, the workload's retry, crash recovery --
#          counts against the deadline.
#   start  the workload starts (after a power cut: the recovery boot starts).
# Check exit status: 0 = satisfied, anything else = not yet (2 = could not check yet). The
# monitor runs in the background from the anchor; `finish` waits for it and emits ONE
# HYDO-ASSERT record per invariant: ok=true if satisfied by the deadline, ok=false if the
# deadline passed first (a violation, and the HYDO-ORACLE verdict names it).

live_anchor() {   # <heal|start>: stamp the anchor (first time only) and start its monitors
  local a=$1 i
  [ -f $H/live.anchor.$a ] && return 0
  UP > $H/live.anchor.$a
  for i in $HYDO_INVARIANTS; do
    [ "$(iv $i type)" = eventually ] && [ "$(iv $i after)" = "$a" ] || continue
    live_monitor $i &
    echo $! > $H/live.pid.$i
  done
  log "liveness anchor '$a' at uptime=$($BB cat $H/live.anchor.$a)s"
}

live_monitor() {   # <inv>: result -> $H/live.res.<inv> = "<ok|late|expired> <elapsed-s> <last rc>"
  local i=$1 t0 w p el rc r
  t0=$($BB cat $H/live.anchor.$(iv $i after)); w=$(iv $i within); p=$(iv $i poll)
  while :; do
    $BB timeout $((w + 1)) $BB sh -c ". /etc/hydo/prelude.sh; $(iv $i cmd)" > $H/inv.$i.out 2>&1
    rc=$?
    el=$($BB awk -v a="$t0" -v b="$(UP)" 'BEGIN { printf "%.2f", b - a }')
    if [ $rc = 0 ]; then
      if $BB awk -v e="$el" -v w="$w" 'BEGIN { exit !(e <= w) }'; then r=ok; else r=late; fi
      break
    fi
    if $BB awk -v e="$el" -v w="$w" 'BEGIN { exit !(e > w) }'; then r=expired; break; fi
    $BB sleep $p
  done
  echo "$r $el $rc" > $H/live.res.$i.tmp && $BB mv $H/live.res.$i.tmp $H/live.res.$i
}

live_resolve() {   # wait for every monitor, emit the records, remember the first violation
  local i a res r el rc d ok
  for i in $HYDO_INVARIANTS; do
    [ "$(iv $i type)" = eventually ] || continue
    a=$(iv $i after)
    [ -f $H/live.anchor.$a ] || live_anchor $a      # an anchor never reached: it is now
    wait $($BB cat $H/live.pid.$i) 2>/dev/null
    res=$($BB cat $H/live.res.$i 2>/dev/null); set -- $res; r=${1:-expired}; el=${2:-?}; rc=${3:-?}
    case $r in
      ok) ok=true
          echo "satisfied ${el}s after $a (within $(iv $i within)s): $(inv_detail $H/inv.$i.out)" >> $H/inv.$i.out;;
      late) ok=false
          echo "satisfied only ${el}s after $a, deadline $(iv $i within)s: $(inv_detail $H/inv.$i.out)" >> $H/inv.$i.out;;
      *) ok=false
          echo "not satisfied ${el}s after $a, deadline $(iv $i within)s (last check rc=$rc: $(inv_detail $H/inv.$i.out))" >> $H/inv.$i.out;;
    esac
    d=$(inv_detail $H/inv.$i.out)
    echo "HYDO-ASSERT {\"t\":\"eventually\",\"id\":\"$(iv $i id)\",\"ok\":$ok,\"msg\":\"$(jesc "$(iv $i msg)")\",\"details\":{\"after\":\"$a\",\"elapsed_s\":\"$el\",\"out\":\"$(jesc "$d")\",\"within_s\":$(iv $i within)}}"
    log "liveness $(iv $i id): $r after ${el}s (deadline $(iv $i within)s after $a)"
    [ $ok = true ] || [ -f $H/violated ] || echo "$i" > $H/violated
  done
}

# the wrapped init functions (originals: _live_orig_<fn>, copied by hydo-build)
step_range() { _live_orig_step_range "$@"; [ "$2" = "$HYDO_STEPS" ] && live_anchor heal; return 0; }
start_periodic() { _live_orig_start_periodic "$@"; live_anchor start; }
start_all() { [ "$2" = recovery ] && { live_anchor heal; live_anchor start; }; _live_orig_start_all "$@"; }
finish() { live_resolve; _live_orig_finish "$@"; }

