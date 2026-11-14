# Formance Ledger without hydo: two first deposits in two assets

This folder reproduces finding 1 of [`../RESULTS.md`](../RESULTS.md) without hydo. Formance Ledger
v2.4.18 fails one of two transactions with `500 INTERNAL` when both are the first to use a new
account, at the same moment, in two different assets. The ledger logs `duplicate key value
violates unique constraint "accounts_ledger" (SQLSTATE 23505)`. v2.3.24 accepts both.

The upstream report is https://github.com/formancehq/ledger/issues/2542.

## Prerequisites

- Docker. The script pulls `postgres:16.16-alpine3.24`, the PostgreSQL 16 of the ledger's own
  `docker-compose.yml`.
- Git, `curl`, and Go. `build-ledger.sh` sets `GOTOOLCHAIN=go1.27.3`, so Go fetches that
  version once.
- The ports 55432 and 3068 on 127.0.0.1 free, or `PG_PORT` and `LEDGER_PORT` set to others.

## Steps

1. Build the release with the bug and the one before it, for this machine:

   ```sh
   sh build-ledger.sh v2.4.18
   sh build-ledger.sh v2.3.24
   ```

2. Send 200 pairs of deposits to v2.4.18, one in `USD/2` and one in `EUR/2` to each new account:

   ```sh
   sh pairs.sh work/ledger-v2.4.18 v2418-two-assets two-assets
   ```

3. Send the same pairs with both deposits in `USD/2`:

   ```sh
   sh pairs.sh work/ledger-v2.4.18 v2418-one-asset one-asset
   ```

4. Send the two-asset pairs to v2.3.24:

   ```sh
   sh pairs.sh work/ledger-v2.3.24 v2324-two-assets two-assets
   ```

Each run of `pairs.sh` starts a new PostgreSQL container, `hydo-native-formance-pg`, and the
ledger on this machine. It creates the ledger `main` and sends the pairs with `curl`. Then it
reads the accounts from PostgreSQL and removes the container. The answer codes, the ledger's log
and the summary line are in `work/runs/<label>/`.

The mode `meta` sends one deposit with the account metadata `a=1`, and the other dated one hour
back with `b=2`. Its summary also counts the accounts that hold both metadata values and the
accounts whose `first_usage` is the earlier date.

## The expected outcome

On v2.4.18 almost every two-asset pair has one `500`, and the log has one `duplicate key` line
for each. The summary of step 2 on 2026-11-14:

```text
v2418-two-assets: mode=two-assets pairs=200 one500=197 two200=3 other=0 shop_accounts=200 volumes=[EUR/2|108|10800 USD/2|95|9500] duplicate_key_log_lines=197
```

The volumes show the lost deposits. Of the 200 new accounts, 108 hold the `EUR/2` deposit and 95
hold the `USD/2` one.

Steps 3 and 4 give `one500=0 two200=200`, with every deposit in the volumes.

## The measured rates

| ledger | the two requests of a pair | pairs with one `500` |
|---|---|---|
| v2.4.18 | `USD/2` and `EUR/2` | 192 and 196 of 200 (RESULTS.md); 194 and 197 of 200 with these scripts on 2026-11-14 |
| v2.4.18 | both in `USD/2` | 0 of 200 |
| v2.4.18 | the mode `meta` | 193 of 200 on 2026-11-14 |
| v2.3.24 | `USD/2` and `EUR/2` | 0 of 200 |

All runs were on an Apple M2 Pro with macOS 26.4.1, with PostgreSQL in Docker.
