# hydo-init-buggify.sh -- spliced into /sbin/init by hydo-build only for a manifest with
# `buggify: true` (the project's programs use an SDK's BUGGIFY fault points: Go hydoassert,
# Rust hydo-assert). The schedule token B=<seed-hex>[,<enable-permille>[,<fire-permille>]]
# reaches explore_run like a knob letter; write it where the SDKs read their configuration
# (/tmp/hydo-buggify, read once at a program's first fault-point evaluation). Without B= the
# file does not exist and every point stays off.
set_knob() {   # <letter> <value>
  if [ "$1" = B ]; then
    echo "$2" > /tmp/hydo-buggify
    $BB chmod 644 /tmp/hydo-buggify
    log "BUGGIFY $2 (/tmp/hydo-buggify)"
    return 0
  fi
  _bg_orig_set_knob "$@"
}
