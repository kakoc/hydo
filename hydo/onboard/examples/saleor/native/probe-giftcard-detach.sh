#!/bin/sh
# A gift card pays checkout A in full. While checkoutComplete of A runs, the same card is
# authorized on checkout B (as the same customer does in a second browser tab). Needs
# `sh stack.sh up` first.
#
#   sh probe-giftcard-detach.sh [gift card index 0-3]
#
# To make "while A completes" repeatable, a psql session holds a share lock on the order table
# for 20 s, so that A's order-creating transaction waits at its insert of the order (after every
# check of the checkout); B's transactionInitialize runs
# during that wait. Then A completes. Prints A's order, its gift card payment, and the card.
set -eu
cd "$(dirname "$0")"
card=${1:-3}
P=${SALEOR_PROJECT:-hydosaleor}
dc() { docker compose -p "$P" -f ../docker-compose.yaml -f compose.payapp.yaml "$@"; }
py() { dc exec -T -e SALEOR_URL=http://localhost:8000/graphql/ api python3 /native/giftcard_steps.py "$@"; }
a=$(py prepare a "$card")
echo "$a"
acid=$(echo "$a" | sed -n 's/^CHECKOUT //p')
b=$(py prepare-unpaid b)
echo "$b"
bcid=$(echo "$b" | sed -n 's/^CHECKOUT //p')
sh stack.sh sql "select 'card before', code, current_balance_amount from giftcard_giftcard order by id"
dc exec -T db psql -U saleor -d saleor -c "BEGIN; LOCK TABLE order_order IN SHARE MODE; SELECT pg_sleep(20); COMMIT;" > /tmp/hydo-saleor-lock.txt 2>&1 &
lock=$!
sleep 2
py finish "$acid" > /tmp/hydo-saleor-a.txt 2>&1 &
first=$!
sleep 8
echo "--- while A completes: the same card on checkout B"
py authorize "$bcid" "$card"
wait "$first" || true
wait "$lock" || true
echo "--- A's completion:"
cat /tmp/hydo-saleor-a.txt
sleep 3
num=$(sed -n 's/^ORDER \([0-9]*\).*/\1/p' /tmp/hydo-saleor-a.txt)
[ -n "$num" ] || exit 1
sh stack.sh sql "select 'order', number, total_gross_amount, total_charged_amount, total_authorized_amount, charge_status, authorize_status, status from order_order where number = $num"
sh stack.sh sql "select 'payment', t.app_identifier, t.gift_card_id, t.authorized_value, t.charged_value, t.canceled_value from payment_transactionitem t join order_order o on o.id = t.order_id where o.number = $num"
sh stack.sh sql "select 'event', e.type, e.amount_value, left(e.message, 90) from payment_transactionevent e join payment_transactionitem t on t.id = e.transaction_id join order_order o on o.id = t.order_id where o.number = $num order by e.created_at"
sh stack.sh sql "select 'card after', code, current_balance_amount from giftcard_giftcard order by id"
