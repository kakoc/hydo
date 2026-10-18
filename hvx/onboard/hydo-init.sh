#!/bin/busybox sh
# /sbin/init of every manifest-built image (hvx/onboard/MANIFEST.md). ONE script for every
# project: everything project-specific comes from /etc/hydo/manifest.sh, which
# hvx/onboard/hydo-build compiles from the project's hydo.yaml. The protocol with the host is
# the explorer's (hvx/hvx_explore.c, hvx/explore/stack-explore.sh), so explore.py, the report
# and hydo_assert_report work unchanged:
#
#   S0  "hydo-explore-ready-S0": root mounted, data disk not mounted, nothing started.
#       command  provision                  -> mount data, start services, provision, sync, S1
#       command  oracle <id>... [failed <id>...]
#                                           -> recovery boot after a power cut: mount the
#                                              durable data disk, start services (= crash
#                                              recovery), run the final invariants
#   S1  "hydo-explore-ready-S1": everything up and synced, no workload step run yet.
#       command  run <tokens>               -> run workload steps 0..steps-1 under the faults:
#           K=<svc>@<at>+<down>   SIGKILL <svc> before step <at>, restart before <at>+<down>
#           P=<svc>@<at>+<n>      SIGSTOP <svc> before step <at>, SIGCONT before <at>+<n>
#           P=<svc>@<at>~<off>:<dur>  pause after <off> ms of guest time, for <dur> ms
#           N=<svc>@<from>-<to>:<netem>   netem on traffic to <svc>'s port, steps [from,to)
#           O=<svc>@<from>-<to>:<req|resp>:<netem>   one direction only
#           C=<at>                power cut before step <at>
#           T=<at>:<delta>        step the realtime clock by <delta> s before step <at>
#           <X>=<value>           a durability knob the manifest defines (e.g. M=2, R=everysec)
#       then (no cut) "hydo-explore-workload-done", wait for "go", run the final invariants.
#   Every verdict is one HYDO-ORACLE line, then "hydo-stack-marker: workload complete".
#
# Without hydo_phase=serve on the kernel command line the image runs SOLO: provision, every
# step, retry, final invariants -- the same verdict lines, no host interaction.
BB=/bin/busybox
[ -c /dev/null ] || $BB mount -t devtmpfs none /dev 2>/dev/null
$BB mount -t proc none /proc 2>/dev/null
$BB mount -t sysfs none /sys 2>/dev/null
$BB mount -t tmpfs none /tmp 2>/dev/null
H=/tmp/hydo; $BB mkdir -p $H
. /etc/hydo/manifest.sh
. /etc/hydo/prelude.sh     # the project's env + shell helpers (also sourced by every command)
RUN() { $BB sh -c ". /etc/hydo/prelude.sh; $1"; }   # every manifest command runs through this
export HYDO_STEPS HYDO_ACKED=$H/acked HYDO_OUTCOMES=$H/outcomes HYDO_FAILED=$H/failed
: > $HYDO_ACKED; : > $HYDO_OUTCOMES; : > $HYDO_FAILED
echo ""
echo "=================================================="
echo "  hydo: generic init for project '$HYDO_NAME' (hvx/onboard)"
echo "  uname: $($BB uname -sr)"
echo "=================================================="
PHASE=solo
for tok in $($BB cat /proc/cmdline 2>/dev/null); do
  case "$tok" in hydo_phase=*) PHASE="${tok#hydo_phase=}";; esac
done
UP() { $BB cut -d' ' -f1 /proc/uptime 2>/dev/null; }
log() { echo "hydo-ob: $*"; }
/bin/hvx_seedrng 2>/dev/null || log "hvx_seedrng missing/failed"
$BB ifconfig lo 127.0.0.1 netmask 255.0.0.0 up 2>/dev/null
$BB ip link set lo up 2>/dev/null
log "phase=$PHASE services=[$HYDO_SERVICES] steps=$HYDO_STEPS"

die_oracle() {   # $1 = verdict line; the stop markers follow so the host can finish
  stop_periodic
  echo "$1"
  echo "hydo-stack-marker: workload complete"; echo "hydo-boot-marker: launching shell on ttyS0"
  while true; do $BB sleep 100; done
}

