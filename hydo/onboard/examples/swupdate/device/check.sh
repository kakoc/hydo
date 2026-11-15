#!/bin/sh
# The device's promises, one per argument (the manifest's invariants). Exit 0 = holds,
# 1 = violated, 2 = could not be evaluated now. Each check first waits (at most 240 s) for the
# end of the boot in progress: a cold boot, a rollback's reboots, the confirmation.
. /opt/ota/device/lib.sh

settle() {
  n=0
  while [ $n -lt 1200 ]; do
    [ -f $RUN/bricked ] && return 0
    [ -f $RUN/booted ] && [ -f $RUN/marked ] && [ -f $RUN/ready ] && return 0
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
    echo "booted slot $(booted slot) v$(booted version) after $(booted gen) boot(s)"
    ;;
  version)   # an acknowledged install survives; an install that did not complete never boots
    settle
    v=$(booted version)
    allowed=$(allowed_versions)
    case " $allowed " in
      *" $v "*) echo "booted v$v, allowed: $allowed" ;;
      *) echo "booted v$v, but the updates allow only v$allowed (boots: $(tr '\n' ';' < $RUN/boots); outcomes: $(grep '^[ir]' "$HYDO_OUTCOMES" | tail -n 4 | tr '\n' ';'))"
         exit 1 ;;
    esac
    ;;
  primary)   # the slot U-Boot boots next holds a complete image
    settle
    s=$(envvar boot_slot A)
    set -- $(slot_content "$s")
    if [ "$1" = none ]; then
      echo "boot_slot=$s (upgrade_available=$(envvar upgrade_available 0)) holds no complete image"
      exit 1
    fi
    echo "boot_slot=$s holds v$1"
    ;;
  records)   # the application keeps every synced record
    for id in $(grep '^d' "$HYDO_ACKED"); do
      grep -qx "$id record-of-update-${id#d}" $APP/records 2>/dev/null || {
        echo "record $id was synced and is gone"; exit 1; }
    done
    echo "$(grep -c '^d' "$HYDO_ACKED") synced records present"
    ;;
  violations)   # every reboot during the workload booted a version the updates allow
    if [ -f $RUN/violations ]; then head -n 1 $RUN/violations; exit 1; fi
    echo "none"
    ;;
esac
exit 0
