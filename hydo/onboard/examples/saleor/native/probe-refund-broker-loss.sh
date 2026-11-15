#!/bin/sh
# A refund request whose Celery task is lost in the broker: Redis ends (SIGKILL, as a crash or an
# out-of-memory kill) while the task waits in its queue. Needs `sh stack.sh up` first, and an
# order paid through the payment app.
#
#   sh probe-refund-broker-loss.sh <order number> [amount]
#
# The worker is stopped (as a worker that is busy or restarting), the refund is requested, Redis
# is killed and started again, then the worker. The script waits 6 minutes (longer than Saleor's
# 5 retries of a request) and prints the payment's amounts, its events and the payment app's
# record.
set -eu
cd "$(dirname "$0")"
n=$1; amount=${2:-5.00}
P=${SALEOR_PROJECT:-hydosaleor}
dc() { docker compose -p "$P" -f ../docker-compose.yaml -f compose.payapp.yaml "$@"; }
amounts() {
  sh stack.sh sql "select 'payment', t.charged_value, t.refunded_value, t.refund_pending_value from payment_transactionitem t join order_order o on o.id = t.order_id where o.number = $n and t.app_identifier = 'hydo.payments'"
}
amounts
dc stop worker
sh stack.sh client refund rb "$n" "$amount" app
echo "--- Redis ends while the request waits in its queue"
dc kill -s KILL redis
dc up -d --wait redis
dc up -d worker
echo "--- waiting 6 minutes"
sleep 360
amounts
sh stack.sh sql "select 'event', e.type, e.amount_value, e.psp_reference, e.created_at from payment_transactionevent e join payment_transactionitem t on t.id = e.transaction_id join order_order o on o.id = t.order_id where o.number = $n and t.app_identifier = 'hydo.payments' order by e.created_at"
sh stack.sh sql "select 'delivery', d.status, d.event_type, d.created_at from core_eventdelivery d where d.event_type = 'transaction_refund_requested' order by d.created_at"
dc logs --no-log-prefix payapp | grep "PAYAPP.*refund" | tail -4