# ---------------------------------------------------------------- manifest accessors
sv() { eval "printf '%s' \"\${HYDO_SVC_${1}_$2}\""; }      # sv <svc> <field>
iv() { eval "printf '%s' \"\${HYDO_INV_${1}_$2}\""; }      # iv <inv> <field>
kv() { eval "printf '%s' \"\${HYDO_KNOB_${1}_$2}\""; }     # kv <letter> <field>
known_svc() { for s in $HYDO_SERVICES; do [ "$s" = "$1" ] && return 0; done; return 1; }
svc_pid() { $BB cat $H/pid.$1 2>/dev/null; }
svc_alive() { local _p; _p=$(svc_pid "$1"); [ -n "$_p" ] && $BB kill -0 "$_p" 2>/dev/null; }
svc_port() { sv "$1" port; }
svc_tree() {   # <svc>: the service's PID and every descendant (a multi-process server such as
               # PostgreSQL forks backends; a signal to the first PID alone misses them)
  local r; r=$(svc_pid "$1"); [ -n "$r" ] || return 0
  $BB cat /proc/[0-9]*/stat 2>/dev/null | $BB sed 's/ (.*) / x /' | $BB awk -v r="$r" '
    { p[$1] = $4 } END { s[r] = 1; ch = 1
      while (ch) { ch = 0; for (i in p) if (!(i in s) && (p[i] in s)) { s[i] = 1; ch = 1 } }
      print r; for (i in s) if (i != r) print i }'
}
svc_sig() { $BB kill -$2 $(svc_tree "$1") 2>/dev/null; }   # <svc> <SIG>: the whole tree

# ---------------------------------------------------------------- services
start_svc() {   # $1 = name; runs setup (if any), the command in the background, waits ready
  local s=$1 L=$H/$1.log setup to n   # local: callers loop over $s / $p
  L=$H/$s.log
  [ -f $L ] && $BB cat $L >> $H/$s.log.old && : > $L
  export HYDO_SVC=$s HYDO_LOG=$L
  setup=$(sv $s setup)
  if [ -n "$setup" ]; then
    RUN "$setup" >> $H/$s.setup.log 2>&1 || { log "$s setup FAILED"; $BB tail -8 $H/$s.setup.log; return 1; }
  fi
  # not through RUN: $! must be the service itself (sh execs into it), so a kill or a pause
  # hits the service and not a wrapper shell
  $BB sh -c ". /etc/hydo/prelude.sh; exec $(sv $s cmd)" > $L 2>&1 &
  echo $! > $H/pid.$s
  to=$(sv $s timeout); n=0
  while [ $n -lt $to ]; do
    if RUN "$(sv $s ready)" > /dev/null 2>&1; then log "$s ready after $n polls (uptime=$(UP)s)"; return 0; fi
    svc_alive $s || { log "$s DIED"; $BB tail -8 $L; return 1; }
    n=$((n + 1)); $BB sleep 1
  done
  log "$s NOT ready after ${to}s"; $BB tail -8 $L; return 1
}
start_all() {   # $1 = verdict prefix if a service cannot come up; $2 = recovery (skip recovery: false)
  local s
  for s in $HYDO_SERVICES; do
    [ "$2" = recovery ] && [ "$(sv $s recovery)" = 0 ] && continue
    start_svc $s || die_oracle "$1 ($s)"
  done
}
restart_dead() {   # $1 = verdict prefix
  local s
  for s in $HYDO_SERVICES; do
    svc_alive $s && continue
    log "$s is down; restarting it"
    start_svc $s || die_oracle "$1 ($s)"
  done
}

mount_data() {   # $1 = verdict if the disk cannot be mounted
  MERR=$($BB mount -t ext4 -o "$HYDO_DATA_OPTS" /dev/vda2 /data 2>&1) || die_oracle "$1 ($MERR)"
  for d in $HYDO_DATA_DIRS; do $BB mkdir -p "/data/$d"; done
  log "mounted /dev/vda2 on /data ($HYDO_DATA_OPTS)"
}

