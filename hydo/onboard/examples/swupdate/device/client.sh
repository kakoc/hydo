#!/bin/sh
# The workload: an update server and an operator, over the agent's request directory.
#   client.sh FROM TO   steps [FROM, TO). Two steps per update k (k = step / 2):
#     step 2k     reboots the device into update k-1 (k > 0), waits until the booted slot is
#                 marked good (or the device rolled back, or is bricked) and checks that the
#                 booted version is one the updates so far allow (lib.sh, allowed_versions);
#                 then asks the agent to install bundle v<2 + k mod 4>, and returns after a delay
#                 of its own (0 to 1.75 s), so that a fault before step 2k+1 meets the install at
#                 a point that depends on k;
#     step 2k+1   waits for the install's answer and appends one synced record to the
#                 application's data. The reboot comes in the next step, as a device installs
#                 in the background and reboots later: a fault before step 2k+2 meets an
#                 installed update that has not booted yet.
#   client.sh reboot    the reboot into the last update (workload.retry: after the heal, when
#                       no power cut ended the run; a power cut's recovery boot is that reboot).
# Records: install k is id i<k> (ack: the tool exited 0; failed: another exit code; unknown: no
# answer); the reboot into update k is id r<k> (unknown) and then r<k>v<version> when the client
# saw the booted version; record k is id d<k>. A wrong booted version goes to $RUN/violations,
# which the invariant device.reboots_boot_allowed reads.
. /opt/ota/device/lib.sh
mkdir -p $RUN/req $RUN/res

# the agent's process is alive (a kill stops it until hydo starts it again, after this step)
agent_alive() { p=$(cat $RUN/agent.pid 2>/dev/null) && [ -n "$p" ] && kill -0 "$p" 2>/dev/null; }

wait_file() {   # file seconds
  n=0
  while [ ! -f "$1" ] && [ $n -lt $(($2 * 5)) ] && agent_alive; do sleep 0.2; n=$((n + 1)); done
  [ -f "$1" ]
}

reboot_and_wait() {   # id; prints the booted version, "bricked", or "timeout"
  old=$(booted gen)
  echo reboot > "$RUN/req/$1.tmp" && mv "$RUN/req/$1.tmp" "$RUN/req/$1"
  n=0
  while [ $n -lt 900 ]; do   # 180 s: a broken version boots three times before the rollback
    if [ -f $RUN/bricked ]; then echo bricked; return; fi
    agent_alive || break
    g=$(booted gen)
    if [ -n "$g" ] && [ "$g" != "$old" ] && [ -f $RUN/ready ] && [ -f $RUN/marked ]; then
      booted version
      return
    fi
    sleep 0.2; n=$((n + 1))
  done
  echo timeout
}

reboot_into() {   # k: the reboot into update k, and the check of the booted version
  echo "r$1 unknown" >> "$HYDO_OUTCOMES"
  got=$(reboot_and_wait "r$1")
  allowed=$(allowed_versions "$1")
  if [ "$got" = timeout ]; then   # the agent was down (a kill): nothing to compare
    say "update $1: the reboot did not finish in time"
    return
  fi
  echo "r${1}v$got ack" >> "$HYDO_OUTCOMES"
  case " $allowed " in
    *" $got "*) say "update $1: booted v$got (allowed: $allowed)" ;;
    *) say "update $1: booted $got, allowed: $allowed"
       echo "after update $1 (v$(bundle_of $1)) the device booted $got; the updates allow v$allowed" >> $RUN/violations ;;
  esac
}

if [ "$1" = reboot ]; then
  reboot_into $((HYDO_STEPS / 2 - 1))
  exit 0
fi

step=$1
while [ "$step" -lt "$2" ]; do
  k=$((step / 2))
  if [ $((step % 2)) = 0 ]; then
    [ $k -gt 0 ] && reboot_into $((k - 1))
    v=$(bundle_of $k)
    echo "i$k unknown" >> "$HYDO_OUTCOMES"
    echo "install $v" > "$RUN/req/i$k.tmp" && mv "$RUN/req/i$k.tmp" "$RUN/req/i$k"
    sleep "$(((k * 5) % 8 / 4)).$(((k * 5) % 8 % 4 * 25))"
  else
    if wait_file "$RUN/res/i$k" 120; then
      case $(cat "$RUN/res/i$k") in
        0) echo "i$k" >> "$HYDO_ACKED"; echo "i$k ack" >> "$HYDO_OUTCOMES" ;;
        lost) echo "i$k unknown" >> "$HYDO_OUTCOMES" ;;
        *) echo "i$k failed" >> "$HYDO_OUTCOMES" ;;
      esac
    fi
    # the application's record, synced before it counts
    echo "d$k record-of-update-$k" >> $APP/records && fsync $APP/records && \
      { echo "d$k" >> "$HYDO_ACKED"; echo "d$k ack" >> "$HYDO_OUTCOMES"; }
  fi
  step=$((step + 1))
done
