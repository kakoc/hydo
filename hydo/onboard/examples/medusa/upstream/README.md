# Two concurrent refunds duplicate an order's items: a reproduction without hydo

This folder shows finding 1 of `../RESULTS.md` with plain Docker Compose and no faults. Two
`POST /admin/payments/:id/refund` requests for the payment of one order are sent at the same
moment, as two operators (or one double click) would. In some trials Medusa accepts both, creates
two order changes with the same order version, and writes the order's line item twice at that
version. From then on the order lists its one item twice.

The stack is Medusa's production setup without Stripe: PostgreSQL 15, Redis 7 (event bus, workflow
engine and locking), and the backend built from `medusa-starter-default` with the built-in system
payment provider (`pp_system_default`), so no payment account is needed.

## Files

| file | what it is |
|---|---|
| `Dockerfile` | the backend: `medusa-starter-default` at the commit that pins Medusa 2.18.0, its `@medusajs/*` versions replaced by `MEDUSA_VERSION`, `medusa build` |
| `medusa-config.ts` | the starter's configuration with Redis modules and the admin dashboard off |
| `seed.ts` | a region paid through `pp_system_default`, a warehouse, a shipping option, a publishable key and one product with 1000 units |
| `docker-compose.yaml` | PostgreSQL, Redis and the backend; `race.mjs` is mounted into the backend container |
| `race.mjs` | one control order refunded twice in sequence, then N trials: a new order (one unit), its payment captured, two refunds of 10 sent together, the order read back |
| `run.sh` | builds and starts the stack, seeds it, creates an admin user, runs `race.mjs`, and lists the duplicate rows from PostgreSQL |

## Run it

```sh
MEDUSA_VERSION=2.18.0 sh run.sh 20     # Docker with Compose; the first build takes several minutes
docker compose down -v                 # remove the containers and the database
```

## Output

On 2026-11-08, Docker Desktop 28.4.0 on an Apple Silicon Mac (arm64 images), Node.js 22.23.4,
PostgreSQL 15.19. The refusal text is cut short in the lines below; in full it is `400
{"type":"invalid_data","message":"Order (order_…) already has an existing active order
change"}`.

Medusa 2.18.0, 5 of 20 trials corrupted (a first run on a fresh stack gave 7 of 20):

```
control (sequential): order_01M6WZ3R7HMDWJNJH6XBVW74HT refunds=[ok | ok] version=3 items=[["ordli_01M6WZ3R7MR87AZGTPXEGT1ZJ0",1]] units=1
trial 1: order_01M6WZ3VHXN2JSSPD2PH6WB532 refunds=[refused: 400 … | ok] version=2 items=[["ordli_01M6WZ3VHXX9GT4V7PWCRPYVGX",1]] units=1
trial 5: order_01M6WZ48BDZD27YM12JTZHNTM3 refunds=[ok | ok] version=2 items=[["ordli_01M6WZ48BEZA52MYW32VV9KD5Q",1],["ordli_01M6WZ48BEZA52MYW32VV9KD5Q",1]] units=2  <-- CORRUPTED: 1 unit was ordered
…
5 of 20 orders corrupted by two concurrent refunds
== order versions with more than one order change
             order_id             | version | changes |          types
----------------------------------+---------+---------+-------------------------
 order_01M6WZ48BDZD27YM12JTZHNTM3 |       2 |       2 | credit_line,credit_line
 … (5 rows)
== order items written twice at one version
             order_id             |             item_id              | version | rows
----------------------------------+----------------------------------+---------+------
 order_01M6WZ48BDZD27YM12JTZHNTM3 | ordli_01M6WZ48BEZA52MYW32VV9KD5Q |       2 |    2
 … (5 rows)
```

The control shows what Medusa does when the two refunds come one after the other: both are
accepted, the order reaches version 3, and it keeps one item. In a concurrent trial either one
refund is refused with "already has an existing active order change" (Medusa's guard worked), or
both are accepted and the order is corrupted.

Medusa 2.22.4, the newest release on 2026-11-08 (`MEDUSA_VERSION=2.22.4 sh run.sh 20`): both
refunds were accepted in 10 of 20 trials, and PostgreSQL holds two `credit_line` order changes at
version 2 for each of those 10 orders. In 9 of them the line item is also written twice; in the
tenth (trial 12) the order still lists one item.

```
control (sequential): order_01M6WZH0WEWDG7423GWC6E6JTK refunds=[ok | ok] version=3 items=[["ordli_01M6WZH0WHWDQ81586HMDK4MRS",1]] units=1
trial 1: order_01M6WZH4803FECCP772WG448VV refunds=[ok | ok] version=2 items=[["ordli_01M6WZH4807E33CNZG04BV5EYY",1],["ordli_01M6WZH4807E33CNZG04BV5EYY",1]] units=2  <-- CORRUPTED: 1 unit was ordered
trial 2: order_01M6WZH6ZNZ8HRFVYMS71VP3SN refunds=[ok | refused: 400 … ] version=2 items=[["ordli_01M6WZH6ZPDA61T9YW105RDKC5",1]] units=1
…
trial 12: order_01M6WZHW1B6F6NPVNX9AYVX45M refunds=[ok | ok] version=2 items=[["ordli_01M6WZHW1D88EVH2SW1BSWV4ZT",1]] units=1
…
9 of 20 orders corrupted by two concurrent refunds
== order versions with more than one order change      (10 rows, all "credit_line,credit_line" at version 2)
== order items written twice at one version            (9 rows)
```
