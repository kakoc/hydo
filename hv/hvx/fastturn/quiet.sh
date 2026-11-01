#!/bin/bash
# Run a measurement script in a quiet window on the shared box: create /var/tmp/hydo-QUIET
# (other sessions' agents in ~/hydo-asw and ~/hydo-pf start no new VM while it exists),
# wait (at most 30 min) until their engine (hydo-hvx, or an explorer or hydo running it in its
# own process; hvx_explore before M137) / hvx_stack_verify processes are gone, run
# the given command, and always remove the file again.
#   quiet.sh <command...>
set -u
Q=/var/tmp/hydo-QUIET
touch $Q
trap 'rm -f $Q' EXIT; trap 'exit 1' INT TERM
# busy = a process from those trees that used more than 0.5 s of CPU in the last 5 s (their
# idle explorer workers stay alive while they wait for the file to go away)
cpu() { awk '{print $14 + $15}' /proc/$1/stat 2>/dev/null || echo 0; }
busy() {
  local p e a b pids=""
  for p in $(pgrep -x hydo-hvx; pgrep -x hydo-explore; pgrep -x hydo; pgrep -x hvx_explore; pgrep -x hvx_stack_verif); do
    e=$(sudo readlink /proc/$p/exe 2>/dev/null; sudo readlink /proc/$p/cwd 2>/dev/null)
    case "$e" in *hydo-asw*|*hydo-pf*) pids="$pids $p";; esac
  done
  [ -z "$pids" ] && return 1
  declare -A t0
  for p in $pids; do t0[$p]=$(cpu $p); done
  sleep 5
  for p in $pids; do a=${t0[$p]}; b=$(cpu $p); [ $((b - a)) -gt 50 ] && return 0; done
  return 1
}
end=$((SECONDS + 1800))
while busy; do
  [ $SECONDS -ge $end ] && { echo "quiet.sh: other runs still active after 30 min, giving up"; exit 1; }
  sleep 10
done
echo "quiet.sh: quiet at $(date +%T) after $SECONDS s; load: $(cut -d' ' -f1-3 /proc/loadavg)"
echo "quiet.sh: other VM processes now: $(pgrep -c -x hydo-hvx) hydo-hvx, $(pgrep -c -x hydo-explore) hydo-explore, $(pgrep -c -x hvx_stack_verif) hvx_stack_verify"
"$@"
rc=$?
echo "quiet.sh: finished at $(date +%T), load: $(cut -d' ' -f1-3 /proc/loadavg)"
exit $rc
