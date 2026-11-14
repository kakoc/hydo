#!/bin/sh
# Finding 1 (overdraft_limit): N draws of 30.00 from a new balance, each with allow_overdraft and
# overdraft_limit 50.00. At most one may apply, because two take the balance to -60.00.
#   MODE=burst N=5 sh probe-limit.sh   # all draws at once: the bug
#   MODE=seq N=3 sh probe-limit.sh     # one draw per second: the documented behaviour
# Prints the answer codes, the source balance (precise, in cents), and each draw's status.
# Needs a running stack (sh stack.sh up).
B=${B:-http://127.0.0.1:55001}
N=${N:-5}
MODE=${MODE:-burst}
id() { sed -n 's/.*"'"$1"'":"\([^"]*\)".*/\1/p'; }
L=$(curl -s -X POST "$B/ledgers" -d '{"name":"limit"}' | id ledger_id)
nb() { curl -s -X POST "$B/balances" -d "{\"ledger_id\":\"$L\",\"currency\":\"USD\"}" | id balance_id; }
K=$(nb); D=$(nb)
tag=$(date +%s)$$
i=1
while [ $i -le "$N" ]; do
  body="{\"precise_amount\":3000,\"precision\":100,\"reference\":\"lim-$tag-$i\",\"currency\":\"USD\",\"source\":\"$K\",\"destination\":\"$D\",\"allow_overdraft\":true,\"overdraft_limit\":50,\"description\":\"draw\"}"
  if [ "$MODE" = seq ]; then
    curl -s -o /dev/null -w '%{http_code} ' -X POST "$B/transactions" -d "$body"; sleep 1
  else
    curl -s -o /dev/null -w '%{http_code} ' -X POST "$B/transactions" -d "$body" &
  fi
  i=$((i + 1))
done
wait
echo
sleep 4
curl -s "$B/balances/$K" | sed 's/,"ledger_id.*//'; echo
i=1
while [ $i -le "$N" ]; do
  curl -s "$B/transactions/reference/lim-$tag-${i}_q" | sed -n 's/.*"status":"\([A-Z]*\)".*/\1/p' | tr '\n' ' '
  i=$((i + 1))
done
echo
