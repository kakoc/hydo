#!/bin/sh
# The device's promises, one per argument (the manifest's invariants). Exit 0 = holds,
# 1 = violated, 2 = could not be evaluated now. Each check first waits (at most 240 s) for the
# end of the boot in progress: a cold boot, a rollback's reboots, the mark-good.
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
      *) echo "booted v$v, but the installs allow only v$allowed (outcomes: $(grep '^i' "$HYDO_OUTCOMES" | tail -n 4 | tr '\n' ';'))"
         exit 1 ;;
    esac
    ;;
  primary)   # the slot U-Boot boots first holds a complete image
    settle
    order=$(envvar BOOT_ORDER "A B")
    for s in $order; do
      l=$(envvar BOOT_${s}_LEFT 3)
      [ $((0x$l)) -gt 0 ] || continue
      set -- $(slot_content "$s")
      if [ "$1" = none ]; then
        echo "slot $s is first in BOOT_ORDER ($order) with $l attempts but holds no complete image"
        exit 1
      fi
      echo "primary slot $s holds v$1 ($order; A_LEFT=$(envvar BOOT_A_LEFT 3) B_LEFT=$(envvar BOOT_B_LEFT 3))"
      exit 0
    done
    echo "no slot has attempts left ($order); U-Boot resets the counters"
    ;;
  status)   # RAUC's own records agree with the slots
    settle
    [ -f $RUN/bricked ] && { echo "bricked: not checked"; exit 0; }
    out=$(rauc_dev status --detailed --output-format=shell 2>&1) || {
      echo "rauc status fails: $(echo "$out" | tail -n 1)"; exit 1; }
    f=$PART/rauc/central.raucs
    [ -f $f ] || { echo "no status file yet"; exit 0; }
    bad=0
    for n in 0 1; do
      s=$( [ $n = 0 ] && echo A || echo B )
      st=$(awk -v g="[slot.rootfs.$n]" '$0 == g { on = 1; next } /^\[/ { on = 0 } on && /^status=/ { sub(/^status=/, ""); print }' $f)
      sha=$(awk -v g="[slot.rootfs.$n]" '$0 == g { on = 1; next } /^\[/ { on = 0 } on && /^sha256=/ { sub(/^sha256=/, ""); print }' $f)
      size=$(awk -v g="[slot.rootfs.$n]" '$0 == g { on = 1; next } /^\[/ { on = 0 } on && /^size=/ { sub(/^size=/, ""); print }' $f)
      [ "$st" = ok ] && [ -n "$sha" ] && [ -n "$size" ] || continue
      have=$(head -c "$size" "$(slot_dev $s)" | sha256sum | cut -d' ' -f1)
      if [ "$have" != "$sha" ]; then
        set -- $(slot_content $s)
        claim=$(awk -v h="$sha" '$1 == h { print "v" $3 }' $OTA/images/known)
        echo "RAUC's status says slot $s (rootfs.$n) is ok with ${claim:-an unknown image} ($sha), but the slot holds ${1:+v}$1"
        bad=1
      fi
    done
    [ $bad = 0 ] || exit 1
    echo "the status of both slots matches their contents"
    ;;
  records)   # the data partition keeps every synced record
    for id in $(grep '^d' "$HYDO_ACKED"); do
      grep -qx "$id record-of-update-${id#d}" $APP/records 2>/dev/null || {
        echo "record $id was synced and is gone"; exit 1; }
    done
    echo "$(grep -c '^d' "$HYDO_ACKED") synced records present"
    ;;
  violations)   # every reboot during the workload booted a version the installs allow
    if [ -f $APP/violations ]; then head -n 1 $APP/violations; exit 1; fi
    echo "none"
    ;;
esac
exit 0
