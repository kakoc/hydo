# Saleor without hydo

These scripts run Saleor 3.23.49 with plain Docker Compose, without hydo, to check outside hydo
what the hydo runs showed. They change nothing in Saleor: the images are Saleor's own, and the
stack is `../docker-compose.yaml` plus a payment app.

## The stack

```sh
sh stack.sh up        # start it (the migrations take about 3 minutes), then create the shop
sh stack.sh client checkout op1 42     # one purchase, as the hydo workload makes it
sh stack.sh sql "select number, total_gross_amount from order_order"
sh stack.sh down      # remove the containers and the volumes
```

- `compose.payapp.yaml` adds the payment app `payapp.py`, which answers every charge and refund
  with success, as the fake `payments` of `../hydo/hydo.yaml` does. `PAYAPP_DELAY=2` delays each
  answer by 2 seconds.
- `stack.sh up` runs the test client's `setup` (`../hydo/workload/helper_client.py`): the
  channel's settings, three variants with stock, three vouchers, four gift cards of 60.00, and
  the payment app with its webhooks.

## The probes

Each probe needs a stack that `stack.sh up` started. RESULTS.md, next to this folder, has the
output of each and what it shows.

| script | what it does |
|---|---|
| `probe-refund.sh <order> [gap] [app\|gift]` | two refunds of one payment, the second sent `gap` seconds after the first |
| `race_refund.py <order> <app\|gift> <amount> [n]` | n refunds of one payment at the same moment (run in the API container) |
| `probe-voucher-crash.sh [seed]` | the API process ends between the two transactions of `checkoutComplete`; the storefront retries |
| `probe-giftcard-detach.sh [card]` | a gift card that pays checkout A is authorized on checkout B while A completes |
| `probe-refund-worker-kill.sh <order> [amount]` | the Celery worker is killed while the payment app works on a refund request |
| `probe-refund-broker-loss.sh <order> [amount]` | Redis is killed while a refund request waits in Celery's queue |
| `probe-giftcard-charge-crash.sh [card]` | the API process ends between the two commits of a gift card charge |

`payapp.py` takes two settings for the probes: `PAYAPP_DELAY=<seconds>` delays every answer, and
`PAYAPP_LOSE=<route>` makes the first request of that route take effect and answer 500, as
`H=payments.<route>@...:status=500,effect=yes` does in hydo.

## The lost refund request, step by step

This is RESULTS.md's finding 1, without hydo:

```sh
sh stack.sh up
sh stack.sh client checkout op1 42             # an order paid by the payment app; note its number
sh probe-refund-worker-kill.sh <number> 5.00   # about 7 minutes
sh probe-refund-broker-loss.sh <number> 5.00   # about 7 minutes
```

Each probe prints the payment's charged, refunded and pending amounts before and after, the
payment's events, and the payment app's record of refunds. A refund that Saleor handles prints
`refund_success` and a refunded amount; a lost one prints a `refund_request` event with nothing
after it, and `pending` for its webhook delivery.

## The gift card charged without a record, step by step

This is RESULTS.md's finding 4, without hydo:

```sh
sh stack.sh up
sh probe-giftcard-charge-crash.sh 1            # about 2 minutes
```

The probe uses two row locks only to stop the charge at a known point; Saleor's code is
unchanged. It prints the card's balance, the order's charged amount and status, and the gift
card payment's events after the API's death and after the storefront's retry. A charge that
Saleor records prints `charge_success`; here the card's balance drops by the charge, and the
payment keeps `charge_request` with nothing after it.
