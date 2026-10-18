# /etc/stack-explore.sh -- the explorer's command-driven mode for the four-service stack
# guest (Milestone 38). Sourced by /sbin/init when the kernel cmdline says hydo_phase=serve
# (build-stack.sh installs it only into the STACK_OUT=explore image).
#
# The guest stops twice and waits for ONE command line on the serial console; the host
# (hvx/hvx_explore.c) snapshots the VM at each stop, so every fault schedule starts from a
# snapshot instead of a fresh boot:
#
#   S0  "hydo-explore-ready-S0"  root mounted, data disk NOT mounted, nothing started.
#       command  provision          -> mount, start the stores + front door, sync, go to S1
#       command  oracle <id> ... [failed <id> ...]
#                                   -> recovery boot after a power cut: mount the durable
#                                      data disk, start the stores (crash recovery), check
#                                      the ACKed ids listed; the ids after "failed" are the
#                                      orders the client was told definitely failed (M39)
#   S1  "hydo-explore-ready-S1"  everything provisioned and synced, no order placed.
#       command  run <tokens>       -> place orders 0..NORDERS-1 under the schedule:
#           K=<svc>@<at>+<down>    SIGKILL <svc> before order <at>, restart it before
#                                  order <at>+<down>
#           N=<svc>@<from>-<to>:<netem args, commas for spaces>
#                                  netem on traffic to <svc>'s port for orders [from,to)
#           C=<at>                 power cut before order <at>: freeze every server, print
#                                  the ACKed ids, stop (the host keeps only flushed disk
#                                  bytes and runs the recovery from S0)
#           M=<0|1|2>              MySQL innodb_flush_log_at_trx_commit (1 = durable)
#           R=<always|everysec|no> Redis appendfsync (always = durable)
#         New fault kinds (M46; absent token = nothing happens):
#           P=<svc>@<at>+<n>       SIGSTOP <svc> (mysql|kafka|redis|front) before order <at>,
#                                  SIGCONT before order <at>+<n>  (pause between requests)
#           P=<svc>@<at>~<off>:<dur>
#                                  before order <at> start a timer: after <off> ms of guest
#                                  time SIGSTOP <svc>, after <dur> ms more SIGCONT; the orders
#                                  go on meanwhile, so the pause can land mid-request
#           T=<at>:<delta>         before order <at> step the guest's REALTIME clock by
#                                  <delta> seconds (signed; settimeofday via date -s). The
#                                  monotonic clock is untouched (an NTP-style step).
#           O=<svc>@<from>-<to>:<req|resp>:<netem args, commas for spaces>
#                                  ONE-WAY netem for orders [from,to): req = packets to
#                                  <svc>'s port only, resp = packets from it only
#       Without a cut the guest then prints "hydo-explore-workload-done" and waits for "go"
#       (the host first switches disk faults off), restarts any store that died, and runs
#       the oracle in the same boot.
#
# Every verdict is a HYDO-ORACLE line, followed by "hydo-stack-marker: workload complete".

explore_read() {   # read one command line from the serial console into CMD
  CMD=""
  read -r CMD < /dev/console
  echo "hydo-explore: command: $CMD"
}

explore_s0() {
  echo "hydo-explore-ready-S0"
  explore_read
  # M46: the guest has no RTC, so its realtime clock starts at 1970 + uptime and a backward
  # clock jump (T=at:-delta) would be refused. Start every boot (provision and recovery) at
  # a fixed date before any service runs. Deterministic: same instant in every run.
  $BB date -s @1767225600 > /dev/null && echo "hydo-explore: realtime clock set to $($BB date -u +%Y-%m-%dT%H:%M:%SZ)"
  case "$CMD" in
    provision) EXPLORE=prov;;
    oracle*)
      EXPLORE=oracle; PHASE=oracle
      : > /tmp/acked; : > /tmp/failed
      dst=/tmp/acked
      for id in ${CMD#oracle}; do
        if [ "$id" = failed ]; then dst=/tmp/failed; else echo "$id" >> $dst; fi
      done
      ;;
    *) echo "HYDO-ORACLE: ERROR explore: unknown S0 command '$CMD'"; EXPLORE=bad;;
  esac
}

