# hydo_assert.sh -- hydo's assertion SDK for busybox sh scripts (guest /sbin/init and friends).
# Source it:  . /etc/hydo_assert.sh
# Wire format and semantics: hvx/ASSERTIONS.md. Records go to the console (/dev/ttyS0 unless
# HYDO_ASSERT_OUT names another file, or is "off"), one printf per record.
#
#   hydo_decl     <type> <id> <msg>             catalog entry (call once per property, early)
#   hydo_always   <id> <msg> <cond-cmd...>      evaluates "cond-cmd": exit 0 = holds
#   hydo_sometimes <id> <msg> <cond-cmd...>     records only when cond-cmd succeeds
#   hydo_reachable <id> <msg>                   this line was reached
#   hydo_unreachable <id> <msg>                 this line must never be reached (violation)
#   hydo_setup_complete                         lifecycle: provisioning done, faults may begin
#
# Example:
#   hydo_decl always data.mounted "the data disk mounts after recovery"
#   hydo_always data.mounted "the data disk mounts after recovery" mount -t ext4 /dev/vda2 /data
#
# Unlike the Go SDK this helper does not deduplicate: call it where an event happens once.

_hydo_out() {
  case "${HYDO_ASSERT_OUT:-/dev/ttyS0}" in
    off) return 0 ;;
    *) printf '%s\n' "$1" >> "${HYDO_ASSERT_OUT:-/dev/ttyS0}" 2>/dev/null || printf '%s\n' "$1" ;;
  esac
}

# JSON string escaping for the characters that can occur in ids and messages.
_hydo_q() {
  printf '%s' "$1" | ${BB:-/bin/busybox} sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

hydo_decl() {
  _hydo_out "HYDO-ASSERT-DECL {\"t\":\"$1\",\"id\":\"$(_hydo_q "$2")\",\"msg\":\"$(_hydo_q "$3")\"}"
}

_hydo_eval() {   # type id ok msg
  _hydo_out "HYDO-ASSERT {\"t\":\"$1\",\"id\":\"$(_hydo_q "$2")\",\"ok\":$3,\"msg\":\"$(_hydo_q "$4")\"}"
}

hydo_always() {
  id=$1; msg=$2; shift 2
  if "$@" >/dev/null 2>&1; then _hydo_eval always "$id" true "$msg"; return 0
  else _hydo_eval always "$id" false "$msg"; return 1; fi
}

hydo_sometimes() {
  id=$1; msg=$2; shift 2
  if "$@" >/dev/null 2>&1; then _hydo_eval sometimes "$id" true "$msg"; return 0; fi
  return 1
}

hydo_reachable()   { _hydo_eval reachable "$1" true "$2"; }
hydo_unreachable() { _hydo_eval unreachable "$1" false "$2"; }
hydo_setup_complete() { _hydo_out "HYDO-SETUP-COMPLETE"; }
