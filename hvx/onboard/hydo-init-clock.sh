# ---------------------------------------------------------------- recovery clock (hydo-init-clock.sh)
# guest.clock_after_cut (hvx/onboard/MANIFEST.md). hydo-build inserts this file and a renamed copy
# _clk_orig_mount_data into /sbin/init just before "main" -- ONLY for a manifest that sets it.
# Every serve boot starts its realtime clock at guest.clock_start, the recovery boot after a power
# cut included, so without this the recovery boot's clock starts BEFORE the times the first boot
# reached: a check that compares timestamps written in the two boots (a broker append time from the
# first boot against an engine time from the recovery boot) sees time run backwards. A real machine's
# clock keeps going across a power loss; this starts the recovery boot clock_after_cut seconds
# after clock_start instead (deterministic; choose more than the workload can run).
mount_data() {
  case "$CMD" in
    oracle*) $BB date -s @$((HYDO_CLOCK_START + HYDO_CLOCK_AFTER_CUT)) > /dev/null &&
               log "recovery boot: realtime clock set to $($BB date -u +%Y-%m-%dT%H:%M:%SZ) (clock_after_cut ${HYDO_CLOCK_AFTER_CUT}s)";;
  esac
  _clk_orig_mount_data "$@"
}
