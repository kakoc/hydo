# Shared by the commands and checks (sourced, not a command).
#
# The client is TigerBeetle's own REPL (`tigerbeetle repl --command=...`), which is built into
# the same static binary as the server: no client to build. A TigerBeetle client never gives up
# on a request (it retries until the cluster answers), so every call has a time limit, and a call
# that hits it is an unknown outcome.
#
# A transfer's id encodes what it moves, so a check can recompute it from the id alone:
#   id = ((cmd * 1000 + amount) * 10 + debit) * 10 + credit     (accounts 1..8, amount 1..999)
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
