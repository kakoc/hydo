# Shared by the commands and checks (sourced, not a command).
#
# The client is TigerBeetle's own REPL (`tigerbeetle repl --command=...`), which is built into
# the same static binary as the server: no client to build. A TigerBeetle client never gives up
# on a request (it retries until the cluster answers), so every call has a time limit, and a call
# that hits it is an unknown outcome.
#
# A transfer's id encodes what it moves, so a check can recompute it from the id alone:
#   id = ((cmd * 1000 + amount) * 10 + debit) * 10 + credit     (accounts 1..8, amount 1..999)
#
# A hold is a pending transfer with a timeout of 1, 2 or 3 seconds: it reserves the amount
# (debits_pending, credits_pending) until it is posted, voided, or expires. Its id also encodes
# its place k in its batch and its timeout:
#   hold = 100000000000 + ((((cmd * 10 + k) * 10 + timeout) * 1000 + amount) * 10 + debit) * 10 + credit
# The post of a hold is 200000000000 + (hold - 100000000000), its void 300000000000 + (the same),
# so their last five digits are the hold's amount and accounts too.
TB_HOLD=100000000000
TB=${TB:-/opt/tb/tigerbeetle}
TB_ADDRESSES=${TB_ADDRESSES:-10.89.0.11:3001,10.89.0.12:3002,10.89.0.13:3003}
TB_ACCOUNTS=8

# tb <seconds> <statement>: one REPL call; its JSON on stdout, nothing on a timeout
tb() {
  timeout "$1" $TB repl --cluster=0 --addresses="$TB_ADDRESSES" --command="$2" 2>/dev/null
}

# tb_create <seconds> <create_accounts|create_transfers statement>: one status word per event,
# in order, or nothing when the call timed out. The REPL's answer changed in 0.17: 0.17 prints
# a JSON object with "status": "...Status.<word>" for every event; 0.16 prints nothing for an
# event that succeeded and "Failed to create ... (<i>): ...Result.<word>." for one that did not.
tb_create() {
  _n=$(printf '%s\n' "$2" | awk -F, '{print NF}')
  _out=$(tb "$1" "$2") || return 0
  printf '%s\n' "$_out" | awk -v n="$_n" '
    /Status\.[a-z_]+"/ { s = $0; sub(/.*Status\./, "", s); sub(/".*/, "", s); new[++k] = s }
    /^Failed to create/ { i = $0; sub(/.*\(/, "", i); sub(/\).*/, "", i)
                          s = $0; sub(/.*Result\./, "", s); sub(/\.$/, "", s); old[i] = s }
    END { if (k) { for (j = 1; j <= k; j++) print new[j] }
          else for (j = 0; j < n; j++) print ((j in old) ? old[j] : "created") }'
}

# transfer_of <seed> <cmd> -> "id debit credit amount"
transfer_of() {
  awk -v s="$1" -v c="$2" -v n=$TB_ACCOUNTS 'BEGIN {
    dr = 1 + s % n; cr = 1 + int(s / n) % (n - 1); if (cr >= dr) cr++
    amt = 1 + int(s / 64) % 999
    printf "%d %d %d %d\n", ((c * 1000 + amt) * 10 + dr) * 10 + cr, dr, cr, amt }'
}

# flat <REPL JSON file> a|t: one line per object.
#   a (accounts):  id debits_posted credits_posted debits_pending credits_pending
#   t (transfers): id debit credit amount flags pending_id timestamp timeout
# flags is the REPL's list without quotes: [], [pending], [post_pending_transfer], ...
flat() {
  awk -v want="$2" '
    /"id":/ { gsub(/[",]/, "", $2); id = $2 }
    { k = $1; gsub(/[":]/, "", k); v = $2; gsub(/[",]/, "", v); f[k] = v }
    /^}/ { if (want == "a") print id, f["debits_posted"], f["credits_posted"], f["debits_pending"], f["credits_pending"];
           else print id, f["debit_account_id"], f["credit_account_id"], f["amount"], f["flags"], f["pending_id"], f["timestamp"], f["timeout"]
           delete f }' "$1"
}

# holds_of <seed> <cmd> -> 1..8 lines "id debit credit amount timeout", one per hold of a batch,
# drawn from the seed (a MINSTD stream: exact in awk's doubles)
holds_of() {
  awk -v s="$1" -v c="$2" -v n=$TB_ACCOUNTS -v H=$TB_HOLD '
    function r(m) { x = (x * 48271) % 2147483647; return x % m }
    BEGIN {
      x = 1 + s % 2147483646; k = 1 + r(8)
      for (i = 0; i < k; i++) {
        dr = 1 + r(n); cr = 1 + r(n - 1); if (cr >= dr) cr++
        amt = 1 + r(999); t = 1 + r(3)
        printf "%.0f %d %d %d %d\n", H + ((((c * 10 + i) * 10 + t) * 1000 + amt) * 10 + dr) * 10 + cr, dr, cr, amt, t
      } }'
}

# outcomes <ids file> <statuses file>: record each id's outcome (one status per id, in order);
# exit status 100 when TigerBeetle says an id exists with other fields (this client never sends
# that), else 0
outcomes() {
  awk -v acked="$HYDO_ACKED" -v outc="$HYDO_OUTCOMES" '
    NR == FNR { st[FNR] = $1; next }
    { s = st[FNR]
      if (s == "created" || s == "exists") { print $1 >> acked; print $1 " ack" >> outc }
      else { print $1 " failed" >> outc; if (s ~ /^exists_with_different/) bad = 1 }
      print "  " $1 ": " s }
    END { exit bad ? 100 : 0 }' "$2" "$1"
}
