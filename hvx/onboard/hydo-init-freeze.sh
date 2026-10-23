# ---------------------------------------------------------------- freeze at a call (hydo-init-freeze.sh)
# The F= fault (hvx/onboard/MANIFEST.md, "Freeze at a call"). hydo-build inserts this file, plus
# renamed copies _fc_orig_<fn> of the four init functions wrapped at the end, into /sbin/init
# just before "main" -- ONLY for a manifest whose faults: list names `freeze`. Every other image
# keeps its init byte for byte. Without an F= token in the run command nothing here acts: no
# netfilter rule, no helper.
#
#   F=<svc>><target>@<step>[#<n>][:<match>]:<dur>   freeze <svc> at the n-th packet it SENDS to <target>
#   F=<svc><<target>@<step>[#<n>][:<match>]:<dur>   freeze <svc> at the n-th packet <target> sends to it
#
# From workload step <step> on (counting starts when that step begins), every TCP packet between
# the two services goes through netfilter queue 7 to the helper /bin/hydo_fc (hydo_fc.c). It
# counts the packets that carry payload in the chosen direction and contain <match> (any bytes,
# %xx escapes; default: every packet), and at the n-th (default 1) it holds that packet, stops
# <svc>'s whole process tree (SIGSTOP, then waits until every thread is stopped), lets the packet
# through, and continues the tree after <dur> ms of guest time. So <svc> is frozen exactly at
# that point of its conversation: it has not run one instruction past the event.
# Which packets belong to the pair: both services have addr: -> source/destination addresses;
# only <target> has a port: -> that port (and <svc>'s addr: when it has one).
# n = 0 never fires: the helper only writes a trace of the pair's traffic, printed at the end
# ("hydo-fc-trace:" lines), to find the n of an event.

FC=""; fc_on=0; fc_pid=""
fc_known() { local s; for s in $HYDO_SERVICES; do [ "$s" = "$1" ] && return 0; done; return 1; }
fc_bad() { die_oracle "HYDO-ORACLE: ERROR bad freeze token F=$FC ($1)"; }
fc_parse() {   # <value of F=>
  local head rest
  FC=$1
  head=${FC%%@*}; rest=${FC#*@}
  [ "$head" != "$FC" ] || fc_bad "no @"
  case "$head" in
    *">"*) fc_dir=req; fc_svc=${head%%>*}; fc_tgt=${head#*>};;
    *"<"*) fc_dir=resp; fc_svc=${head%%<*}; fc_tgt=${head#*<};;
    *) fc_bad "no > or <";;
  esac
  fc_dur=${rest##*:}; rest=${rest%:*}
  [ "$rest" != "${FC#*@}" ] || fc_bad "no :<dur>"
  case "$rest" in *:*) fc_match=${rest#*:}; rest=${rest%%:*};; *) fc_match=-;; esac
  [ -n "$fc_match" ] || fc_match=-
  case "$rest" in *"#"*) fc_step=${rest%%#*}; fc_n=${rest#*#};; *) fc_step=$rest; fc_n=1;; esac
  for v in "$fc_step" "$fc_n" "$fc_dur"; do
    case "$v" in ""|*[!0-9]*) fc_bad "step, n and dur must be numbers";; esac
  done
  if ! fc_known "$fc_svc" || ! fc_known "$fc_tgt" || [ "$fc_svc" = "$fc_tgt" ]; then
    log "F: no services $fc_svc and $fc_tgt, ignored"; FC=""; return 0
  fi
  log "freeze at a call: $fc_svc ($fc_dir with $fc_tgt) from step $fc_step, event #$fc_n, match '$fc_match', for ${fc_dur}ms"
}

fc_rules() {   # <-A|-D>: the netfilter rules that queue the pair's packets
  local sa ta tp
  sa=$(sv $fc_svc addr); ta=$(sv $fc_tgt addr); tp=$(sv $fc_tgt port)
  if [ -n "$sa" ] && [ -n "$ta" ]; then
    $HYDO_IPT $1 OUTPUT -s $sa -d $ta -j NFQUEUE --queue-num 7 --queue-bypass &&
    $HYDO_IPT $1 OUTPUT -s $ta -d $sa -j NFQUEUE --queue-num 7 --queue-bypass
  elif [ -n "$tp" ]; then
    local s1="" s2=""
    [ -n "$sa" ] && { s1="-s $sa"; s2="-d $sa"; }
    $HYDO_IPT $1 OUTPUT $s1 -p tcp --dport $tp -j NFQUEUE --queue-num 7 --queue-bypass &&
    $HYDO_IPT $1 OUTPUT $s2 -p tcp --sport $tp -j NFQUEUE --queue-num 7 --queue-bypass
  else
    return 3
  fi
}