# ---------------------------------------------------------------- assertions + invariants
jesc() { printf '%s' "$1" | $BB tr -d '\000-\037' | $BB sed 's/\\/\\\\/g; s/"/\\"/g'; }
declare_all() {
  for i in $HYDO_INVARIANTS; do
    echo "HYDO-ASSERT-DECL {\"t\":\"$(iv $i type)\",\"id\":\"$(iv $i id)\",\"msg\":\"$(jesc "$(iv $i msg)")\"}"
  done
}
assert_once() {   # <inv> <true|false> <details>: one HYDO-ASSERT record per (id, outcome)
  local f=$H/asserted.$1.$2
  [ -f $f ] && return 0
  : > $f
  echo "HYDO-ASSERT {\"t\":\"$(iv $1 type)\",\"id\":\"$(iv $1 id)\",\"ok\":$2,\"msg\":\"$(jesc "$(iv $1 msg)")\",\"details\":{\"out\":\"$(jesc "$3")\"}}"
}
inv_detail() {   # the invariant's own HYDO-ORACLE line (prefix stripped) or its last line
  local d
  d=$($BB grep -a "HYDO-ORACLE:" $1 2>/dev/null | $BB tail -1 | $BB sed 's/.*HYDO-ORACLE: *//; s/^OK *//; s/^VIOLATION *//')
  [ -n "$d" ] || d=$($BB grep -av '^ *$' $1 2>/dev/null | $BB grep -av '^HYDO-ASSERT' | $BB tail -1)
  printf '%s' "$d"
}
eval_inv() {   # <inv>: 0 holds, 1 violated, 2 could not be evaluated. Output -> $H/inv.<inv>.out
  local o=$H/inv.$1.out rc
  RUN "$(iv $1 cmd)" > $o 2>&1
  rc=$?
  case $rc in
    0) assert_once $1 true "$(inv_detail $o)"; return 0;;
    2) return 2;;
    *) assert_once $1 false "$(inv_detail $o)"
       [ -f $H/violated ] || echo "$1" > $H/violated
       return 1;;
  esac
}

periodic_loop() {   # <inv>: every <every> seconds of guest time while the workload runs
  local e; e=$(iv $1 every)
  while [ ! -f $H/stop_periodic ]; do
    $BB sleep $e
    [ -f $H/stop_periodic ] && break
    eval_inv $1
  done
}
start_periodic() {
  $BB rm -f $H/stop_periodic
  PERIODIC_PIDS=""
  for i in $HYDO_INVARIANTS; do
    [ -n "$(iv $i every)" ] || continue
    periodic_loop $i & PERIODIC_PIDS="$PERIODIC_PIDS $!"
  done
}
stop_periodic() {   # stop the checkers deterministically: kill (no further output)
  : > $H/stop_periodic
  for p in $PERIODIC_PIDS; do $BB kill $p 2>/dev/null; done
  PERIODIC_PIDS=""
}

final_verdict() {   # every `final` invariant, in manifest order; one HYDO-ORACLE line
  nheld=0; first=""
  for i in $HYDO_INVARIANTS; do
    [ "$(iv $i final)" = 1 ] || continue
    eval_inv $i; rc=$?
    case $rc in
      0) nheld=$((nheld + 1)); [ -n "$first" ] || first="$(iv $i id): $(inv_detail $H/inv.$i.out)";;
      2) [ -f $H/violated ] || echo "HYDO-ORACLE: ERROR invariant $(iv $i id) could not be evaluated: $(inv_detail $H/inv.$i.out)" > $H/error;;
    esac
  done
  if [ -f $H/violated ]; then
    v=$($BB cat $H/violated)
    echo "HYDO-ORACLE: VIOLATION [$(iv $v id)] $(inv_detail $H/inv.$v.out)"
  elif [ -f $H/error ]; then
    $BB cat $H/error
  else
    echo "HYDO-ORACLE: OK invariants=$nheld held ($first)"
  fi
}

finish() {   # final invariants + markers (same boot or recovery boot)
  final_verdict
  echo "hydo-stack-marker: workload complete"
  echo "hydo-boot-marker: launching shell on ttyS0"
  exec $BB sh
}

# ---------------------------------------------------------------- workload
step_range() {   # run workload steps [$1,$2), bounded by workload.timeout seconds
  local rc
  [ "$1" -lt "$2" ] || return 0
  HYDO_FROM=$1 HYDO_TO=$2 $BB timeout $HYDO_WORKLOAD_TIMEOUT $BB sh -c ". /etc/hydo/prelude.sh; $HYDO_WORKLOAD_RUN" >> $H/workload.log 2>&1
  rc=$?
  [ $rc = 143 ] && log "workload steps $1..$(($2 - 1)) TIMED OUT after ${HYDO_WORKLOAD_TIMEOUT}s (killed)"
  log "workload steps $1..$(($2 - 1)) rc=$rc (uptime=$(UP)s)"
}
retry_workload() {
  [ -n "$HYDO_WORKLOAD_RETRY" ] || return 0
  $BB timeout $HYDO_WORKLOAD_TIMEOUT $BB sh -c ". /etc/hydo/prelude.sh; $HYDO_WORKLOAD_RETRY" >> $H/workload.log 2>&1
  log "workload retry rc=$? (uptime=$(UP)s)"
}
failed_ids() {   # ids whose LAST outcome was a definite failure -> $HYDO_FAILED
  $BB awk '{ o[$1] = $2 } END { for (i in o) if (o[i] == "failed") print i }' $HYDO_OUTCOMES 2>/dev/null | $BB sort -n > $HYDO_FAILED
}
provision() {
  if [ -n "$HYDO_PROVISION" ]; then
    RUN "$HYDO_PROVISION" > $H/provision.log 2>&1 || { $BB tail -8 $H/provision.log; die_oracle "HYDO-ORACLE: ERROR provision failed"; }
  fi
  $BB sync   # provisioning is not part of the window under test: make it durable on its own
  log "provisioning synced (uptime=$(UP)s)"
  echo "HYDO-SETUP-COMPLETE"
}

