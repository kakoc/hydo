# Blnk without hydo: the overdraft limit and the atomic bulk

This folder reproduces findings 1 and 2 of [`../RESULTS.md`](../RESULTS.md) without hydo, on
Blnk at commit `67d88333` (main of 2026-11-11):

1. Queued draws from one balance with `allow_overdraft` and an `overdraft_limit` ignore the limit
   when they arrive together. Five draws of 30.00 against a limit of 50.00 all apply, and the
   balance ends at -150.00. Upstream: https://github.com/blnkfinance/blnk/issues/437.
2. An atomic bulk in the default queued mode applies its first transfer when a later transfer
   fails before it is queued. The answer says that nothing was applied. Upstream:
   https://github.com/blnkfinance/blnk/issues/438.

## Prerequisites

- Docker. The scripts pull `postgres:16.16-alpine3.24` and `redis:7.2.4`.
- Git, `curl`, and Go. `build-blnk.sh` sets `GOTOOLCHAIN=go1.27.3`, so Go fetches that version
  once.
- The ports 55432, 56379, 55001, and 55004 on 127.0.0.1 free.

## Steps

1. Build Blnk for this machine:

   ```sh
   sh build-blnk.sh
   ```

2. Measure both findings, 20 times each, on a fresh stack:

   ```sh
   sh measure.sh
   ```

`measure.sh` runs `stack.sh up`, which starts two containers, `hydo-native-blnk-pg` and
`hydo-native-blnk-redis`. Then it runs `blnk migrate up`, `blnk start` and `blnk workers` on
this machine, with their logs in `work/`. At the end it runs `stack.sh down`, which stops Blnk
and removes both containers.

To see one case at a time, start the stack and run a probe:

```sh
sh stack.sh up
MODE=burst N=5 sh probe-limit.sh     # five draws at once
MODE=seq N=3 sh probe-limit.sh       # one draw per second
sh probe-bulk.sh
sh stack.sh down
```

## The expected outcome

`probe-limit.sh` prints the source balance in cents. At most one draw of 3000 may apply, so the
balance must not go below -3000. With the draws sent at once it usually ends at -15000 or -9000,
and the draws show `APPLIED`. With one draw per second the first applies and the others end
`REJECTED` with `transaction exceeds overdraft limit`, as Blnk's overdraft page says.

`probe-bulk.sh` prints the bulk's answer: `409`, the code `TXN_DUPLICATE_REFERENCE`, and the
message "Nothing from this batch had been applied." A few seconds later the first transfer shows `status APPLIED`, and
the balance it paid into holds 5000.

## The measured rates

| run | bursts below -50.00 | bulks with the first transfer applied |
|---|---|---|
| RESULTS.md, 2026-11-13, PostgreSQL `postgres:16` | 20 of 20, lowest -150.00 | 20 of 20 |
| `measure.sh`, 2026-11-14 | 17 of 20, lowest -150.00 | 16 of 20 |

Both runs were on an Apple M2 Pro with macOS 26.4.1. PostgreSQL and Redis ran in Docker.
