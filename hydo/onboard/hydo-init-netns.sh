# ---------------------------------------------------------------- service networks (hydo-init-netns.sh)
# network: bridge (hvx/onboard/MANIFEST.md, "Service networks"). hydo-build inserts this file, plus
# renamed copies _ns_orig_<fn> of the init functions it wraps (start_svc; fc_arm with faults:
# freeze), into /sbin/init just before "main", after every other extension -- ONLY for a manifest
# with network: bridge. Every other image keeps its init byte for byte.
#
# Like Docker's bridge network. Every service runs in its own network namespace, /tmp/hydo/ns/<svc>,
# with
#   lo    127.0.0.1 and ::1, private to the service (localhost inside a container), and
#   eth0  its addr: (an address in 10.89.0.0/16) with the MAC 02:42:<addr in hex>, one end of a
#         veth pair whose other end, vh<i> (i = the service's place in HYDO_SERVICES), is a port of
#         the bridge hydo0 in the guest's own namespace.
# hydo0 has $HYDO_NET_GW (10.89.0.1/16): the address of the guest's own namespace, where the
# workload, provisioning, the invariants and the fake outside world (hydo-world, also on the
# addresses in $HYDO_NET_EXTRA) run. IPv6 is off on hydo0, the veths and eth0 (no router
# solicitations, duplicate-address checks or multicast reports on the wire); lo keeps ::1.
# The network is set up when /sbin/init reaches this file, before the S0 snapshot, so every boot
# restored from S0 (the provisioning run and the recovery boot after a power cut) has it.
#
# What changes for a service (everything else of hydo-init.sh is unchanged):
#   start_svc   its setup:, its command and its ready: check run inside its namespace (as a
#               Compose healthcheck runs inside the container); kills, pauses and the process-tree
#               reach of svc_tree are unchanged (one PID namespace)
#   net_on      a network fault is a netem qdisc on the veth where the packets leave:
#                 N=<svc> / O=<svc>:req  packets to <svc>'s port: on vh<i>, leaving the bridge
#                                        towards <svc> (from every other service and the workload)
#                 O=<svc>:resp           packets from <svc>'s port: on eth0 inside <svc>'s namespace
#                 N=a-b / O=a-b:req|resp packets a->b on b's vh, b->a on a's vh (u32 on source and
#                                        destination address); N=a-* the same for every other service
#   net_off     drains every netem queue of the fault (as net_heal in hydo-init.sh), then removes it
#   fc_arm/fc_rules  (faults: freeze) the netfilter rules and the helper hydo_fc run in the frozen
#               service's namespace (netfilter queues are per namespace): OUTPUT holds what it
#               sends, INPUT what it receives; the workload peer is $HYDO_NET_GW.
NSD=$H/ns
ns_mac() { echo "$1" | $BB awk -F. -v p="$2" '{ printf "%s:%02x:%02x:%02x:%02x\n", p, $1, $2, $3, $4 }'; }
ns_veth() { eval "printf '%s' \"\${NS_VETH_$1}\""; }      # <svc> -> its bridge port vh<i>
ns_v6off() { [ -d /proc/sys/net/ipv6/conf/$1 ] && echo 1 > /proc/sys/net/ipv6/conf/$1/disable_ipv6; return 0; }
ns_setup() {   # the bridge, one namespace + veth pair per service; output -> $H/netns.log
  local m s a i=0 f
  for m in llc stp bridge veth; do $BB insmod /lib/modules/$m.ko 2>/dev/null; done
  $BB mkdir -p $NSD
  $HYDO_IP link add hydo0 address $(ns_mac $HYDO_NET_GW 02:42) type bridge forward_delay 0 stp_state 0 || return 1
  ns_v6off hydo0
  $HYDO_IP addr add $HYDO_NET_GW/16 dev hydo0 || return 1
  for a in $HYDO_NET_EXTRA; do $HYDO_IP addr add $a/32 dev hydo0 || return 1; done
  $HYDO_IP link set hydo0 up || return 1
  for s in $HYDO_SERVICES; do
    i=$((i + 1)); a=$(sv $s addr); f=$NSD/$s
    : > $f && $BB unshare --net=$f true || return 1
    $HYDO_IP link add vh$i address $(ns_mac $a 02:43) type veth peer name eth0 address $(ns_mac $a 02:42) netns $f || return 1
    ns_v6off vh$i
    $HYDO_IP link set vh$i master hydo0 up || return 1
    $BB nsenter -n$f $BB sh -c "[ -d /proc/sys/net/ipv6/conf/eth0 ] && echo 1 > /proc/sys/net/ipv6/conf/eth0/disable_ipv6
      $HYDO_IP link set lo up && $HYDO_IP addr add $a/16 dev eth0 && $HYDO_IP link set eth0 up" || return 1
    eval "NS_VETH_$s=vh$i"
    echo "$s=$a@vh$i"
  done
}