svc_alive() { pid=$(svc_pid "$1"); [ -n "$pid" ] && $BB kill -0 "$pid" 2>/dev/null; }

explore_oracle() {   # $1 = HYDO-ORACLE prefix for a store that will not come back
  for s in redis mysql kafka; do
    if ! svc_alive $s; then
      echo "hydo-explore: $s is down; restarting it (crash recovery)"
      case "$s" in
        mysql) start_mysql;; kafka) start_kafka;; redis) start_redis;;
      esac || die_oracle "$1 ($s)"
    fi
  done
  /bin/hydostack oracle $NORDERS /tmp/acked /tmp/failed > /tmp/oracle.log 2>&1
  $BB grep -a "HYDO-ORACLE" /tmp/oracle.log || { echo "HYDO-ORACLE: ERROR oracle produced no verdict"; $BB tail -6 /tmp/oracle.log; }
  echo "hydo-stack-marker: workload complete"
  echo "hydo-boot-marker: launching shell on ttyS0"
  exec $BB sh
}

xassert() {   # <type> <id> <ok> <msg>: one HYDO-ASSERT record (hvx/ASSERTIONS.md)
  echo "HYDO-ASSERT {\"t\":\"$1\",\"id\":\"$2\",\"ok\":$3,\"msg\":\"$4\"}"
}

oneway_on() {   # $1 = <svc>:<req|resp>:<netem args with commas>
  osv=${1%%:*}; r=${1#*:}; odir=${r%%:*}; oargs=$(echo "${r#*:}" | $BB tr ',' ' '); oport=$(svc_port $osv)
  case "$odir" in req) omatch=dport;; resp) omatch=sport;; *) die_oracle "HYDO-ORACLE: ERROR explore: bad one-way direction '$odir'";; esac
  for m in sch_prio sch_netem cls_u32; do $BB insmod /lib/modules/$m.ko 2>/dev/null; done
  $TC qdisc add dev lo root handle 1: prio bands 4 priomap 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 && \
  $TC qdisc add dev lo parent 1:4 handle 40: netem $oargs && \
  $TC filter add dev lo parent 1:0 protocol ip prio 1 u32 match ip $omatch $oport 0xffff flowid 1:4 || \
    die_oracle "HYDO-ORACLE: ERROR tc setup failed"
  echo "hydo-explore: ONE-WAY NET FAULT on $odir packets of $osv (ip $omatch $oport): netem $oargs"
  xassert reachable fault.oneway_$odir true "a one-way network fault hit the $odir direction"
}

pause_pid() { case "$1" in front) echo $FPID;; *) svc_pid "$1";; esac; }

kill_timer() {   # $1 svc $2 off-ms (background): SIGKILL while orders run, e.g. mid-request
  $BB sleep $($BB awk "BEGIN{printf \"%.3f\", $2/1000}")
  if $BB pgrep -f "hydostack (place|retry)" > /dev/null 2>&1; then mid=true; else mid=false; fi
  $BB kill -9 $(pause_pid $1) 2>/dev/null
  echo "hydo-explore: KILLED $1 (timer, mid_request=$mid)"
  xassert reachable fault.killed_timer true "a service was killed by a timer while orders ran"
}