# ---------------------------------------------------------------- faults
net_on() {   # <svc> <match: dport|sport> <netem args with commas>
  local port args m
  port=$(svc_port $1); args=$(echo "$3" | $BB tr ',' ' ')
  [ -n "$port" ] && [ -n "$HYDO_TC" ] || { log "network fault on $1 skipped (no port or no tc)"; return 1; }
  for m in sch_prio sch_netem cls_u32; do $BB insmod /lib/modules/$m.ko 2>/dev/null; done
  $HYDO_TC qdisc add dev lo root handle 1: prio bands 4 priomap 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 && \
  $HYDO_TC qdisc add dev lo parent 1:4 handle 40: netem $args && \
  $HYDO_TC filter add dev lo parent 1:0 protocol ip prio 1 u32 match ip $2 $port 0xffff flowid 1:4 || \
    die_oracle "HYDO-ORACLE: ERROR tc setup failed"
  log "NET FAULT on $1 (ip $2 $port): netem $args"
}
net_off() { $HYDO_TC qdisc del dev lo root 2>/dev/null; }
xassert() { echo "HYDO-ASSERT {\"t\":\"$1\",\"id\":\"$2\",\"ok\":$3,\"msg\":\"$4\"}"; }
pause_timer() {   # <svc> <off-ms> <dur-ms>, in the background
  $BB sleep $($BB awk "BEGIN{printf \"%.3f\", $2/1000}")
  pp=$1
  if $BB pgrep -f "$HYDO_WORKLOAD_PGREP" > /dev/null 2>&1; then mid=true; else mid=false; fi
  svc_sig $pp STOP
  log "PAUSED $1 (timer, mid_request=$mid)"
  xassert reachable fault.paused true "a service was paused (SIGSTOP)"
  [ $mid = true ] && xassert sometimes fault.pause_mid_request true "a pause began while a client request was in flight"
  $BB sleep $($BB awk "BEGIN{printf \"%.3f\", $3/1000}")
  svc_sig $pp CONT
  log "RESUMED $1 (timer)"
}
set_knob() {   # <letter> <value>
  local d; d=$(kv $1 default)
  [ -n "$d" ] || { log "knob $1 not in the manifest, ignored"; return 0; }
  [ "$2" = "$d" ] && return 0
  HYDO_VALUE=$2 RUN "$(kv $1 set)" > /dev/null 2>&1 || die_oracle "HYDO-ORACLE: ERROR knob $1=$2 failed"
  log "knob $1=$2 ($(kv $1 name))"
}

explore_read() { CMD=""; read -r CMD < /dev/console; log "command: $CMD"; }

