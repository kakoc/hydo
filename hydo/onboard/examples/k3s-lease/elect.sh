#!/bin/sh
# Leader election on a Kubernetes Lease, done the way client-go's leaderelection package does it,
# with kubectl from a shell so that the image needs nothing but busybox:
#   - a candidate takes the Lease when holderIdentity is empty, or when renewTime +
#     leaseDurationSeconds has passed on its own wall clock; the write carries the object's
#     resourceVersion, so of two candidates only one can win;
#   - the holder renews every RETRY seconds, writing its wall clock into renewTime;
#   - the holder keeps acting as leader until RENEW_DEADLINE seconds have passed on its
#     monotonic clock since the start of its last successful renewal, or until it sees another
#     holder.
# While it leads, the pod does its "work" every half second: it appends a line `L <uptime in
# centiseconds>` to its log. Every pod also logs each acquisition, loss and step-down. The
# logs are on the guest's data disk (a hostPath), so the hydo invariant can read them.
ID=$(hostname)
K=/k3s/kubectl
LEASE=elect
DUR=10            # leaseDurationSeconds
RD=5              # renewDeadline (seconds, monotonic)
RETRY=2           # retryPeriod (seconds)
LOG=/log/$ID.log
mono() { awk '{ printf "%d", $1 * 100 }' /proc/uptime; }
say() { echo "$1 $(mono) $ID $2" >> "$LOG"; }
wall_of() {   # RFC 3339 renewTime -> unix seconds
  date -u -D '%Y-%m-%dT%H:%M:%S' -d "${1%%.*}" +%s 2>/dev/null || echo 0
}
put() {   # put RV TRANSITIONS ACQUIRE-TIME: write the Lease as held by this pod (RV empty: create)
  now=$(date -u +%Y-%m-%dT%H:%M:%S.000000Z)
  {
    echo "apiVersion: coordination.k8s.io/v1"
    echo "kind: Lease"
    echo "metadata:"
    echo "  name: $LEASE"
    [ -n "$1" ] && echo "  resourceVersion: \"$1\""
    echo "spec:"
    echo "  holderIdentity: $ID"
    echo "  leaseDurationSeconds: $DUR"
    echo "  leaseTransitions: $2"
    echo "  acquireTime: ${3:-$now}"
    echo "  renewTime: $now"
  } > /tmp/lease.yaml
  if [ -n "$1" ]; then $K replace -f /tmp/lease.yaml > /dev/null 2>/tmp/put.err
  else $K create -f /tmp/lease.yaml > /dev/null 2>/tmp/put.err; fi
}

say S "start"
lead_until=0      # monotonic centiseconds until which this pod leads; 0: it does not lead
next_try=0
while :; do
  t0=$(mono)
  if [ "$t0" -ge "$next_try" ]; then
    next_try=$((t0 + RETRY * 100))
    if out=$($K get lease $LEASE -o 'jsonpath={.spec.holderIdentity}|{.spec.renewTime}|{.metadata.resourceVersion}|{.spec.leaseTransitions}|{.spec.acquireTime}' 2>/tmp/get.err); then
      holder=$(echo "$out" | cut -d'|' -f1); renew=$(echo "$out" | cut -d'|' -f2)
      rv=$(echo "$out" | cut -d'|' -f3); trans=$(echo "$out" | cut -d'|' -f4)
      acq=$(echo "$out" | cut -d'|' -f5)
      [ -n "$trans" ] || trans=0
      expired=0
      [ $(( $(wall_of "$renew") + DUR )) -le "$(date +%s)" ] && expired=1
      if [ "$holder" = "$ID" ]; then
        if put "$rv" "$trans" "$acq"; then
          [ "$lead_until" -gt 0 ] || say A "resumed"
          lead_until=$((t0 + RD * 100))
        fi
      elif [ -z "$holder" ] || [ "$expired" = 1 ]; then
        if put "$rv" $((trans + 1)); then
          say A "acquired from ${holder:-nobody}"
          lead_until=$((t0 + RD * 100))
        fi
      elif [ "$lead_until" -gt 0 ]; then
        say D "lost to $holder"
        lead_until=0
      fi
    elif grep -q NotFound /tmp/get.err && put "" 0; then
      say A "created"
      lead_until=$((t0 + RD * 100))
    fi
  fi
  t1=$(mono)
  if [ "$lead_until" -gt "$t1" ]; then
    echo "L $t1" >> "$LOG"
  elif [ "$lead_until" -gt 0 ]; then
    say D "renew deadline passed"
    lead_until=0
  fi
  sleep 0.5
done
