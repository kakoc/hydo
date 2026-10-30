# hydo-init-links.sh -- network faults on the link between two named services
# (hvx/onboard/MANIFEST.md, "Link faults"). hydo-build copies this file to /etc/hydo/init-links.sh
# and adds ONE line to /sbin/init that sources it just before hydo-init.sh's main section, but
# ONLY for a manifest in which a service has an addr: -- every other image stays byte-identical.
#
# Sourced after hydo-init.sh's own functions, it redefines two of them:
#   known_svc <name>   also accepts a link: "a-b" (two services that have addr:) or "a-*" (a and
#                      every other service that has addr:), so N=/O= tokens can name a link
#   net_on <svc|link> <dport|sport> <netem args>
#                      on a link: netem on the packets between the two addresses (tc u32 match on
#                      the IP source and destination), not on a port. N= on a link: both
#                      directions. O= on a link: req = packets from a to b, resp = from b to a.
#                      On a plain service name: net_on_port, a copy of hydo-init.sh's net_on.
# A service's packets carry its addr: only if it listens on that address AND its outgoing
# connections leave from it; on loopback the kernel picks 127.0.0.1 as the source unless the
# program binds or the routing says otherwise (examples/etcd/hydo.yaml shows a per-uid recipe).
svc_addr() { sv "$1" addr; }
link_ends() {   # <link> -> "a b1 [b2 ...]" (first end, then the other ends); status 1 if not a link
  local a b s
  case "$1" in *-*) ;; *) return 1;; esac
  a=${1%%-*}; b=${1#*-}
  case "$a" in ""|*[!a-z0-9_]*) return 1;; esac
  [ -n "$(svc_addr $a)" ] || return 1
  if [ "$b" = "*" ]; then
    b=""
    for s in $HYDO_SERVICES; do [ "$s" != "$a" ] && [ -n "$(svc_addr $s)" ] && b="$b $s"; done
    [ -n "$b" ] || return 1
  else
    case "$b" in ""|*[!a-z0-9_]*) return 1;; esac
    [ "$b" != "$a" ] && [ -n "$(svc_addr $b)" ] || return 1
  fi
  echo $a $b
}
known_svc() {
  local s
  for s in $HYDO_SERVICES; do [ "$s" = "$1" ] && return 0; done
  link_ends "$1" > /dev/null
}
net_on_port() {   # hydo-init.sh's net_on (one service, match on its port), unchanged
  local port args m
  port=$(svc_port $1); args=$(echo "$3" | $BB tr ',' ' ')
  [ -n "$port" ] && [ -n "$HYDO_TC" ] || { log "network fault on $1 skipped (no port or no tc)"; return 1; }
  for m in sch_prio sch_netem cls_u32; do $BB insmod /lib/modules/$m.ko 2>/dev/null; done
  $HYDO_TC qdisc add dev lo root handle 1: prio bands 4 priomap 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 && \
  $HYDO_TC qdisc add dev lo parent 1:4 handle 40: netem $args && \
  $HYDO_TC filter add dev lo parent 1:0 protocol ip prio 1 u32 match ip $2 $port 0xffff flowid 1:4 || \
    die_oracle "HYDO-ORACLE: ERROR tc setup failed"
  NET_ARGS=$args
  log "NET FAULT on $1 (ip $2 $port): netem $args"
}
net_on() {
  local ends args m dir a aa b ba
  ends=$(link_ends "$1") || { net_on_port "$@"; return; }
  args=$(echo "$3" | $BB tr ',' ' ')
  dir=both                                   # N= (or O= and N= on the same link): both ways
  [ "$1" = "$osvc" ] && [ "$1" != "$nsvc" ] && dir=$2   # O=: dport = req (a->b), sport = resp (b->a)
  [ -n "$HYDO_TC" ] || { log "link fault on $1 skipped (no tc)"; return 1; }
  for m in sch_prio sch_netem cls_u32; do $BB insmod /lib/modules/$m.ko 2>/dev/null; done
  $HYDO_TC qdisc add dev lo root handle 1: prio bands 4 priomap 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 && \
  $HYDO_TC qdisc add dev lo parent 1:4 handle 40: netem $args || die_oracle "HYDO-ORACLE: ERROR tc setup failed"
  set -- $ends
  a=$1; aa=$(svc_addr $a); shift
  for b in "$@"; do
    ba=$(svc_addr $b)
    if [ $dir != sport ]; then
      $HYDO_TC filter add dev lo parent 1:0 protocol ip prio 1 u32 match ip src $aa/32 match ip dst $ba/32 flowid 1:4 || \
        die_oracle "HYDO-ORACLE: ERROR tc setup failed"
    fi
    if [ $dir != dport ]; then
      $HYDO_TC filter add dev lo parent 1:0 protocol ip prio 1 u32 match ip src $ba/32 match ip dst $aa/32 flowid 1:4 || \
        die_oracle "HYDO-ORACLE: ERROR tc setup failed"
    fi
  done
  NET_ARGS=$args
  log "LINK FAULT $a <-> $* ($dir): netem $args"
}
