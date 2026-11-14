# Results: a fake made from an OpenAPI description

This page records the runs that proved `hydo fake from-openapi` (Milestone 249). The example is
a marketplace service, `market`, in Go. For each order it pays the seller out through a payouts
provider and then reads the payout back. The provider's API is the OpenAPI 3.1 description
`api/payouts.yaml`, written for this example. The fake in `hydo.yaml` is the entry that this
command wrote, unedited:

```sh
hydo fake from-openapi api/payouts.yaml --name payouts --port 9001 --stateful --ref json:order \
  --add-to hydo.yaml
```

The fake has three routes. `create_payout` answers 201 with `"id":"po_{seq}"`, repeats the
request's `amount`, `currency` and `order`, saves them, and is idempotent by the header
`Idempotency-Key`. `get_payout` loads the payout by `{path:id}` and answers the saved fields.
Each route's `errors:` holds the spec's error answers (400, 409, 429 and `5XX` on the create).

One planted bug is in `src/retry_buggy.go`: only the first try of a create carries the
idempotency key. `src/retry_fixed.go` sends the key on every try.

## How the runs were made

- Machine: this Mac (an M2 Pro), the Mac engine.
- hydo: this branch's `hydo`, `hydo-build`, `hydo-hvm` and arm64 `hydo-world`, inside the
  unpacked 0.3.3 Mac release, with the epoch-3 kernel `Image-explore-cg` (`84ea22e8…`).
- Programs: `PAYOUTS_GOARCH=arm64 build.sh` with Go 1.27.3.
- Images: `hydo build hydo.yaml --set bin=<dir> --set variant=buggy|fixed`, fingerprint epoch 3.

## Directed runs

Each run below was replayed with `hydo repro`, and each replay gave the recorded fingerprint.

| Image | Schedule | Result |
|---|---|---|
| fixed | `disk=none \| run` | OK, `fp=daf9fd92d5ee0ddc`; 8 payouts, each order at most once |
| buggy | `disk=none \| run` | OK, `fp=a8b14bd12c772942` |
| buggy | `disk=none \| run H=payouts.create_payout@3-4:status=500,effect=yes` | VIOLATION `[payouts.paid_at_most_once] payouts.create_payout: ref o3 took effect 3 times (records 7,8,9)`, `fp=73a17ac86c25da66` |
| fixed | the same schedule | OK, `fp=1766042b23138823` |

The fake's record shows the difference. With the bug, the two retries carry no key, so each one
is a new payout:

```text
hydo-world: 7 payouts.create_payout ref=o3 idem=payout-o3 fx=1 ok+status500
hydo-world: 8 payouts.create_payout ref=o3 idem=- fx=1 ok+status500
hydo-world: 9 payouts.create_payout ref=o3 idem=- fx=1 ok+status500
```

With the fix, the retries carry the key, and the fake answers them as replays:

```text
hydo-world: 7 payouts.create_payout ref=o3 idem=payout-o3 fx=1 ok+status500
hydo-world: 8 payouts.create_payout ref=o3 idem=payout-o3 fx=0 replay+status500
hydo-world: 9 payouts.create_payout ref=o3 idem=payout-o3 fx=0 replay+status500
```

In the run without faults every create is followed by a read of the same payout, which the fake
answers from the saved object (`payouts.get_payout ... fx=0 ok`).

## Blind exploration

`hydo explore --seeds 1-120 --protocol-faults --workers 1`:

| Image | Schedules | Failures | Wall time |
|---|---|---|---|
| buggy | 120 | 15, each reproduced, one signature `[payouts.paid_at_most_once]` | 164 s |
| fixed | 120 | 0 | 130 s |

The minimal schedule of the buggy image is
`disk=none dseed=905c768ad49f146c | run H=payouts.create_payout@1-2:delay=2500`: the answer
comes after the client's 2-second timeout, and the retry without a key pays again. Its two runs
gave `d945627dfcb557fa`. Besides 500, 502, 503 and 429, both explorations drew the statuses
that the spec lists for the routes: 400, 404, and 409.
