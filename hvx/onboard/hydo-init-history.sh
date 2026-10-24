# ---------------------------------------------------------------- history checks (hydo-init-history.sh)
# Ready-made checkers for recorded operation histories (`checks:` in hydo.yaml;
# hvx/HISTORIES.md). hydo-build inserts this file, plus renamed copies _hist_orig_<fn> of the
# two init functions wrapped below, into /sbin/init just before "main" -- ONLY for a manifest
# with checks:. Every other image keeps its /sbin/init byte for byte.
#
# The checks themselves run on the HOST after the run (hvx/history/hydohist.py, called by
# explore.py, `hydo run` and hvx/history/hydo-check). The guest only
#   - prints one HYDO-CHECK line per check when the workload starts (the host reads the
#     check's spec from the console, so a saved console is self-describing), and
#   - at the end -- in the same boot, or in the recovery boot after a power cut -- runs each
#     check's `final:` command (typically: read every key back and record the reads in the
#     history), then the original finish (invariants, verdict line).
# The workload records operations with hydo_hist (hvx/history/hydo_hist.sh, in the prelude).

cv() { eval "printf '%s' \"\${HYDO_CHECK_${1}_$2}\""; }      # cv <check> <field>

hist_declare() {   # once per boot, at the start of the workload
  local c
  [ -f $H/hist.declared ] && return 0
  : > $H/hist.declared
  for c in $HYDO_CHECKS; do echo "HYDO-CHECK $(cv $c json)"; done
}

hist_final() {     # each check's final: read-back, bounded by workload.timeout
  local c f rc
  for c in $HYDO_CHECKS; do
    f=$(cv $c final)
    [ -n "$f" ] || continue
    $BB timeout $HYDO_WORKLOAD_TIMEOUT $BB sh -c ". /etc/hydo/prelude.sh; $f" >> $H/workload.log 2>&1
    rc=$?
    log "check $(cv $c id): final read rc=$rc (uptime=$(UP)s)"
  done
}

# the wrapped init functions (originals: _hist_orig_<fn>, copied by hydo-build)
start_periodic() { _hist_orig_start_periodic "$@"; hist_declare; }
finish() { hist_final; _hist_orig_finish "$@"; }