explore_run() {   # $CMD = "run <tokens>"
  K=""; N=""; C=""; P=""; T=""; O=""
  for t in ${CMD#run}; do
    case "$t" in
      K=*) K="${t#K=}";; N=*) N="${t#N=}";; C=*) C="${t#C=}";;
      P=*) P="${t#P=}";; T=*) T="${t#T=}";; O=*) O="${t#O=}";;
      ?=*) set_knob "${t%%=*}" "${t#*=}";;
    esac
  done
  for d in "reachable fault.paused a service was paused (SIGSTOP)" \
           "sometimes fault.pause_mid_request a pause began while a client request was in flight" \
           "reachable fault.clock_back the realtime clock was stepped backwards" \
           "reachable fault.clock_fwd the realtime clock was stepped forwards" \
           "reachable fault.oneway_req a one-way network fault hit the req direction" \
           "reachable fault.oneway_resp a one-way network fault hit the resp direction"; do
    set -- $d; dt=$1; di=$2; shift 2
    echo "HYDO-ASSERT-DECL {\"t\":\"$dt\",\"id\":\"$di\",\"msg\":\"$*\"}"
  done
  # a token naming a service the manifest does not have is ignored (the explorer's service
  # names are fixed; a project without that service simply does not get that fault)
  ksvc=""; kat=-1; kup=-1; nsvc=""; nfrom=-1; nto=-1; nargs=""; cat_=-1
  psvc=""; pat=-1; pup=-1; poff=""; tat=-1; tdelta=0; osvc=""; ofrom=-1; oto=-1; odir=""; oargs=""
  if [ -n "$K" ]; then ksvc=${K%@*}; r=${K#*@}; kat=${r%+*}; kup=$((kat + ${r#*+})); known_svc $ksvc || { log "K: no service $ksvc, ignored"; kat=-1; kup=-1; }; fi
  if [ -n "$N" ]; then nsvc=${N%%@*}; r=${N#*@}; rng=${r%%:*}; nargs=${r#*:}; nfrom=${rng%-*}; nto=${rng#*-}; known_svc $nsvc || { log "N: no service $nsvc, ignored"; nfrom=-1; nto=-1; }; fi
  [ -n "$C" ] && cat_=$C
  if [ -n "$P" ]; then
    psvc=${P%@*}; r=${P#*@}
    case "$r" in
      *~*) pat=${r%~*}; r=${r#*~}; poff=${r%:*}; pdur=${r#*:};;
      *) pat=${r%+*}; pup=$((pat + ${r#*+}));;
    esac
    known_svc $psvc || { log "P: no service $psvc, ignored"; pat=-1; pup=-1; }
  fi
  if [ -n "$T" ]; then tat=${T%%:*}; tdelta=${T#*:}; fi
  if [ -n "$O" ]; then osvc=${O%%@*}; r=${O#*@}; rng=${r%%:*}; r=${r#*:}; odir=${r%%:*}; oargs=${r#*:}; ofrom=${rng%-*}; oto=${rng#*-}
    known_svc $osvc || { log "O: no service $osvc, ignored"; ofrom=-1; oto=-1; }; fi
  pts=$(for p in 0 $kat $kup $nfrom $nto $cat_ $pat $pup $tat $ofrom $oto $HYDO_STEPS; do [ "$p" -ge 0 ] 2>/dev/null && [ "$p" -le $HYDO_STEPS ] && echo $p; done | $BB sort -n -u)
  : > $HYDO_ACKED; : > $HYDO_OUTCOMES; : > $H/workload.log
  start_periodic
  netup=0; down=0; cut=0; paused=0; ptimer=""; oneup=0; prev=""
  for p in $pts; do
    if [ -n "$prev" ] && [ "$prev" -lt "$p" ]; then step_range $prev $p; fi
    [ "$p" = "$cat_" ] && { cut=1; break; }
    if [ "$p" = "$nto" ] && [ $netup = 1 ]; then net_off; netup=0; log "network healed before step $p"; fi
    if [ "$p" = "$kup" ] && [ $down = 1 ]; then log "restarting $ksvc before step $p"; start_svc $ksvc || die_oracle "HYDO-ORACLE: ERROR $ksvc restart failed"; down=0; fi
    if [ "$p" = "$kat" ]; then svc_sig $ksvc 9; down=1; log "KILLED $ksvc before step $p"; fi
    if [ "$p" = "$nfrom" ] && [ "$nfrom" -lt "$nto" ]; then net_on $nsvc dport "$nargs" && netup=1; fi
    if [ "$p" = "$oto" ] && [ $oneup = 1 ]; then net_off; oneup=0; log "one-way fault healed before step $p"; fi
    if [ "$p" = "$ofrom" ] && [ "$ofrom" -lt "$oto" ]; then
      case "$odir" in req) om=dport;; resp) om=sport;; *) die_oracle "HYDO-ORACLE: ERROR bad one-way direction '$odir'";; esac
      net_on $osvc $om "$oargs" && { oneup=1; xassert reachable fault.oneway_$odir true "a one-way network fault hit the $odir direction"; }
    fi
    if [ "$p" = "$pup" ] && [ $paused = 1 ]; then svc_sig $psvc CONT; paused=0; log "RESUMED $psvc before step $p"; fi
    if [ "$p" = "$pat" ]; then
      if [ -n "$poff" ]; then
        pause_timer $psvc $poff $pdur & ptimer=$!
        log "pause timer for $psvc armed before step $p (after ${poff}ms, for ${pdur}ms)"
      else
        svc_sig $psvc STOP; paused=1; log "PAUSED $psvc before step $p"
        xassert reachable fault.paused true "a service was paused (SIGSTOP)"
      fi
    fi
    if [ "$p" = "$tat" ]; then
      tnow=$($BB date +%s)
      $BB date -s @$((tnow + tdelta)) > /dev/null || log "date -s failed"
      tnew=$($BB date +%s)
      log "CLOCK JUMP of ${tdelta}s before step $p (realtime $tnow -> $tnew)"
      tdiff=$((tnew - tnow - tdelta))
      if [ $tdiff -gt 5 ] || [ $tdiff -lt -5 ]; then die_oracle "HYDO-ORACLE: ERROR clock jump not applied ($tnow -> $tnew, wanted $tdelta)"; fi
      if [ "$tdelta" -lt 0 ]; then xassert reachable fault.clock_back true "the realtime clock was stepped backwards"
      else xassert reachable fault.clock_fwd true "the realtime clock was stepped forwards"; fi
    fi
    prev=$p
  done
  if [ $cut = 1 ]; then
    [ -n "$ptimer" ] && $BB kill $ptimer 2>/dev/null   # no SIGCONT may follow the freeze
    stop_periodic
    failed_ids
    echo "hydo-explore-acked: $($BB tr '\n' ' ' < $HYDO_ACKED)"
    echo "hydo-explore-failed: $($BB tr '\n' ' ' < $HYDO_FAILED)"
    # NO sync: it would FLUSH the volatile write cache and close the window under test.
    for s in $HYDO_SERVICES; do svc_sig $s STOP; done
    log "POWER CUT before step $cat_ (all services frozen)"
    echo "hydo-explore-workload-done cut"
    while true; do $BB sleep 100; done
  fi
  # no cut: faults are transient -- heal, bring everything back, let the workload retry
  [ $netup = 1 ] && { net_off; log "network healed at the end"; }
  [ $oneup = 1 ] && { net_off; log "one-way fault healed at the end"; }
  [ -n "$ptimer" ] && { wait $ptimer 2>/dev/null; svc_sig $psvc CONT; }
  [ $paused = 1 ] && { svc_sig $psvc CONT; log "RESUMED $psvc at the end"; }
  restart_dead "HYDO-ORACLE: ERROR restart after the faults"
  retry_workload
  stop_periodic
  failed_ids
  echo "hydo-explore-acked: $($BB tr '\n' ' ' < $HYDO_ACKED)"
  echo "hydo-explore-failed: $($BB tr '\n' ' ' < $HYDO_FAILED)"
  echo "hydo-explore-workload-done"
  explore_read     # "go": the host has switched disk faults off
  restart_dead "HYDO-ORACLE: VIOLATION service cannot recover after the faults"
  finish
}

# ---------------------------------------------------------------- main
declare_all
case "$PHASE" in
  serve)
    echo "hydo-explore-ready-S0"
    explore_read
    # The guest has no RTC: start every boot at one fixed instant (so a backward clock step
    # works, and every run sees the same dates).
    $BB date -s @$HYDO_CLOCK_START > /dev/null && log "realtime clock set to $($BB date -u +%Y-%m-%dT%H:%M:%SZ)"
    case "$CMD" in
      provision)
        mount_data "HYDO-ORACLE: ERROR mount data"
        start_all "HYDO-ORACLE: ERROR service start"
        provision
        echo "hydo-explore-ready-S1"
        explore_read
        case "$CMD" in run*) explore_run;; *) die_oracle "HYDO-ORACLE: ERROR unknown S1 command '$CMD'";; esac
        ;;
      oracle*)
        dst=$HYDO_ACKED
        for id in ${CMD#oracle}; do
          if [ "$id" = failed ]; then dst=$HYDO_FAILED; else echo "$id" >> $dst; fi
        done
        mount_data "HYDO-ORACLE: VIOLATION data disk unmountable"
        echo "hydo-stack: recovering from log"
        start_all "HYDO-ORACLE: VIOLATION service cannot recover (committed data unrecoverable)" recovery
        finish
        ;;
      *) die_oracle "HYDO-ORACLE: ERROR unknown S0 command '$CMD'";;
    esac
    ;;
  *)
    $BB date -s @$HYDO_CLOCK_START > /dev/null
    mount_data "HYDO-ORACLE: ERROR mount data"
    start_all "HYDO-ORACLE: ERROR service start"
    provision
    start_periodic
    step_range 0 $HYDO_STEPS
    retry_workload
    stop_periodic
    failed_ids
    finish
    ;;
esac