pause_timer() {   # $1 svc $2 off-ms $3 dur-ms (runs in the background)
  $BB sleep $($BB awk "BEGIN{printf \"%.3f\", $2/1000}")
  pp=$(pause_pid $1)
  if $BB pgrep -f "hydostack (place|retry)" > /dev/null 2>&1; then mid=true; else mid=false; fi
  $BB kill -STOP $pp 2>/dev/null
  echo "hydo-explore: PAUSED $1 (timer, mid_request=$mid)"
  xassert reachable fault.paused true "a service was paused (SIGSTOP)"
  [ $mid = true ] && xassert sometimes fault.pause_mid_request true "a pause began while a client request was in flight"
  $BB sleep $($BB awk "BEGIN{printf \"%.3f\", $3/1000}")
  $BB kill -CONT $pp 2>/dev/null
  echo "hydo-explore: RESUMED $1 (timer)"
}

explore_run() {   # $CMD = "run <tokens>"
  K=""; N=""; C=""; M=1; R=always; P=""; T=""; O=""; Q=""; X=""
  for t in ${CMD#run}; do
    case "$t" in
      K=*) K="${t#K=}";; N=*) N="${t#N=}";; C=*) C="${t#C=}";;
      M=*) M="${t#M=}";; R=*) R="${t#R=}";;
      P=*) P="${t#P=}";; T=*) T="${t#T=}";; O=*) O="${t#O=}";; Q=*) Q="${t#Q=}";; X=*) X="${t#X=}";;
    esac
  done
  for d in "reachable fault.paused a service was paused (SIGSTOP)" \
           "sometimes fault.pause_mid_request a pause began while a client request was in flight" \
           "reachable fault.clock_back the realtime clock was stepped backwards" \
           "reachable fault.clock_fwd the realtime clock was stepped forwards" \
           "reachable fault.oneway_req a one-way network fault hit the req direction" \
           "reachable fault.oneway_resp a one-way network fault hit the resp direction" \
           "reachable fault.killed_timer a service was killed by a timer while orders ran"; do
    set -- $d; dt=$1; di=$2; shift 2
    echo "HYDO-ASSERT-DECL {\"t\":\"$dt\",\"id\":\"$di\",\"msg\":\"$*\"}"
  done
  if [ -n "$Q" ]; then   # Q=<n>[d]: n concurrent requests; d = every order sent twice at once
    export HYDO_CONC=${Q%d}
    case "$Q" in *d) export HYDO_DUP=1;; esac
    echo "hydo-explore: client sends $HYDO_CONC requests at a time${HYDO_DUP:+, every order twice}"
  fi
  if [ "$M" != 1 ]; then SQL -e "SET GLOBAL innodb_flush_log_at_trx_commit=$M"; echo "hydo-explore: mysql innodb_flush_log_at_trx_commit=$M"; fi
  if [ "$R" != always ]; then /bin/redis-cli -p 6379 CONFIG SET appendfsync "$R" > /dev/null; echo "hydo-explore: redis appendfsync=$R"; fi
  ksvc=""; kat=-1; kup=-1; nsvc=""; nfrom=-1; nto=-1; nargs=""; cat_=-1
  if [ -n "$K" ]; then ksvc=${K%@*}; r=${K#*@}; kat=${r%+*}; kup=$((kat + ${r#*+})); fi
  if [ -n "$N" ]; then nsvc=${N%%@*}; r=${N#*@}; rng=${r%%:*}; nargs=${r#*:}; nfrom=${rng%-*}; nto=${rng#*-}; fi
  [ -n "$C" ] && cat_=$C
  psvc=""; pat=-1; pup=-1; poff=""; pdur=""; tat=-1; tdelta=0; osvc=""; ofrom=-1; oto=-1; ospec=""
  if [ -n "$P" ]; then
    psvc=${P%@*}; r=${P#*@}
    case "$r" in
      *~*) pat=${r%~*}; r=${r#*~}; poff=${r%:*}; pdur=${r#*:};;
      *) pat=${r%+*}; pup=$((pat + ${r#*+}));;
    esac
  fi
  if [ -n "$T" ]; then tat=${T%%:*}; tdelta=${T#*:}; fi
  xsvc=""; xat=-1; xoff=""; xtimer=""
  if [ -n "$X" ]; then xsvc=${X%@*}; r=${X#*@}; xat=${r%~*}; xoff=${r#*~}; fi
  if [ -n "$O" ]; then osvc=${O%%@*}; r=${O#*@}; rng=${r%%:*}; ospec="$osvc:${r#*:}"; ofrom=${rng%-*}; oto=${rng#*-}; fi
  # order-stream boundaries where something happens, in order
  pts=$(for p in 0 $kat $kup $nfrom $nto $cat_ $pat $pup $tat $ofrom $oto $xat $NORDERS; do [ "$p" -ge 0 ] 2>/dev/null && [ "$p" -le $NORDERS ] && echo $p; done | $BB sort -n -u)
  : > /tmp/acked; : > /tmp/outcomes; : > /tmp/client.log
  netup=0; down=0; cut=0; paused=0; ptimer=""; oneup=0
  prev=""
  for p in $pts; do
    if [ -n "$prev" ] && [ "$prev" -lt "$p" ]; then client $prev $p; fi
    [ "$p" = "$cat_" ] && { cut=1; break; }
    if [ "$p" = "$nto" ] && [ $netup = 1 ]; then $TC qdisc del dev lo root 2>/dev/null; netup=0; echo "hydo-explore: network healed before order $p"; fi
    if [ "$p" = "$kup" ] && [ $down = 1 ]; then echo "hydo-explore: restarting $ksvc before order $p"; restart_svc $ksvc; down=0; fi
    if [ "$p" = "$xat" ]; then
      kill_timer $xsvc $xoff & xtimer=$!
      [ "$xsvc" != front ] && { ksvc=$xsvc; down=1; }   # brought back at the end
      echo "hydo-explore: kill timer for $xsvc armed before order $p (after ${xoff}ms)"
    fi
    if [ "$p" = "$kat" ]; then $BB kill -9 $(svc_pid $ksvc) 2>/dev/null; down=1; echo "hydo-explore: KILLED $ksvc before order $p"; fi
    if [ "$p" = "$nfrom" ] && [ "$nfrom" -lt "$nto" ]; then net_fault_on "$nsvc:$nargs"; netup=1; fi
    if [ "$p" = "$oto" ] && [ $oneup = 1 ]; then $TC qdisc del dev lo root 2>/dev/null; oneup=0; echo "hydo-explore: one-way fault healed before order $p"; fi
    if [ "$p" = "$ofrom" ] && [ "$ofrom" -lt "$oto" ]; then oneway_on "$ospec"; oneup=1; fi
    if [ "$p" = "$pup" ] && [ $paused = 1 ]; then $BB kill -CONT $(pause_pid $psvc) 2>/dev/null; paused=0; echo "hydo-explore: RESUMED $psvc before order $p"; fi
    if [ "$p" = "$pat" ]; then
      if [ -n "$poff" ]; then
        pause_timer $psvc $poff $pdur & ptimer=$!
        echo "hydo-explore: pause timer for $psvc armed before order $p (after ${poff}ms, for ${pdur}ms)"
      else
        $BB kill -STOP $(pause_pid $psvc) 2>/dev/null; paused=1; echo "hydo-explore: PAUSED $psvc before order $p"
        xassert reachable fault.paused true "a service was paused (SIGSTOP)"
      fi
    fi
    if [ "$p" = "$tat" ]; then
      tnow=$($BB date +%s)
      $BB date -s @$((tnow + tdelta)) > /dev/null || echo "hydo-explore: date -s failed"
      tnew=$($BB date +%s)
      echo "hydo-explore: CLOCK JUMP of ${tdelta}s before order $p (realtime $tnow -> $tnew)"
      tdiff=$((tnew - tnow - tdelta))
      if [ $tdiff -gt 5 ] || [ $tdiff -lt -5 ]; then
        die_oracle "HYDO-ORACLE: ERROR explore: clock jump not applied ($tnow -> $tnew, wanted $tdelta)"
      fi
      if [ "$tdelta" -lt 0 ]; then xassert reachable fault.clock_back true "the realtime clock was stepped backwards"
      else xassert reachable fault.clock_fwd true "the realtime clock was stepped forwards"; fi
    fi
    prev=$p
  done
  if [ $cut = 1 ]; then
    [ -n "$ptimer" ] && $BB kill $ptimer 2>/dev/null   # no SIGCONT may follow the freeze
    [ -n "$xtimer" ] && $BB kill $xtimer 2>/dev/null
    failed_ids
    echo "hydo-explore-acked: $($BB tr '\n' ' ' < /tmp/acked)"
    echo "hydo-explore-failed: $($BB tr '\n' ' ' < /tmp/failed)"
    $BB grep -a "HYDO-STACK" /tmp/client.log | $BB tail -3
    # NO sync: it would FLUSH the volatile write cache and close the window under test.
    $BB kill -STOP $FPID $KPID $MPID $RPID 2>/dev/null
    echo "hydo-explore: POWER CUT before order $cat_ (all servers frozen)"
    echo "hydo-explore-workload-done cut"
    while true; do $BB sleep 100; done
  fi
  # no cut: faults are transient -- heal, bring everything back, let the client retry
  [ $netup = 1 ] && { $TC qdisc del dev lo root 2>/dev/null; echo "hydo-explore: network healed at the end"; }
  [ $oneup = 1 ] && { $TC qdisc del dev lo root 2>/dev/null; echo "hydo-explore: one-way fault healed at the end"; }
  [ -n "$ptimer" ] && { wait $ptimer 2>/dev/null; $BB kill -CONT $(pause_pid $psvc) 2>/dev/null; }
  [ $paused = 1 ] && { $BB kill -CONT $(pause_pid $psvc) 2>/dev/null; echo "hydo-explore: RESUMED $psvc at the end"; }
  [ -n "$xtimer" ] && wait $xtimer 2>/dev/null
  [ $down = 1 ] && { echo "hydo-explore: restarting $ksvc at the end"; restart_svc $ksvc; }
  $BB kill -0 $FPID 2>/dev/null || { echo "hydo-explore: front door died; restarting it"
    $BB grep -a -m 3 "^fatal error\|^panic" /tmp/front.log | $BB sed "s/^/hydo-explore: front door log: /"
    start_front || die_oracle "HYDO-ORACLE: ERROR front door restart"; }
  retry_unacked
  failed_ids
  $BB grep -a "HYDO-STACK" /tmp/client.log | $BB tail -4
  echo "hydo-explore-acked: $($BB tr '\n' ' ' < /tmp/acked)"
  echo "hydo-explore-failed: $($BB tr '\n' ' ' < /tmp/failed)"
  echo "hydo-explore-workload-done"
  explore_read     # "go": the host has switched disk faults off
  explore_oracle "HYDO-ORACLE: VIOLATION store cannot recover after the faults"
}

explore_main() {
  case "$EXPLORE" in
    oracle)
      echo "hydo-stack: recovering from log"
      start_all "HYDO-ORACLE: VIOLATION store cannot recover (committed data unrecoverable)"
      explore_oracle "HYDO-ORACLE: VIOLATION store cannot recover (committed data unrecoverable)"
      ;;
    prov)
      start_all "HYDO-ORACLE: ERROR server start"
      start_front || die_oracle "HYDO-ORACLE: ERROR front door"
      $BB sync
      echo "HYDO-SETUP-COMPLETE"   # hvx/ASSERTIONS.md lifecycle: faults begin after this
      echo "hydo-explore-ready-S1"
      explore_read
      case "$CMD" in
        run*) explore_run;;
        *) die_oracle "HYDO-ORACLE: ERROR explore: unknown S1 command '$CMD'";;
      esac
      ;;
    *) die_oracle "HYDO-ORACLE: ERROR explore: bad state";;
  esac
}