fc_arm() {   # before step $fc_step
  local sa tp m n=0 rc
  sa=$(sv $fc_svc addr); tp=$(sv $fc_tgt port)
  fc_on=1
  export XTABLES_LOCKFILE=$H/xtables.lock
  for m in x_tables ip_tables iptable_filter xt_tcpudp xt_NFQUEUE nfnetlink nfnetlink_queue; do
    $BB insmod /lib/modules/$m.ko 2>/dev/null
  done
  $BB rm -f $H/fc.bound $H/fc.fired $H/fc.resumed
  $HYDO_FC 7 $H/pid.$fc_svc "${sa:--}" "${tp:--}" $fc_dir $fc_n "$fc_match" $fc_dur $H/fc.log $H &
  fc_pid=$!
  while [ ! -f $H/fc.bound ] && [ $n -lt 200 ]; do $BB sleep 0.05; n=$((n + 1)); done
  [ -f $H/fc.bound ] || die_oracle "HYDO-ORACLE: ERROR freeze helper did not start"
  fc_rules -A > $H/fc.ipt 2>&1; rc=$?
  [ $rc = 3 ] && { log "freeze: $fc_svc and $fc_tgt have neither addresses nor a port, skipped"; fc_stop_helper; return 0; }
  [ $rc = 0 ] || { $BB cat $H/fc.ipt; die_oracle "HYDO-ORACLE: ERROR freeze rules failed"; }
  log "FREEZE ARMED before step $fc_step: $fc_svc at $fc_dir #$fc_n with $fc_tgt"
}

fc_stop_helper() { [ -n "$fc_pid" ] && { $BB kill $fc_pid 2>/dev/null; wait $fc_pid 2>/dev/null; }; fc_pid=""; }

fc_heal() {   # end of fault injection (no cut): let a running freeze end, remove the rules
  local n=0 lim
  [ $fc_on = 1 ] || return 0
  fc_on=2
  if [ -f $H/fc.fired ] && [ ! -f $H/fc.resumed ]; then
    lim=$((fc_dur / 1000 + 30))
    while [ ! -f $H/fc.resumed ] && [ $n -lt $lim ]; do $BB sleep 1; n=$((n + 1)); done
  fi
  fc_rules -D > /dev/null 2>&1
  [ -n "$fc_pid" ] && ! $BB kill -0 $fc_pid 2>/dev/null && log "freeze helper had exited early"
  fc_stop_helper
  if [ -f $H/fc.fired ] && [ ! -f $H/fc.resumed ]; then svc_sig $fc_svc CONT; log "freeze of $fc_svc ended by the init"; fi
  if [ -f $H/fc.fired ]; then log "freeze healed"
  else
    log "freeze never fired: $($BB grep -c ' #' $H/fc.log 2>/dev/null) of $fc_n matching events after step $fc_step"
    # n = 0: the trace, compact (the console holds 64 KiB): packet, length, [#count,] uptime ms,
    # and the first two words (4+ of A-Z a-z 0-9 _ : -, not "def") of the payload
    [ "$fc_n" = 0 ] && $BB awk -v max=${HYDO_FC_TRACE_MAX:-600} 'NR <= max {
        h = ($4 ~ /^#/); w = ""; c = 0
        for (i = (h ? 6 : 5); i <= NF && c < 2; i++) { s = $i; gsub(/[^A-Za-z0-9_:-]/, " ", s); n = split(s, a, " ")
          for (j = 1; j <= n && c < 2; j++) if (length(a[j]) >= 4 && a[j] != "def") { w = w " " substr(a[j], 1, 32); c++ } }
        print "hydo-fc-trace: " $1 " " $3 (h ? " " $4 : "") " " $(h ? 5 : 4) w }
      END { if (NR > max) print "hydo-fc-trace: ... " NR - max " more packets not shown" }' $H/fc.log
  fi
}

fc_kill() {   # a power cut: the helper must not continue the service any more
  [ -n "$fc_pid" ] && { $BB kill -9 $fc_pid 2>/dev/null; wait $fc_pid 2>/dev/null; fc_pid=""; }
}

# the wrapped init functions (originals: _fc_orig_<fn>, copied by hydo-build)
set_knob() { if [ "$1" = F ]; then fc_parse "$2"; else _fc_orig_set_knob "$@"; fi; }
step_range() {
  [ -f $H/fc.decl ] || { : > $H/fc.decl
    echo "HYDO-ASSERT-DECL {\"t\":\"reachable\",\"id\":\"fault.freeze_call\",\"msg\":\"a service was frozen at a call (F=)\"}"; }
  if [ -n "$FC" ] && [ $fc_on = 0 ] && [ "$fc_step" -ge "$1" ] && [ "$fc_step" -lt "$2" ]; then
    _fc_orig_step_range "$1" "$fc_step"; fc_arm; _fc_orig_step_range "$fc_step" "$2"
  else
    _fc_orig_step_range "$@"
  fi
}
restart_dead() { fc_heal; _fc_orig_restart_dead "$@"; }
stop_periodic() { fc_kill; _fc_orig_stop_periodic "$@"; }