start_svc() {   # <svc>: the service's setup, command and readiness check in its namespace
  local _bb=$BB rc
  BB="$_bb nsenter -n$NSD/$1 $_bb"      # RUN and the command line both go through $BB
  _ns_orig_start_svc "$@"; rc=$?
  BB=$_bb
  return $rc
}

ns_tc() {   # <namespace file|-> <tc args...>: tc in a service's namespace (- = the guest's own)
  local f=$1; shift
  if [ "$f" = - ]; then $HYDO_TC "$@"; else $BB nsenter -n$f $HYDO_TC "$@"; fi
}
NET_DEVS=""     # the devices of the network faults in force: <namespace file|->:<dev> ...
ns_fault_dev() {   # <ns|-> <dev> <netem args> <u32 match...>: a prio root + netem band once per
                   # device and fault (a second fault on the same device fails, as on lo), one filter
  local f=$1 d=$2 a=$3
  shift 3
  case " $NS_THIS " in
    *" $f:$d "*) ;;
    *) ns_tc $f qdisc add dev $d root handle 1: prio bands 4 priomap 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 && \
       ns_tc $f qdisc add dev $d parent 1:4 handle 40: netem $a || die_oracle "HYDO-ORACLE: ERROR tc setup failed"
       NS_THIS="$NS_THIS $f:$d"; NET_DEVS="$NET_DEVS $f:$d";;
  esac
  ns_tc $f filter add dev $d parent 1:0 protocol ip prio 1 u32 "$@" flowid 1:4 || die_oracle "HYDO-ORACLE: ERROR tc setup failed"
}
net_on() {   # <svc|link> <dport|sport> <netem args with commas>
  local ends args m dir a aa b ba port
  args=$(echo "$3" | $BB tr ',' ' ')
  [ -n "$HYDO_TC" ] || { log "network fault on $1 skipped (no tc)"; return 1; }
  for m in sch_prio sch_netem cls_u32; do $BB insmod /lib/modules/$m.ko 2>/dev/null; done
  NS_THIS=""
  if ends=$(link_ends "$1"); then
    dir=both                                   # N= (or O= and N= on the same link): both ways
    [ "$1" = "$osvc" ] && [ "$1" != "$nsvc" ] && dir=$2   # O=: dport = req (a->b), sport = resp (b->a)
    set -- $ends
    a=$1; aa=$(svc_addr $a); shift
    for b in "$@"; do
      ba=$(svc_addr $b)
      [ $dir != sport ] && ns_fault_dev - $(ns_veth $b) "$args" match ip src $aa/32 match ip dst $ba/32
      [ $dir != dport ] && ns_fault_dev - $(ns_veth $a) "$args" match ip src $ba/32 match ip dst $aa/32
    done
    log "LINK FAULT $a <-> $* ($dir): netem $args on$NS_THIS"
  else
    port=$(svc_port $1)
    [ -n "$port" ] || { log "network fault on $1 skipped (no port)"; return 1; }
    if [ "$2" = dport ]; then ns_fault_dev - $(ns_veth $1) "$args" match ip dport $port 0xffff
    else ns_fault_dev $NSD/$1 eth0 "$args" match ip sport $port 0xffff; fi
    log "NET FAULT on $1 (ip $2 $port): netem $args on$NS_THIS"
  fi
  NET_ARGS=$args
}
ns_qsum() {   # packets in the netem queues of every device in NET_DEVS
  local x n=0 q
  for x in $NET_DEVS; do
    q=$(ns_tc ${x%:*} -s qdisc show dev ${x##*:} | $BB awk '/^qdisc netem 40:/ { f = 1 } f && $1 == "backlog" { sub(/p$/, "", $3); print $3 + 0; exit }')
    n=$((n + ${q:-0}))
  done
  echo $n
}
ns_heal() {   # net_heal of hydo-init.sh (keep in sync) for every device of the faults in force
  local bound q0 n t=0 w=0 x st
  bound=$(echo "$NET_ARGS" | $BB tr ',' ' ' | $BB awk '
    function ms(v,  x) { x = v + 0; if (v ~ /us/) return x / 1000; if (v ~ /ms/) return x
                         if (v ~ /s/) return x * 1000; return x / 1000 }
    { m = 0; for (i = 1; i <= NF; i++) if ($i == "delay") { m = ms($(i + 1))
        if ($(i + 2) ~ /^[0-9.]+(us|usec|usecs|ms|msec|msecs|s|sec|secs)?$/) m += 4 * ms($(i + 2)) }
      printf "%d\n", m + 500 }')
  q0=$(ns_qsum); n=$q0
  for x in $NET_DEVS; do ns_tc ${x%:*} qdisc change dev ${x##*:} parent 1:4 handle 40: netem rate 100gbit corrupt 0% 2>/dev/null; done
  while [ "$n" -gt 0 ] && [ $t -lt $bound ]; do $BB sleep 0.01; t=$((t + 10)); n=$(ns_qsum); done
  for x in $NET_DEVS; do ns_tc ${x%:*} filter del dev ${x##*:} parent 1:0 2>/dev/null; done
  n=$(ns_qsum)
  while [ "$n" -gt 0 ] && [ $w -lt 50 ]; do $BB sleep 0.01; w=$((w + 10)); n=$(ns_qsum); done
  st=$(ns_stats)
  for x in $NET_DEVS; do ns_tc ${x%:*} qdisc del dev ${x##*:} root 2>/dev/null; done
  NET_DEVS=""
  echo "queued=$q0 drained_ms=$t left=$n acted on:$st"
}
ns_stats() {   # what each netem of the fault saw: <dev>[@<svc namespace>]:sent=<packets>,dropped=<packets>
  local x
  for x in $NET_DEVS; do
    printf ' %s%s:' ${x##*:} "$([ "${x%:*}" = - ] || echo "@${x%:*}" | $BB sed 's#.*/#@#')"
    ns_tc ${x%:*} -s qdisc show dev ${x##*:} | $BB awk '/^qdisc netem 40:/ { f = 1; next } f && $1 == "Sent" {
      d = $7; sub(/,$/, "", d); printf "sent=%s,dropped=%s", $4, d; exit }'
  done
}
net_off() { log "network fault lifted: $(ns_heal)"; }

if [ -n "$HYDO_FC" ]; then     # faults: freeze (hydo-init-freeze.sh)
fc_arm() {   # the helper binds the netfilter queue of the frozen service's namespace
  local _fc=$HYDO_FC rc
  HYDO_FC="$BB nsenter -n$NSD/$fc_svc $_fc"
  _ns_orig_fc_arm "$@"; rc=$?
  HYDO_FC=$_fc
  return $rc
}
fc_rules() {   # <-A|-D>: the pair's packets, in the frozen service's namespace
  local ipt sa ta tp
  ipt="$BB nsenter -n$NSD/$fc_svc $HYDO_IPT"
  sa=$(sv $fc_svc addr); ta=$(sv $fc_tgt addr)
  if [ "$fc_tgt" = workload ]; then          # the workload's clients connect from the bridge's address
    tp=$(sv $fc_svc port); [ -n "$tp" ] || return 3
    $ipt $1 INPUT -s $HYDO_NET_GW -d $sa -p tcp --dport $tp -j NFQUEUE --queue-num 7 --queue-bypass &&
    $ipt $1 OUTPUT -s $sa -d $HYDO_NET_GW -p tcp --sport $tp -j NFQUEUE --queue-num 7 --queue-bypass
  else
    $ipt $1 OUTPUT -s $sa -d $ta -j NFQUEUE --queue-num 7 --queue-bypass &&
    $ipt $1 INPUT -s $ta -d $sa -j NFQUEUE --queue-num 7 --queue-bypass
  fi
}
fi

if ns_setup > $H/netns.log 2>&1; then
  log "service network (network: bridge): hydo0 $HYDO_NET_GW/16${HYDO_NET_EXTRA:+ + $HYDO_NET_EXTRA}; $($BB tr '\n' ' ' < $H/netns.log)"
else
  $BB tail -8 $H/netns.log
  die_oracle "HYDO-ORACLE: ERROR service network setup failed (network: bridge)"
fi
