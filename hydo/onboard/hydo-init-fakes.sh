# ---------------------------------------------------------------- fake outside services (hydo-init-fakes.sh)
# fakes: and proxies: (hvx/onboard/MANIFEST.md, "Fake outside services"). hydo-build inserts this
# file, plus renamed copies _pf_orig_<fn> of the five init functions wrapped below, into /sbin/init
# just before "main" -- ONLY for a manifest with fakes: or proxies:. Every other image keeps its
# init byte for byte.
#
# The outside world is one process, /bin/hydo-world serving /etc/hydo/world.json: every fake
# service and every fault proxy, with a record of each request it received in /tmp/hydo/world.log
# (the side-effect checks read it: hydo-world check ...). It starts before the services, in every
# boot. Protocol faults are schedule tokens:
#
#   H=<target>@<from>-<to>:<rule>[,<rule>...]
#       target  <fake|proxy> or <fake|proxy>.<route>
#       rules   status=<code>[,retry-after=<s>] | delay=<ms> | drop | reset    (requests)
#               dup | reorder | late=<ms> | lost                               (webhooks)
#               p=<1..100> (share of matching requests), effect=yes|no
#
# The rule is on while workload steps [from, to) run (switched at step boundaries, like N=), and
# off when fault injection ends. Its decisions are a pure function of the token text and the
# request's index under it, so a schedule gives the same faults on every run.
#
# A power cut stops the system under test, not the outside world: at the cut the world is frozen
# and what the checks and the recovery need -- every side effect, every event, the idempotency
# memory, the webhooks not delivered yet -- is appended to the acknowledged ids as w: tokens. The
# harness carries that line into the recovery boot's "oracle" command (hvx_explore.c, oracle_cmd),
# where start_all takes the w: tokens out again and starts the world from them.
# Just before the verdict (final_verdict) the console gets what the outside world saw: "hydo-world: <seq> ..." lines.

pf_world_start() {   # [carry file]: start the world once per boot, wait until it answers
  local n=0
  [ -f $H/world.started ] && return 0
  : > $H/world.started
  $BB sh -c "exec $HYDO_WORLD serve /etc/hydo/world.json ${1:+--carry $1}" > $H/world.out 2>&1 &
  echo $! > $H/pid.world
  while [ $n -lt 300 ]; do
    if $HYDO_WORLD ctl ping > /dev/null 2>&1; then log "fake world up: $($BB grep -c '^hydo-world: ' $H/world.out) endpoints (uptime=$(UP)s)"; return 0; fi
    $BB kill -0 $($BB cat $H/pid.world) 2>/dev/null || break
    n=$((n + 1)); $BB sleep 0.1
  done
  $BB tail -5 $H/world.out
  die_oracle "HYDO-ORACLE: ERROR the fake outside world (hydo-world) did not start"
}

start_all() {
  local c=""
  case "$PHASE:$CMD" in
    serve:oracle*)   # the recovery boot: the world the first boot carried across the power cut
      $BB grep -h '^w:' $HYDO_ACKED $HYDO_FAILED > $H/world.carry 2>/dev/null
      $BB sed -i '/^w:/d' $HYDO_ACKED $HYDO_FAILED 2>/dev/null
      c=$H/world.carry
      log "recovery boot: the fake world starts from $($BB wc -l < $c) carried records";;
  esac
  pf_world_start $c
  _pf_orig_start_all "$@"
}

pf_known() { local t; for t in $HYDO_WORLD_TARGETS; do [ "$t" = "$1" ] && return 0; done; return 1; }

pf_add() {   # <value of H=>: remember the token; the window is switched by step_range
  local tgt rest rng from to
  tgt=${1%%@*}; rest=${1#*@}; rng=${rest%%:*}; from=${rng%-*}; to=${rng#*-}
  case "$from$to" in ""|*[!0-9]*) die_oracle "HYDO-ORACLE: ERROR bad protocol fault H=$1 (window)";; esac
  [ "$rest" != "$1" ] && [ "${rest#*:}" != "$rest" ] || die_oracle "HYDO-ORACLE: ERROR bad protocol fault H=$1"
  pf_known "$tgt" || { log "H: no fake or proxy $tgt, ignored"; return 0; }
  [ -f $H/pf.tokens ] || echo "HYDO-ASSERT-DECL {\"t\":\"reachable\",\"id\":\"fault.protocol\",\"msg\":\"a protocol fault (H=) was switched on\"}"
  PF_N=$(($($BB wc -l < $H/pf.tokens 2>/dev/null || echo 0) + 1))
  echo "$PF_N $from $to $1" >> $H/pf.tokens
  log "protocol fault $PF_N: H=$1"
}
set_knob() { if [ "$1" = H ]; then pf_add "$2"; else _pf_orig_set_knob "$@"; fi; }

pf_point() {   # <step>: switch the H= rules whose window starts or ends before this step
  local id from to tok out
  [ -f $H/pf.tokens ] || return 0
  while read -r id from to tok; do
    if [ "$to" = "$1" ] && [ -f $H/pf.on.$id ]; then
      $HYDO_WORLD ctl off $id > /dev/null; $BB rm -f $H/pf.on.$id
      log "protocol fault $id off before step $1"
    fi
    if [ "$from" = "$1" ] && [ "$from" -lt "$to" ] && [ ! -f $H/pf.done.$id ]; then
      out=$($HYDO_WORLD ctl on $id "$tok") || die_oracle "HYDO-ORACLE: ERROR protocol fault H=$tok: $out"
      : > $H/pf.on.$id; : > $H/pf.done.$id
      log "PROTOCOL FAULT $id on before step $1: H=$tok"
      echo "HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"fault.protocol\",\"ok\":true,\"msg\":\"a protocol fault (H=) was switched on\"}"
    fi
  done < $H/pf.tokens
}

step_range() {   # [$1,$2): split at the H= window boundaries inside it
  local prev=$1 p
  [ -f $H/pf.tokens ] || { _pf_orig_step_range "$@"; return; }
  pf_point $1
  for p in $($BB awk -v a=$1 -v b=$2 '{ if ($2 > a && $2 < b) print $2; if ($3 > a && $3 < b) print $3 }' $H/pf.tokens | $BB sort -n -u); do
    _pf_orig_step_range $prev $p; pf_point $p; prev=$p
  done
  _pf_orig_step_range $prev $2
}

restart_dead() {   # the end of fault injection (no cut): every protocol fault off
  if [ -f $H/pf.tokens ] && [ ! -f $H/pf.healed ]; then
    : > $H/pf.healed
    $HYDO_WORLD ctl heal > /dev/null && log "protocol faults healed"
  fi
  _pf_orig_restart_dead "$@"
}

failed_ids() {   # a power cut: freeze the world, carry its records with the acknowledged ids
  _pf_orig_failed_ids "$@"
  [ "$cut" = 1 ] || return 0
  $HYDO_WORLD ctl dump > $H/world.dump 2>&1 || { log "world dump failed: $($BB head -1 $H/world.dump)"; echo "w:trunc" > $H/world.dump; }
  $BB cat $H/world.dump >> $HYDO_ACKED
  log "fake world frozen at the power cut: $($BB wc -l < $H/world.dump) records carried ($($BB wc -c < $H/world.dump) bytes)"
  return 0
}

final_verdict() {   # just before the verdict (after any liveness wait): what the outside world saw
  $HYDO_WORLD ctl summary 2> /dev/null | $BB sed 's/^/hydo-world: /'
  _pf_orig_final_verdict "$@"
}
