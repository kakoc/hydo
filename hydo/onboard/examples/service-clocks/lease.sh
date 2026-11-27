#!/bin/sh
# One node of the example's lease (Milestone 372). The lease is the file /tmp/lease, "<holder>
# <expiry>", with the expiry in milliseconds of the writer's own wall clock. Every 250 ms the node
# reads its wall clock (clockprobe, a static Go program), and takes or renews the lease when it
# holds it, when there is none, or when the expiry has passed by its own clock. It counts itself
# the leader while its own clock is before the expiry it wrote last, as a leader that trusts its
# lease until it runs out. Each turn as leader appends "<uptime> <node>" to /tmp/leads; the
# uptime is the guest kernel's, the same for both nodes.
me=$1
L=/tmp/lease
myexp=0
n=0
while :; do
  now=$(/bin/clockprobe)
  up=$(cut -d' ' -f1 /proc/uptime)
  h=; e=0
  [ -f $L ] && read h e < $L
  if [ "$h" = "$me" ] || [ -z "$h" ] || [ "$now" -ge "$e" ]; then
    myexp=$((now + 3000))
    echo "$me $myexp" > $L.$me && mv $L.$me $L
  fi
  if [ "$now" -lt "$myexp" ]; then echo "$up $me" >> /tmp/leads; fi
  echo "$up $now" >> /tmp/clock.$me
  if [ $((n % 8)) = 0 ]; then
    echo "hydo-clocks: $me uptime=$up go=$now date=$(date +%s)" > /dev/ttyS0
  fi
  n=$((n + 1))
  sleep 0.25
done
