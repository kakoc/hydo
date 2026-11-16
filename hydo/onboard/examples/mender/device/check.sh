#!/bin/sh
# The device's promises, one per argument (the manifest's invariants). Exit 0 = holds,
# 1 = violated, 2 = could not be evaluated now. Each check first waits (at most 240 s) for the
# end of the boot in progress: a cold boot, a rollback's reboots, the commit.
. /opt/ota/device/lib.sh

settle() {
  n=0
  while [ $n -lt 1200 ]; do
    [ -f $RUN/bricked ] && return 0
    [ -f $RUN/booted ] && [ -f $RUN/checked ] && [ -f $RUN/ready ] && return 0
    sleep 0.2; n=$((n + 1))
  done
  echo "the device did not finish booting in 240 s (boots: $(tr '\n' ';' < $RUN/boots 2>/dev/null))"
  exit 1
}

case $1 in
  boots)   # after any power cut, the device boots a complete, verified slot
    settle
    if [ -f $RUN/bricked ]; then
      echo "bricked: no slot verified in $MAX_RESETS boots (boots: $(tr '\n' ';' < $RUN/boots))"
      exit 1
    fi
    echo "booted partition $(booted part) v$(booted version) after $(booted gen) boot(s)"
    ;;
  version)   # an acknowledged install or commit survives; an install that did not complete never boots
    settle
    v=$(booted version)
    allowed=$(allowed_versions)
    case " $allowed " in
      *" $v "*) echo "booted v$v, allowed: $allowed" ;;
      *) echo "booted v$v, but the updates allow only v$allowed (boots: $(tr '\n' ';' < $RUN/boots); outcomes: $(grep '^[irc]' "$HYDO_OUTCOMES" | tail -n 5 | tr '\n' ';'); commits: $(tr '\n' ';' < $RUN/commits 2>/dev/null))"
         exit 1 ;;
    esac
    ;;
  primary)   # the partition U-Boot boots next holds a complete image
    settle
    set -- $(env_now)
    p=$1 ua=$2
    set -- $(slot_content "$p")
    if [ "$1" = none ]; then
      echo "mender_boot_part=$p (upgrade_available=$ua) holds no complete image"
      exit 1
    fi
    echo "mender_boot_part=$p holds v$1"
    ;;
  commit)   # the operator's next commit: one that exits 0 leaves the booted version permanent
    # The last check of the manifest, because it changes the device: after the run (the end of
    # the workload, or a power cut's recovery boot) the operator commits an update that Mender
    # still has in progress, as after any reboot.
    settle
    [ -f $RUN/bricked ] && { echo "bricked"; exit 0; }
    s=$(mender_state)
    [ "$s" = none ] && { echo "no update in progress"; exit 0; }
    # an update that booted, is healthy and waits for its commit: the commit must not fail
    waiting=no
    [ "${s#* }" = "v$(booted version)" ] && [ "$(booted health)" = ok ] && [ "$(envvar upgrade_available 0)" = 1 ] && waiting=yes
    a=$(mender commit)
    set -- $(env_now)
    p=$1 ua=$2
    art=$(mender_artifact)
    after=$(mender_state)
    if [ "$waiting" = yes ] && [ "${a%% *}" != 0 ]; then
      e=$(echo "${a#* }" | sed 's/^Could not fulfill request: No error information: //; s/;.*//')
      echo "the commit of v$(booted version), which booted healthy and waits for it, failed: Mender names $art (state $after), U-Boot boots partition $p (upgrade_available=$ua); its error: $e"
      exit 1
    fi
    if [ "${a%% *}" = 0 ]; then   # Mender: the commit marks the update successful and permanent
      if [ "$p" != "$(booted part)" ] || [ "$ua" != 0 ]; then
        set -- $(slot_content "$p")
        echo "with Mender's update in progress ($s), mender-update commit exited 0 ('${a#0 }') on v$(booted version) in partition $(booted part), but U-Boot boots partition $p (v$1) next, upgrade_available=$ua"
        exit 1
      fi
      if [ "$art" != "${s#* }" ]; then
        echo "with Mender's update ${s#* } in progress (${s%% *}), mender-update commit exited 0 ('${a#0 }'), but Mender's artifact is $art and the device runs v$(booted version)"
        exit 1
      fi
    fi
    echo "with Mender's update in progress ($s), mender-update commit: $a; U-Boot boots partition $p next (upgrade_available=$ua), Mender's artifact $art"
    ;;
  state)   # Mender's own state matches the slots
    settle
    [ -f $RUN/bricked ] && { echo "bricked"; exit 0; }
    set -- $(env_now)
    p=$1 ua=$2
    art=$(mender_artifact)
    set -- $(mender_state)
    st=$1 name=${2:-}
    running=v$(booted version)
    if [ "$st" = none ]; then
      if [ "$art" != "$running" ]; then
        set -- $(slot_content "$p")
        echo "no update in progress, Mender's artifact is $art, but the device runs $running (U-Boot boots partition $p, v$1, next)"
        exit 1
      fi
      if [ "$p" != "$(booted part)" ] || [ "$ua" != 0 ]; then
        echo "no update in progress and Mender's artifact is $art, but U-Boot boots partition $p next (upgrade_available=$ua), not the running $running"
        exit 1
      fi
      echo "Mender's artifact $art runs from partition $p"
    else   # an update in progress: Mender's next command (commit, rollback or resume) ends it
      echo "update $name in progress ($st), Mender's artifact $art, U-Boot boots partition $p next (upgrade_available=$ua), the device runs $running"
    fi
    ;;
  records)   # the application keeps every synced record
    for id in $(grep '^d' "$HYDO_ACKED"); do
      grep -qx "$id record-of-update-${id#d}" $ROOT/data/app/records 2>/dev/null || {
        echo "record $id was synced and is gone"; exit 1; }
    done
    echo "$(grep -c '^d' "$HYDO_ACKED") synced records present"
    ;;
  violations)   # every reboot during the workload booted a version the updates allow
    if [ -f $ROOT/data/app/violations ]; then head -n 1 $ROOT/data/app/violations; exit 1; fi
    echo "none"
    ;;
esac
exit 0
