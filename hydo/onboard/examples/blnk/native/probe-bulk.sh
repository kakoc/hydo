#!/bin/sh
# Finding 2 (atomic bulk): an atomic bulk in the default queued mode whose second item fails
# before it is queued, here because its reference is already used. The answer says that nothing
# was applied. The script then reads the first item's status and the balance it paid into.
#   sh probe-bulk.sh
# Prints `status APPLIED` and a balance of 5000 when the bug shows. Needs a running stack.
B=${B:-http://127.0.0.1:55001}
id() { sed -n 's/.*"'"$1"'":"\([^"]*\)".*/\1/p'; }
tag=$(date +%s)$$
L=$(curl -s -X POST "$B/ledgers" -d '{"name":"bulk"}' | id ledger_id)
nb() { curl -s -X POST "$B/balances" -d "{\"ledger_id\":\"$L\",\"currency\":\"USD\"}" | id balance_id; }
A=$(nb); C=$(nb); D=$(nb)
tx() { curl -s -o /dev/null -w "$2: %{http_code}\n" -X POST "$B/transactions" -d "{\"precise_amount\":$1,\"precision\":100,\"reference\":\"$2\",\"currency\":\"USD\",\"source\":\"$3\",\"destination\":\"$4\",\"description\":\"$2\"$5}"; }
tx 10000 "fund-$tag" @world "$A" ',"allow_overdraft":true'
tx 100 "used-$tag" "$A" "$D"
sleep 3
echo "-- atomic bulk: A->C 50.00 (new reference), A->D 1.00 (reference already used)"
curl -s -w ' [%{http_code}]\n' -X POST "$B/transactions/bulk" -d "{\"atomic\":true,\"transactions\":[{\"precise_amount\":5000,\"precision\":100,\"reference\":\"b1-$tag\",\"currency\":\"USD\",\"source\":\"$A\",\"destination\":\"$C\",\"description\":\"b\"},{\"precise_amount\":100,\"precision\":100,\"reference\":\"used-$tag\",\"currency\":\"USD\",\"source\":\"$A\",\"destination\":\"$D\",\"description\":\"b\"}]}"
sleep 5
echo "-- item 1 afterwards:"
curl -s "$B/transactions/reference/b1-${tag}_q" | sed -n 's/.*"status":"\([A-Z]*\)".*/status \1/p'
echo "-- balance C (0 if the bulk had no effect):"
curl -s "$B/balances/$C" | sed 's/,"version.*//'; echo
