#!/bin/sh
# workload.sh FROM TO: steps [FROM, TO). Each step lets the cluster run for 1.3 s of guest time, so
# that faults at different steps meet the pods' 2 s renewal rhythm at different phases.
# Before the step named by /tmp/hydo-k3s-kill-at (the knob Z), it kills the process of the pod
# that holds the Lease, as a crash of the leader would: SIGKILL to the container's first
# process, from the node, so the container ends and the kubelet starts it again. The same pod's
# process is killed again before each of the next 3 steps, so the pod crashes in a loop and the
# kubelet waits before it starts it again (its back-off), as a leader that crashes at every start.
kc() { /opt/k3s/data/$(cat /opt/k3s/data-hash)/bin/kubectl "$@"; }
up() { cut -d' ' -f1 /proc/uptime; }
kill_at=$(cat /tmp/hydo-k3s-kill-at 2>/dev/null || echo none)
i=$1
while [ "$i" -lt "$2" ]; do
  if [ "$kill_at" != none ] && [ "$i" -ge "$kill_at" ] && [ "$i" -lt $((kill_at + 4)) ]; then
    if [ "$i" = "$kill_at" ]; then
      kc get lease elect -o 'jsonpath={.spec.holderIdentity}' > /tmp/hydo-k3s-victim 2>/dev/null
    fi
    h=$(cat /tmp/hydo-k3s-victim 2>/dev/null)
    pid=""
    for p in /proc/[0-9]*; do
      tr '\0' '\n' < "$p/environ" 2>/dev/null | grep -qx "HOSTNAME=$h" || continue
      [ "$(tr '\0' ' ' < "$p/cmdline" 2>/dev/null)" = "sh /app/elect.sh " ] || continue
      pid=${p#/proc/}
    done
    if [ -n "$h" ] && [ -n "$pid" ]; then
      kill -9 "$pid"
      echo "hydo-k3s: step $i: killed $h (pid $pid) at uptime $(up)" > /dev/console
    else
      echo "hydo-k3s: step $i: no process of '$h' to kill at uptime $(up)" > /dev/console
    fi
  fi
  sleep 1.3
  i=$((i + 1))
done
echo "hydo-k3s: steps $1..$2 done at uptime $(up)" > /dev/console
if [ "$2" = "${HYDO_STEPS:-0}" ]; then   # after the last step: what happened, for the reader
  kc get pods -o 'custom-columns=POD:.metadata.name,RESTARTS:.status.containerStatuses[0].restartCount,READY:.status.containerStatuses[0].ready' 2>&1 |
    sed 's/^/hydo-k3s: /' > /dev/console
  kc get lease elect -o 'jsonpath=holder={.spec.holderIdentity} transitions={.spec.leaseTransitions}' 2>&1 |
    sed 's/^/hydo-k3s: lease /' > /dev/console
  echo > /dev/console
  grep -h '^[ASD] ' /data/elect/*.log | sort -n -k2 | sed 's/^/hydo-k3s: event /' > /dev/console
fi
