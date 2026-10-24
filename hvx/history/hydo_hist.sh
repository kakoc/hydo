# hydo_hist.sh -- hydo's operation-history recorder for busybox sh workloads (hvx/HISTORIES.md).
# A manifest with checks: gets these functions in its prelude (every workload, retry, check and
# invariant command sees them); other scripts source this file.
#
#   hydo_hist <history> <process> <type> <f> [<arg>...]   one record, one write()
#       type: invoke | ok | fail (definitely had no effect) | info (outcome unknown)
#   hydo_hist_enc <string>                                 a value as one token (%xx escapes)
#
# Record the invocation BEFORE sending the request and the completion AFTER the reply: the host
# orders operations by the position of their records in the console, so this keeps real-time
# order. One process (a client, a session) has at most one operation in flight; use a new
# process id for concurrent clients.
#
#   hydo_hist kv 0 invoke write k1 v1
#   if out=$(redis-cli SET k1 v1) && [ "$out" = OK ]; then hydo_hist kv 0 ok write k1 v1
#   else hydo_hist kv 0 info write k1 v1; fi           # a timeout: it may or may not have happened
#   hydo_hist kv 0 invoke read k1
#   v=$(redis-cli GET k1) && hydo_hist kv 0 ok read k1 "${v:-nil}" || hydo_hist kv 0 info read k1
#
# Records go to the console (/dev/console), unless HYDO_HIST_OUT names another file or is "off".
# The time field is guest time since boot in ns (from /proc/uptime, 10 ms resolution; under
# hydo deterministic); the checkers do not use it for ordering.

hydo_hist() {   # no subshell, one echo = one write()
  local _hh=$1 _hp=$2 _hy=$3 _hu _hi _ht
  [ "${HYDO_HIST_OUT:-}" = off ] && return 0
  read _hu _hi 2>/dev/null < /proc/uptime || _hu=-
  case "$_hu" in *.*) _ht=${_hu%.*}${_hu#*.}0000000;; *) _ht=-;; esac
  shift 3
  echo "HYDO-HIST $_hh $_hp $_hy $_ht $*" >> "${HYDO_HIST_OUT:-/dev/console}" 2>/dev/null
}

hydo_hist_enc() {   # the empty string is the token %
  [ -n "$1" ] || { printf '%%'; return 0; }
  printf '%s' "$1" | ${BB:-/bin/busybox} sed -e 's/%/%25/g' -e 's/ /%20/g' -e 's/@/%40/g' \
    -e 's/,/%2C/g' -e 's/\[/%5B/g' -e 's/\]/%5D/g' -e 's/:/%3A/g' -e 's/	/%09/g'
}
