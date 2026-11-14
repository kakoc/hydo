# shared by the commands and checks: the static test client for this machine's architecture
NJ=/bin/natsjs-$(uname -m)

# one HYDO-ASSERT record in the wire format of hydo's shell SDK (hydo_assert.sh): a `sometimes`
# property that held now. Written to the guest console, as the SDK does, so that a reaction
# (A=) sees it at once: a record on the command's own output reaches the report only when the
# command's log is collected, and reactions do not see it there.
assert_sometimes() {   # assert_sometimes <id> <message>
  printf 'HYDO-ASSERT {"t":"sometimes","id":"%s","ok":true,"msg":"%s"}\n' "$1" "$2" > /dev/console
}
