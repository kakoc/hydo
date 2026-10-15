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

explore_run() {   # $CMD = "run <tokens>"
  K=""; N=""; C=""; M=1; R=always
  for t in ${CMD#run}; do
    case "$t" in
      K=*) K="${t#K=}";; N=*) N="${t#N=}";; C=*) C="${t#C=}";;
      M=*) M="${t#M=}";; R=*) R="${t#R=}";;
    esac
  done
  if [ "$M" != 1 ]; then SQL -e "SET GLOBAL innodb_flush_log_at_trx_commit=$M"; echo "hydo-explore: mysql innodb_flush_log_at_trx_commit=$M"; fi
  if [ "$R" != always ]; then /bin/redis-cli -p 6379 CONFIG SET appendfsync "$R" > /dev/null; echo "hydo-explore: redis appendfsync=$R"; fi
  ksvc=""; kat=-1; kup=-1; nsvc=""; nfrom=-1; nto=-1; nargs=""; cat_=-1
  if [ -n "$K" ]; then ksvc=${K%@*}; r=${K#*@}; kat=${r%+*}; kup=$((kat + ${r#*+})); fi
  if [ -n "$N" ]; then nsvc=${N%%@*}; r=${N#*@}; rng=${r%%:*}; nargs=${r#*:}; nfrom=${rng%-*}; nto=${rng#*-}; fi
  [ -n "$C" ] && cat_=$C
  # order-stream boundaries where something happens, in order
  pts=$(for p in 0 $kat $kup $nfrom $nto $cat_ $NORDERS; do [ "$p" -ge 0 ] 2>/dev/null && [ "$p" -le $NORDERS ] && echo $p; done | $BB sort -n -u)
  : > /tmp/acked; : > /tmp/outcomes; : > /tmp/client.log
  netup=0; down=0; cut=0
  prev=""
  for p in $pts; do
    if [ -n "$prev" ] && [ "$prev" -lt "$p" ]; then client $prev $p; fi
    [ "$p" = "$cat_" ] && { cut=1; break; }
    if [ "$p" = "$nto" ] && [ $netup = 1 ]; then $TC qdisc del dev lo root 2>/dev/null; netup=0; echo "hydo-explore: network healed before order $p"; fi
    if [ "$p" = "$kup" ] && [ $down = 1 ]; then echo "hydo-explore: restarting $ksvc before order $p"; restart_svc $ksvc; down=0; fi
    if [ "$p" = "$kat" ]; then $BB kill -9 $(svc_pid $ksvc) 2>/dev/null; down=1; echo "hydo-explore: KILLED $ksvc before order $p"; fi
    if [ "$p" = "$nfrom" ] && [ "$nfrom" -lt "$nto" ]; then net_fault_on "$nsvc:$nargs"; netup=1; fi
    prev=$p
  done
  if [ $cut = 1 ]; then
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
  [ $down = 1 ] && { echo "hydo-explore: restarting $ksvc at the end"; restart_svc $ksvc; }
  $BB kill -0 $FPID 2>/dev/null || { echo "hydo-explore: front door died; restarting it"; start_front || die_oracle "HYDO-ORACLE: ERROR front door restart"; }
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
