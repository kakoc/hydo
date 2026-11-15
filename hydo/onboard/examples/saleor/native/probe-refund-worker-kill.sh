#!/bin/sh
# A refund request whose Celery worker dies while the payment app works on it. Needs
# `sh stack.sh up` first, and an order paid through the payment app.
#
#   sh probe-refund-worker-kill.sh <order number> [amount]
#
# The payment app is restarted with a 6 s delay; the refund is requested; 3 s later the worker
# container is killed and started again. The payment app refunds; the script then waits 6
# minutes (longer than Saleor's 5 retries of the request) and prints the payment's amounts, its
# events and the payment app's record.
set -eu
cd "$(dirname "$0")"
n=$1; amount=${2:-5.00}
P=${SALEOR_PROJECT:-hydosaleor}
dc() { docker compose -p "$P" -f ../docker-compose.yaml -f compose.payapp.yaml "$@"; }
amounts() {
  sh stack.sh sql "select 'payment', t.charged_value, t.refunded_value, t.refund_pending_value from payment_transactionitem t join order_order o on o.id = t.order_id where o.number = $n and t.app_identifier = 'hydo.payments'"
}
PAYAPP_DELAY=6 dc up -d payapp
sleep 3
amounts
sh stack.sh client refund rk "$n" "$amount" app
sleep 3
echo "--- killing the worker while the payment app works on the refund"
dc kill -s KILL worker
dc up -d worker
echo "--- waiting 6 minutes"
sleep 360
amounts
sh stack.sh sql "select 'event', e.type, e.amount_value, e.psp_reference, e.created_at from payment_transactionevent e join payment_transactionitem t on t.id = e.transaction_id join order_order o on o.id = t.order_id where o.number = $n and t.app_identifier = 'hydo.payments' order by e.created_at"
sh stack.sh sql "select 'delivery', d.status, d.event_type, d.created_at from core_eventdelivery d where d.event_type = 'transaction_refund_requested' order by d.created_at"
dc logs --no-log-prefix payapp | grep "PAYAPP.*refund" | tail -4
PAYAPP_DELAY=0 dc up -d payapp
