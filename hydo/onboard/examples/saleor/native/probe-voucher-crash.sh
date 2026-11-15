#!/bin/sh
# checkoutComplete stops between its two database transactions: the API process ends (as a pod
# that is killed or redeployed) while the order-creating transaction waits for a lock.
# Needs `sh stack.sh up` first; uses a single-use voucher code (ONE-C by default).
#
#   sh probe-voucher-crash.sh [seed]
#
# 1. a checkout with a single-use code, fully paid through the payment app (not completed);
# 2. a psql session holds the stock rows (SELECT ... FOR UPDATE) for 40 s, so that the order's
#    transaction waits at its stock allocation;
# 3. checkoutComplete starts; 10 s later the API container is killed and started again;
# 4. after the lock is gone, the client sends checkoutComplete again, as a storefront retries.
# Prints the code, the checkout and the orders before and after the retry.
set -eu
cd "$(dirname "$0")"
seed=${1:-12005}
P=${SALEOR_PROJECT:-hydosaleor}
dc() { docker compose -p "$P" -f ../docker-compose.yaml -f compose.payapp.yaml "$@"; }
cl() { dc exec -T -e SALEOR_URL=http://localhost:8000/graphql/ "$@"; }
state() {
  echo "--- $1"
  sh stack.sh sql "select 'code', code, is_active, used from discount_vouchercode where code like 'ONE-%' order by code"
  sh stack.sh sql "select 'checkout', token, voucher_code, is_voucher_usage_increased, total_gross_amount, charge_status from checkout_checkout where token = '$token'"
  sh stack.sh sql "select 'order', number, voucher_code, total_gross_amount, total_charged_amount, charge_status from order_order where checkout_token = '$token'"
  sh stack.sh sql "select 'charged', sum(charged_value) from payment_transactionitem where checkout_id = '$token' or order_id in (select id from order_order where checkout_token = '$token')"
}
out=$(cl -e CLIENT_NO_COMPLETE=1 api python3 /hydo/workload/helper_client.py checkout vc "$seed")
echo "$out"
cid=$(echo "$out" | sed -n 's/^CHECKOUT //p')
token=$(echo "$out" | sed -n 's/^TOKEN //p')
state "paid, not completed"
dc exec -T db psql -U saleor -d saleor -c "BEGIN; SELECT id FROM warehouse_stock FOR UPDATE; SELECT pg_sleep(40); COMMIT;" > /tmp/hydo-saleor-lock.txt 2>&1 &
lock=$!
sleep 2
cl -e CLIENT_DEADLINE=8 api python3 /hydo/workload/helper_client.py complete vc "$cid" > /tmp/hydo-saleor-complete1.txt 2>&1 &
first=$!
sleep 10
echo "--- killing the API container during checkoutComplete"
dc kill -s KILL api
wait "$first" || true
cat /tmp/hydo-saleor-complete1.txt
dc up -d --wait api
wait "$lock" || true
state "after the API restarted"
echo "--- the storefront sends checkoutComplete again"
cl api python3 /hydo/workload/helper_client.py complete vc "$cid" || true
state "after the retry"
