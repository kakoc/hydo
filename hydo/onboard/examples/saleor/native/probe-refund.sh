#!/bin/sh
# Two refunds of one order's payment, the second sent while the payment app still works on the
# first (sh stack.sh up first). Each asks for the whole charged amount.
#   sh probe-refund.sh <order number> [delay between the two, seconds] [app|gift]
# Prints the transaction's amounts afterwards and the payment app's record of refunds.
set -eu
cd "$(dirname "$0")"
n=$1; gap=${2:-0.5}; which=${3:-app}
P=${SALEOR_PROJECT:-hydosaleor}
gw=hydo.payments; [ "$which" = gift ] && gw=saleor.io.gift-card-payment-gateway
dc() { docker compose -p "$P" -f ../docker-compose.yaml -f compose.payapp.yaml "$@"; }
amount=$(sh stack.sh sql "select t.charged_value from payment_transactionitem t join order_order o on o.id = t.order_id where o.number = $n and t.app_identifier = '$gw'")
echo "order $n: charged $amount; two refunds of $amount, $gap s apart"
sh stack.sh client refund ra "$n" "$amount" "$which" > /tmp/hydo-saleor-ra.txt 2>&1 &
a=$!
sleep "$gap"
sh stack.sh client refund rb "$n" "$amount" "$which" > /tmp/hydo-saleor-rb.txt 2>&1 || true
wait "$a" || true
cat /tmp/hydo-saleor-ra.txt /tmp/hydo-saleor-rb.txt
sh stack.sh sql "select o.number, t.charged_value, t.refunded_value from payment_transactionitem t join order_order o on o.id = t.order_id where o.number = $n and t.app_identifier = '$gw'"
dc logs --no-log-prefix payapp | grep "PAYAPP refund" | tail -4
