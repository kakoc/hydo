#!/bin/sh
# The workload: an operator of standalone deployments (Mender's documentation: install, reboot,
# commit), over the agent's request directory.
#   client.sh FROM TO   steps [FROM, TO). Two steps per update k (k = step / 2):
#     step 2k     (k > 0) asks the agent for the reboot into update k-1. The reboot is a real
#                 one (guest.reboot): hydo shuts the guest down, and this process with it, and
#                 the run goes on with step 2k+1 in the new boot. When the agent is down (a
#                 kill), no reboot comes and the step ends after at most 120 s.
#     step 2k+1   (k > 0) waits for the device's boot, records the booted version and checks
#                 it against the versions the updates so far allow (lib.sh, allowed_versions);
#                 asks the agent for `mender-update commit` (once more when the answer is
#                 lost) and checks that a commit that exited 0 left the booted version
#                 permanent; then asks the agent to install Artifact v<2 + k mod 4>, waits for
#                 the answer, and appends one synced record to the application's data on the
#                 data partition. The reboot comes in the next step, as a device installs in
#                 the background and reboots later.
#   client.sh reboot    the reboot into the last update (workload.retry: after the heal, when
#                       no power cut ended the run; a power cut's recovery boot is that reboot).
# Records: install k is id i<k> (ack: mender-update exited 0; failed: another exit code;
# unknown: no answer); the reboot into update k is id r<k> (unknown) and then r<k>v<version>
# when the client saw the booted version; the commit after it is id c<k> (ack: exit 0; none:
# exit 2, no update in progress; failed: another exit code; unknown: no answer); record k is id
# d<k>. A wrong booted version or a commit that did not hold goes to /data/app/violations on
# the data partition (synced), which the invariant device.reboots_boot_allowed reads.
. /opt/ota/device/lib.sh
mkdir -p $RUN/req $RUN/res
APP=$ROOT/data/app

# the agent's process is alive (a kill stops it until hydo starts it again)
agent_alive() { p=$(cat $RUN/agent.pid 2>/dev/null) && [ -n "$p" ] && kill -0 "$p" 2>/dev/null; }

wait_file() {   # file seconds
  n=0
  while [ ! -f "$1" ] && [ $n -lt $(($2 * 5)) ] && agent_alive; do sleep 0.2; n=$((n + 1)); done
  [ -f "$1" ]
}

ask() {   # id request...: one request to the agent; prints its answer, or "lost"
  id=$1; shift
  echo "$*" > "$RUN/req/$id.tmp" && mv "$RUN/req/$id.tmp" "$RUN/req/$id"
  if wait_file "$RUN/res/$id" 120; then cat "$RUN/res/$id"; else echo lost; fi
}

violation() {
  say "$*"
  echo "$*" >> $APP/violations && fsync $APP/violations
}

ask_reboot() {   # k: the reboot into update k; the reboot ends this process
  echo "r$1 unknown" >> "$HYDO_OUTCOMES"
  : > $RUN/reboot-asked
  echo reboot > "$RUN/req/r$1.tmp" && mv "$RUN/req/r$1.tmp" "$RUN/req/r$1"
  n=0
  while [ $n -lt 600 ] && agent_alive; do sleep 0.2; n=$((n + 1)); done
  say "update $1: no reboot came (the agent is down)"
}

wait_booted() {   # prints the booted version, "bricked", or "timeout"
  n=0
  while [ $n -lt 900 ]; do
    if [ -f $RUN/bricked ]; then echo bricked; return; fi
    if [ -f $RUN/booted ] && [ -f $RUN/ready ] && [ -f $RUN/checked ]; then
      booted version
      return
    fi
    sleep 0.2; n=$((n + 1))
  done
  echo timeout
}

check_reboot() {   # k: the device after the reboot into update k, and its commit
  if [ -f $RUN/reboot-asked ]; then   # RAM survived: the reboot did not happen
    rm -f $RUN/reboot-asked
    say "update $1: the device did not reboot"
    return
  fi
  got=$(wait_booted)
  [ "$got" = timeout ] && { say "update $1: the device did not finish booting"; return; }
  allowed=$(allowed_versions "$1")
  echo "r${1}v$got ack" >> "$HYDO_OUTCOMES"
  case " $allowed " in
    *" $got "*) say "update $1: booted v$got (allowed: $allowed)" ;;
    *) violation "after update $1 (v$(bundle_of $1)) the device booted $got; the updates allow v$allowed" ;;
  esac
  [ "$got" = bricked ] && return
  echo "c$1 unknown" >> "$HYDO_OUTCOMES"
  a=$(ask "c$1" commit)
  if [ "$a" = lost ]; then   # the operator commits again once the device answers
    n=0; while [ $n -lt 300 ] && ! agent_alive; do sleep 0.2; n=$((n + 1)); done
    rm -f "$RUN/req/c$1" "$RUN/res/c$1"
    a=$(ask "c$1" commit)
  fi
  case ${a%% *} in
    0) echo "c$1 ack" >> "$HYDO_OUTCOMES" ;;
    2) echo "c$1 none" >> "$HYDO_OUTCOMES" ;;
    lost) echo "c$1 unknown" >> "$HYDO_OUTCOMES" ;;
    *) echo "c$1 failed" >> "$HYDO_OUTCOMES" ;;
  esac
  say "update $1: mender-update commit: $a"
  if [ "${a%% *}" = 0 ]; then   # Mender: the commit marks the update permanent
    set -- "$1" $(env_now)
    if [ "$2" != "$(booted part)" ] || [ "$3" != 0 ]; then
      violation "after update $1 mender-update commit exited 0 ('${a#0 }') on v$got in partition $(booted part), but U-Boot boots partition $2 next (upgrade_available=$3)"
    elif [ "$(mender_artifact)" != "v$got" ]; then
      violation "after update $1 mender-update commit exited 0 ('${a#0 }') on v$got, but Mender's artifact is $(mender_artifact)"
    fi
  fi
}

if [ "$1" = reboot ]; then
  ask_reboot $((HYDO_STEPS / 2 - 1))
  exit 0
fi

step=$1
while [ "$step" -lt "$2" ]; do
  k=$((step / 2))
  if [ $((step % 2)) = 0 ]; then
    [ $k -gt 0 ] && ask_reboot $((k - 1))
  else
    [ $k -gt 0 ] && check_reboot $((k - 1))
    v=$(bundle_of $k)
    echo "i$k unknown" >> "$HYDO_OUTCOMES"
    a=$(ask "i$k" install $v)
    case ${a%% *} in
      0) echo "i$k" >> "$HYDO_ACKED"; echo "i$k ack" >> "$HYDO_OUTCOMES" ;;
      lost) ;;
      *) echo "i$k failed" >> "$HYDO_OUTCOMES" ;;
    esac
    say "update $k: mender-update install v$v: $a"
    # the application's record on the data partition, synced before it counts
    echo "d$k record-of-update-$k" >> $APP/records && fsync $APP/records && \
      { echo "d$k" >> "$HYDO_ACKED"; echo "d$k ack" >> "$HYDO_OUTCOMES"; }
  fi
  step=$((step + 1))
done
