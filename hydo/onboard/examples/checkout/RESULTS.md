# Results: the checkout example with three emulators

This page records the runs that proved `emulators:` (Milestone 242). The example is an order
service, `checkout`, in 242 lines of Go. For each order it does three things:

1. It charges the order through Stripe at `https://api.stripe.com`.
2. It stores the receipt in the S3 bucket `receipts`.
3. It mails the receipt over SMTP.

Three `emulators:` entries in `hydo.yaml` provide Stripe, S3, and SMTP. The emulators are
stripe-mock 0.206.0, `rclone serve s3` 1.76.0, and Mailpit 1.33.1. One planted bug is in
`src/key_buggy.go`: every try of a charge gets a new idempotency key.

## How the runs were made

- Machine: the bare-metal machine, the hvx engine.
- hydo: this branch's `hydo` and its x86_64 `hydo-world` and `hydo-world-tls`, inside the
  unpacked 0.3.2 Linux release.
- Programs: `build.sh` with Go 1.26.5.
- Images: `hydo build hydo.yaml --set bin=<dir> --set variant=buggy|fixed`. The build fetched the
  three images into `emulators/`: 9.6 MB, 34.7 MB, and 15.0 MB.

## Directed runs

Each run below was replayed with `hydo repro`, and each replay gave the recorded fingerprint
(`REPRODUCED`).

| Image | Schedule | Result |
|---|---|---|
| fixed | `disk=none \| run` | OK, `fp=2419ffbe836fb182`; 8 charges, each order at most once |
| buggy | `disk=none \| run H=stripe.payment_intent@3-4:status=500,effect=yes` | VIOLATION `[stripe.charged_at_most_once] stripe.payment_intent: ref o3 took effect 3 times (records 10,11,12)`, `fp=3884fc23ddc03321` |
| fixed | the same schedule | OK, `fp=d0ed3445d829f5a1` |
| buggy | `disk=none \| run H=stripe.payment_intent@3-4:delay=3000` | VIOLATION, ref o3 took effect 3 times, `fp=6c548970b20f44e4` |
| fixed | the same schedule | OK, `fp=d92392bfc6209af3` |

In the fixed run with `status=500,effect=yes`, the outside world's record shows how the proxy
answers a retry. The first try reached stripe-mock. The two retries carried the same key, so the
proxy answered each with the stored answer and recorded it as a replay:

```text
hydo-world: 10 stripe.payment_intent ref=o3 idem=charge-o3 fx=1 fwd+status500
hydo-world: 11 stripe.payment_intent ref=o3 idem=charge-o3 fx=0 replay+status500
hydo-world: 12 stripe.payment_intent ref=o3 idem=charge-o3 fx=0 replay+status500
hydo-world: 13 stripe.payment_intent ref=o4 idem=charge-o4 fx=1 fwd
hydo-world: 14 s3.put_object ref=o4 idem=- fx=1 fwd
hydo-world: 15 email.conn ref=- idem=- fx=1 fwd
```

The rule kept answering 500 for the whole step, so the service never acknowledged order o3. The
check `receipts.stored_and_mailed` reads S3 and Mailpit's API directly. It found every
acknowledged order's receipt in both places.

## Exploration

The explorer drew the faults without being told where the bug is. Its fault space named only
`checkout` as a service, so the three emulator services were never fault targets. The space had
24 `H=` targets: the routes of the three proxies.

| Image | Command | Result |
|---|---|---|
| buggy | `hydo explore --seeds 1-40 --protocol-faults --workers 4` | 40 schedules, no failure, 118 s |
| buggy | `hydo explore --seeds 41-240 --protocol-faults --workers 4` | 200 schedules, 5 failures (seeds 41, 77, 142, 156, 184), each reproduced, 242 s |
| fixed | `hydo explore --seeds 41-240 --protocol-faults --workers 3` | 200 schedules, no failure, 160 s |

The five failures are one property, `stripe.charged_at_most_once`. The shrinker gave two minimal
schedules, each run twice with one fingerprint:

- `disk=none dseed=118e846ea93bc949 | run H=stripe.payment_intent@4-5:drop`: the answers of step 4
  never come, and order o4 is charged 3 times. `fp=af456b6763e45693`.
- `disk=none dseed=6258cbe07c1ff081 | run H=stripe.payment_intent@2-5:status=500,effect=yes,p=50`:
  order o2 is charged twice. `fp=697297ee352d0eb5`.

None of the schedules that the explorations wrote names an emulator's service.

## The s3 emulator across a power cut

`hydo-s3-cut.yaml` (Milestone 354) checks that every write the s3 emulator acknowledged is on the
server after a power cut. Each step sends five writes through the s3 proxy:

- a PUT of 64 bytes and a PUT of 1 MiB;
- a copy of the first object;
- a multipart upload of 5 MiB and 1 KiB;
- a DELETE of the step before's first object.

`s3probe cut-check` reads each acknowledged write from rclone's own port in the recovery boot.
The runs used the Mac engine on 2026-11-24, each schedule twice with one fingerprint:

| Schedule | `durable: false` | `durable: true` (the file's value) |
|---|---|---|
| `disk=none \| run` | OK, 19 writes | OK, 19 writes |
| `disk=cut \| run C=3` | VIOLATION, 10 of 14 writes lost | OK, 14 writes |
| `disk=cut \| run C=4` | VIOLATION, 13 of 19 writes lost | OK, 19 writes |
| `disk=cut \| run A=s3.acked@2+5:cut` | VIOLATION, 10 of 12 writes lost | OK, 10 writes |
| `disk=cut \| run A=s3.acked@3:cut` | VIOLATION, 11 of 15 writes lost | OK, 15 writes |

Without `durable: true` every lost object answered 404: rclone renamed it into place, but the
rename stayed in the guest's memory. The fingerprints are in `docs/HISTORY.md` Milestone 354.
