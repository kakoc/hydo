#!/bin/sh
# The API process ends between the two commits of a gift card charge: Saleor deducts the card's
# balance in one database transaction and records the charge (the CHARGE_SUCCESS event and the
# payment's charged amount) after it. Needs `sh stack.sh up` first.
#
#   sh probe-giftcard-charge-crash.sh [gift card index 0-3]
#
# 1. a checkout paid half by a gift card (authorized) and half by the payment app, not completed;
# 2. psql session 1 holds the gift card's row (SELECT ... FOR UPDATE) for 10 s, so that the
#    charge, which runs when the order is confirmed right after checkoutComplete, waits there;
# 3. checkoutComplete starts; 5 s later psql session 2 takes the charge request event's row, so
#    that the charge's first write after its balance commit waits there;
# 4. session 1 ends: the charge deducts the balance and commits, then waits on session 2;
# 5. the API container is killed and started again; checkoutComplete is sent again.
# Prints the card, the order and the gift card payment's events before and after.
set -eu
cd "$(dirname "$0")"
card=${1:-1}
P=${SALEOR_PROJECT:-hydosaleor}
dc() { docker compose -p "$P" -f ../docker-compose.yaml -f compose.payapp.yaml "$@"; }
cl() { dc exec -T -e SALEOR_URL=http://localhost:8000/graphql/ "$@"; }
code=$(cl api python3 -c "import sys; sys.path.insert(0, '/hydo/workload'); import helper_client as c; print(c.gift_card_codes()[$card])")
seed=$(( 16807 * 1 + 117649 * card ))   # helper_client.checkout: a gift card pays half, card index $card
out=$(cl -e CLIENT_NO_COMPLETE=1 api python3 /hydo/workload/helper_client.py checkout gc "$seed")
echo "$out"
cid=$(echo "$out" | sed -n 's/^CHECKOUT //p')
token=$(echo "$out" | sed -n 's/^TOKEN //p')
show() {
  echo "--- $1"
  sh stack.sh sql "select 'card', code, initial_balance_amount, current_balance_amount from giftcard_giftcard where code = '$code'"
  sh stack.sh sql "select 'order', number, total_gross_amount, total_charged_amount, charge_status, status from order_order where checkout_token = '$token'"
  sh stack.sh sql "select 'event', e.type, e.amount_value from payment_transactionevent e join payment_transactionitem t on t.id = e.transaction_id where t.app_identifier = 'saleor.io.gift-card-payment-gateway' and t.gift_card_id = (select id from giftcard_giftcard where code = '$code') order by e.created_at"
}
show "paid, not completed"
dc exec -T db psql -U saleor -d saleor -c "BEGIN; SELECT id FROM giftcard_giftcard WHERE code = '$code' FOR UPDATE; SELECT pg_sleep(10); COMMIT;" > /tmp/hydo-saleor-lock1.txt 2>&1 &
l1=$!
sleep 1
cl -e CLIENT_DEADLINE=6 api python3 /hydo/workload/helper_client.py complete gc "$cid" > /tmp/hydo-saleor-gc1.txt 2>&1 &
first=$!
sleep 5
dc exec -T db psql -U saleor -d saleor -c "BEGIN; SELECT e.id FROM payment_transactionevent e JOIN payment_transactionitem t ON t.id = e.transaction_id WHERE e.type = 'charge_request' AND t.gift_card_id = (SELECT id FROM giftcard_giftcard WHERE code = '$code') FOR UPDATE OF e; SELECT pg_sleep(25); ROLLBACK;" > /tmp/hydo-saleor-lock2.txt 2>&1 &
l2=$!
wait "$l1" || true
sleep 6
echo "--- killing the API container while the charge waits after its balance commit"
dc kill -s KILL api
wait "$first" || true
cat /tmp/hydo-saleor-gc1.txt
wait "$l2" || true
dc up -d --wait api
show "after the API restarted"
echo "--- the storefront sends checkoutComplete again"
cl api python3 /hydo/workload/helper_client.py complete gc "$cid" || true
sleep 5
show "after the retry"
