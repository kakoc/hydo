# Lago without hydo: the lost termination invoice and the double invoice

This folder reproduces two Lago bugs of [`../RESULTS.md`](../RESULTS.md) without hydo, on Lago
v1.57.0 in Docker:

1. Finding 1. A subscription cancelled while Redis is down is terminated and never invoiced. The
   API saves the termination and then fails to enqueue the invoice job, which needs Redis for
   its uniqueness lock. Upstream: lago-api issue 6901, confirmed on v1.57.0 in
   https://github.com/getlago/lago-api/issues/6901#issuecomment-6864157424.
2. Finding 8. A subscription whose cancellation is not committed yet at the hourly billing run
   is invoiced twice for one period: the periodic invoice and the termination invoice. No
   upstream issue describes it.

## Prerequisites

- Docker with Compose v2. The scripts use the images of `compose.yaml`: `getlago/api:v1.57.0`,
  `getlago/postgres-partman:15.0-alpine` and `redis:7.4.7-alpine`.
- `curl` and `python3` (the standard library only).
- The port 13000 on 127.0.0.1 free.

## Steps for finding 1

1. Run the reproduction:

   ```sh
   sh repro-6901.sh
   ```

The script starts Lago as the Compose project `hydo-native-lago-6901` and waits for the API. Then
it sends these requests with `lago.py`:

1. Create a billable metric, a monthly plan in arrears, and two customers `cust1` and `cust2`.
   Each customer has a calendar subscription and one event of 12 units.
2. Cancel `sub-cust2` with Redis up, as a control.
3. Stop Redis, cancel `sub-cust1`, and start Redis.
4. Cancel `sub-cust1` again.
5. Read `sub-cust1` and the invoices of `cust1`. Read the invoices again 60 s later.

At the end it removes the project with its volumes. `KEEP=1` leaves it running.

## The expected outcome of finding 1

The run of 2026-11-14 printed:

```text
DELETE sub-cust2: HTTP 200 after 0.2 s, status=terminated terminated_at=2026-11-14T06:21:19Z
cust2 finalized subscription created=2026-11-14T06:21:19Z total=153 reason=
DELETE sub-cust1: HTTP 500 after 5.9 s, status=None terminated_at=None Internal Server Error
DELETE sub-cust1: HTTP 404 after 0.0 s, status=None terminated_at=None subscription_not_found
GET sub-cust1: HTTP 200, status=terminated terminated_at=2026-11-14T06:21:39Z
cust1: no invoice
```

The control gets its termination invoice. `sub-cust1` is terminated and has no invoice, also 60
s later. RESULTS.md records the same five answers from the run of 2026-11-13.

## Steps for finding 8

The bug needs the termination's transaction to stay open from before midnight in the customer's
time zone until after the billing run at minute 10 of the next hour. A TCP proxy between the API
and PostgreSQL, `hold-proxy.rb`, does that. When it is armed and sees the termination's `UPDATE
"subscriptions" ... "terminated_at"`, it holds the database's answers on that connection. The
hold stands for a network outage that does not break the connection.

1. Choose a time zone of Rails' list and the UTC time that is 23:56 there. In November,
   `Atlantic/Azores` is UTC-1, so 23:56 there is 00:56 UTC.
2. Run the reproduction at least 5 minutes before that time:

   ```sh
   sh double-invoice.sh Atlantic/Azores 00:56:00
   ```

The script starts Lago with `compose.hold.yaml` as the Compose project `hydo-native-lago-dbl`.
It creates five customers with monthly anniversary subscriptions whose period ends at that
midnight. It sets their time zone in the database, because the API accepts a time zone only
with a license. Then `experiment.py` cancels `held1`, `held2` and `held3` at 23:56 local, 10 s
apart, with the answers held for 16 minutes. It cancels `ctrl1` 60 s later without a hold, and
leaves `plain1` alone. At the end the script prints every customer's invoices and removes the
project, its volumes and the proxy's image. The whole run takes about 25 minutes after the start.

## The expected outcome of finding 8

The run of 2026-11-14 used `Atlantic/Azores`, with the cancellations at 00:56:01, 00:56:11 and
00:56:21 UTC. The three held subscriptions got two finalized invoices each, of 11.20 USD:

| subscription | invoices |
|---|---|
| `held1`, `held2`, `held3` | the periodic invoice at 01:10:00 UTC for the whole period, and the termination invoice at 01:12:01 UTC for the period up to 23:56 local |
| `ctrl1` | the periodic invoice, and a termination invoice of 0.33 USD for 01:00 to 01:12 UTC (correct) |
| `plain1` | the periodic invoice (correct) |

`ctrl1` was cancelled without a hold, but the API's three threads were busy with the held
requests, so its cancellation ran at 01:12:01. With the answers held across the billing run,
3 of 3 subscriptions were invoiced twice.

These scripts are the ones of that run. Three things changed: the Compose project's name, the
proxy's image, which Compose now names, and the seed and time-zone steps, which
`double-invoice.sh` now runs.

`double-invoice.sh` ran end to end on 2026-11-14 with `America/Phoenix` (UTC-7, so 23:56 there
is 06:56 UTC). It gave the same result. `held1`, `held2` and `held3` each got two finalized
invoices of 11.20 USD: the periodic one at 07:10:00 UTC and the termination one at 07:12:01 UTC.
`ctrl1` got the periodic invoice and one of 0.33 USD, and `plain1` the periodic invoice.
