# ---------------------------------------------------------------- recovery clock (hydo-init-clock.sh)
# guest.clock_after_cut (hvx/onboard/MANIFEST.md). hydo-build inserts this file and renamed copies
# _clk_orig_failed_ids / _clk_orig_mount_data into /sbin/init just before "main" -- ONLY for a
# manifest that sets it.
# Every serve boot starts its realtime clock at guest.clock_start, the recovery boot after a power
# cut included, so without this the recovery boot's clock starts BEFORE the times the first boot
# reached: a check that compares timestamps written in the two boots (a broker append time from the
# first boot against an engine time from the recovery boot) sees time run backwards. A real
# machine's clock keeps going across a power loss. So at a cut the first boot appends
# "clock=<epoch>" to its hydo-explore-acked line, which the harness (hvx_explore.c, oracle_cmd)
# copies into the recovery boot's "oracle" command unchanged; the recovery boot removes it from the
# acknowledged ids and starts its clock at that epoch plus clock_after_cut seconds (the time the
# machine is off). Deterministic: the epoch is the guest's own clock.
failed_ids() {
  _clk_orig_failed_ids "$@"
  [ "$cut" = 1 ] && echo "clock=$($BB date +%s)" >> $HYDO_ACKED   # only on the power-cut path
  return 0
}
mount_data() {
  local c
  case "$CMD" in
    oracle*)
      c=$(echo "$CMD" | $BB tr ' ' '\n' | $BB sed -n 's/^clock=//p' | $BB tail -1)
      $BB sed -i '/^clock=/d' $HYDO_ACKED $HYDO_FAILED 2>/dev/null
      case "$c" in ""|*[!0-9]*) c=$HYDO_CLOCK_START;; esac
      $BB date -s @$((c + HYDO_CLOCK_AFTER_CUT)) > /dev/null &&
        log "recovery boot: realtime clock set to $($BB date -u +%Y-%m-%dT%H:%M:%SZ) (first boot at the cut: $c, +${HYDO_CLOCK_AFTER_CUT}s)";;
  esac
  _clk_orig_mount_data "$@"
}
