# Results: the web-shop example

This page records the runs that proved `browser:` (Milestone 253). The design is
`docs/design/web-apps.md`, and `hydo guide web-apps` is the how-to.

## What is in the example

| file | what it is |
|---|---|
| `app/` | A React 19 shop built with Vite: a product list, a cart, a checkout, and an order page. `serve.mjs` serves the built app and passes `/api/*` on to the API, as a production web server does. |
| `e2e/` | The shop's Playwright tests (`@playwright/test` 1.65.1): three tests in two files, and an ordinary `playwright.config.ts` with a `webServer`. |
| `hydo.yaml` | `browser:` with the tests, a fake `api` with four routes, and one check over the fake's record. |
| `build.sh` | `npm ci` in `app/` and `e2e/`, and both builds of the app. |

The variant `buggy` (`--set variant=buggy`) has two planted bugs:

- The order is retried after a timeout without an idempotency key (`app/src/api.js`). An order
  whose answer is lost is placed twice.
- The order page has no error handling (`app/src/App.jsx`). When the order cannot be read, the
  spinner stays and the promise rejection is unhandled.

The variant `fixed` sends one key per cart, and shows an error toast (`role=alert`) on every
failed call.

One test is flaky as it is written. "adds two products to the cart" clicks two buttons, waits
250 ms, and reads the cart count once. It passes while the API answers quickly. On this Mac, with a
local stand-in for the API that answers after a random 0 to 300 ms, it passed 19 of 20 runs; with
no added latency it passed 10 of 10.

## How the runs were made

- Machine: the bare-metal machine, the hvx engine, the main module (srcversion
  `91D86B693F4D4035C368F9B`).
- hydo: this branch's `hydo` over the 0.3.3 Linux release, fingerprint epoch 3.
- Inputs: the app's two builds and the tests' packages, made on the Mac with Node.js 24.16.0 from
  the two lock files (as `build.sh` makes them). The Playwright image was fetched with
  `hydo-fetch-image` by the digest that `hydo build` pins (`docs/BUILD-PINS.md`, "Playwright for
  `browser:`"); `hydo build`'s own fetch of a missing image did not run, because the file was
  already there.
- Images: `hydo build hydo.yaml --set variant=buggy` gave `explore-ob-web-shop-disk.img`, sha256
  `c03111de5b8d1c64…`, 4,094,689,280 bytes (a root of 3,392 MiB and a data disk of 512 MiB). The
  Playwright image's tree is 35,790 entries; hydo-build left out two systemd unit names with a
  backslash.
- Each schedule ran with `hydo run --image … "<schedule>"` from a fresh boot, under
  `~/bin/hvx-run`, one VM at a time. The bare-metal machine was shared with other sessions: a run
  took 48 s to 10 minutes of wall time, most of it reading the 4 GB image.

## Determinism

The clean schedule gave one fingerprint on every run, and so did every schedule below. A run boots
Linux, starts the fake, and runs `playwright test` twice (two shards): Node.js, the TypeScript
transform of the tests, the config's web server, and Chromium's headless shell. Nothing in the
browser, the image or the tests had to change for it. Playwright's own timers, Chromium's
renderer and the page's `Date.now()` all run on the guest's clock, which hvx makes the same on
every run.

| image | schedule | verdict | fingerprint | runs |
|---|---|---|---|---|
| buggy | `disk=none \| run` | OK: 3 tests passed, every watched property held | `ce47cfb323d48c69` | 2 |

## Proof 1: a flaky test that fails on every run of one schedule

The test "adds two products to the cart" waits 250 ms after two clicks and reads the cart count
once. On the Mac it passed 19 of 20 runs against an API that answers within 0 to 300 ms. Under
hydo the answer time is part of the schedule, so each schedule gives one verdict on every run:

| schedule | verdict | fingerprint | runs |
|---|---|---|---|
| `disk=none \| run` | OK | `ce47cfb323d48c69` | 2 |
| `disk=none \| run H=api.add_item@1-2:delay=300` | VIOLATION `browser.tests_pass`: Expected "2", Received "1" | `6101da44b9f69c95` | 1 |
| `disk=none \| run H=api.add_item@1-2:delay=500` | VIOLATION, Received "1" | `4149e334f7cda032` | 1 |
| `disk=none \| run H=api.add_item@1-2:delay=1000` | VIOLATION, Received "0" | `139a04e83258d442` | 2 |

The verdict's message is the test's own assertion:

```text
The test "shop.spec.ts › adds two products to the cart" failed after 0.9 s: Error:
expect(received).toBe(expected) // Object.is equality / Expected: "2" / Received: "0"
```

With 300 ms the second answer comes after the read, so the count shows 1. With 1000 ms both come
after it. Step 1 holds only this test (`shards: 2` with `--fully-parallel`), so the token touches
no other test.

## Proof 2: what only an API fault shows

The buggy variant's two bugs pass every test of the suite when the API answers. Each shows under
one `H=` token, and the fixed variant passes the same token's watched properties (below).

**A lost order answer: one cart, two orders.** `disk=none | run H=api.place_order@0-1:drop`: the
API takes the order and its answer never comes. The client gives up after 3 s and sends the order
again without a key. Two runs, `1fadce7a74395c4a` both times:

```text
VIOLATION [api.order_placed_once] api.place_order: ref cart-1767225604615-534415 took effect 2
times (records 3,4)
```

The page handled the lost answer as its code says: it sent the request again. No watched
property of the page failed; the fake's record is what shows the double order.

**An error answer: a spinner that stays and an unhandled rejection.** `disk=none | run
H=api.get_order@0-1:status=500`: the order page cannot read the order. Two runs,
`3f71aaf23542d434` both times, with four findings:

```text
VIOLATION [browser.no_page_errors] The page /orders/o3 left a promise rejection unhandled during
the test "checkout.spec.ts › places an order": HttpError: HTTP 500 for /api/orders/o3

browser.busy_ends: The page /orders/o3 showed the busy element .spinner ("Loading your order…")
for 5.1 s, until the test ended, after GET /api/orders/o3 answered 500 during the test
"checkout.spec.ts › places an order"; it showed no error. Console: HttpError: HTTP 500 for
/api/orders/o3 Screenshot: /tmp/hydo/web/shots/1.png

browser.failures_shown: The page /orders/o3 showed no error when GET /api/orders/o3 answered 500
during the test "checkout.spec.ts › places an order"; nothing appeared in the next 5.0 s.

browser.tests_pass: The test "checkout.spec.ts › places an order" failed after 6.6 s: Error:
expect(locator).toHaveText(expected) failed / Locator: getByTestId('order-title')
```

## The fixed variant under the same tokens

The fixed image (`--set variant=fixed`, sha256 `e11f4729c5dc0bac…`) sends one idempotency key per
cart, and shows an error toast on every failed call. Under each token that broke the buggy build,
only the tests' own assertions fail; every watched property of the page holds:

| schedule | verdict | fingerprint | runs |
|---|---|---|---|
| `disk=none \| run` | OK | `d4cba4c0812505be` | 2 |
| `disk=none \| run H=api.get_order@0-1:status=500` | VIOLATION `browser.tests_pass` only: the order page shows its toast | `86f32d0f9b6e5254` | 1 |
| `disk=none \| run H=api.place_order@0-1:drop` | VIOLATION `browser.tests_pass` only: the second try is a replay, one order | `55b6755b7802c9d7` | 1 |
| `disk=none \| run H=api.add_item@1-2:delay=1000` | VIOLATION `browser.tests_pass`: the flaky test is the same in both builds | `3d57f8b7988570e5` | 1 |
| `disk=none dseed=63cbe1e459320dd7 \| run H=api.get_order@0-1:reset` | VIOLATION `browser.tests_pass` only | `7725678008c965ea` | 1 |

Under faults the tests fail by design: a test that expects "Order o3 confirmed" cannot pass when
the order cannot be read. `browser.must_pass: false` keeps only the watched properties, for an
exploration that should report the page's own failures alone.

## The exploration: the stuck spinner found blind

`hydo explore --seeds 1-20 --protocol-faults --workers 2` on the buggy image drew `H=` tokens on
the four routes of the fake, with no hint of where the bugs are. The browser's service was not a
fault target. 20 schedules ran in 46 minutes of wall time on the shared machine (a mean of 18.7 s
per run, 104 s to boot each worker). Four failed, each reproduced, and each was shrunk:

| seed | minimal schedule | verdict | fingerprint, two runs |
|---|---|---|---|
| 7 | `disk=none dseed=63cbe1e459320dd7 \| run H=api.get_order@0-1:reset` | VIOLATION `browser.no_page_errors`: "The page /orders/o3 left a promise rejection unhandled during the test "checkout.spec.ts › places an order": HttpError: HTTP 502 for /api/orders/o3" | `e405de9b8e7299de` |
| 9 | `disk=none dseed=aeaf52febe706064 \| run H=api.add_item@1-2:drop` | VIOLATION `browser.tests_pass`: the flaky test reads "0" | `1253addc6f66ef5c` |
| 11 | `disk=none dseed=50f5647d2380309d \| run H=api.products@0-2:status=429,retry-after=1,effect=yes,p=25` | VIOLATION `browser.tests_pass`: "lists the products" times out | `0e3e421d46644322` |
| 20 | `disk=none dseed=362259904816818c \| run H=api.add_item@0-2:status=502` | VIOLATION `browser.tests_pass`: the cart count stays "0" | `f0f863f2fc571d36` |

Seed 7 is the planted bug, found blind. `hydo run` of its minimal schedule by hand gave the
explorer's fingerprint, `e405de9b8e7299de`, and the same four findings. The fault was a reset of the order read's connection. The
shop's web server answered 502, the order page left the rejection unhandled, and its spinner
stayed with no error on the screen. `browser.busy_ends` and `browser.failures_shown` failed in the
same run. Seeds 9, 11 and 20 are the tests' own assertions under API faults; the watched
properties of the page held in them, because the page showed its error toast or sent the request
again.

The explorer warned of 16 vacuous passes: the workload acknowledges nothing (`$HYDO_ACKED` stays
empty). A passing test is the workload's acknowledgement, and the next version should record it.

A second exploration, `--seeds 21-40`, took 52 minutes. 9 of its 20 schedules failed, each
reproduced; all nine were the tests' own assertions or the tests' 30 s timeout under API faults
(for example `H=api.get_order@0-1:drop`, `3b1ed60aa424e97d` twice). No watched property of the page
failed in them. In seed 38, `H=api.get_order@0-2:drop`, the buggy order page would reject at 6 s
(two tries of 3 s), but the test ends at 5 s, so the rule judges nothing: a test's own timeout
limits what the watcher sees.

The double order was not found blind in these 40 schedules. It needs a lost or late answer to
`place_order` in step 0, and the drawn `place_order` tokens fell into step 1, which holds no
order. `hydo explore --sweep protocol` tries every rule kind on every route in every step and
reaches it, but it was stopped after 12 of its schedules to leave the shared machine to the
tests; the directed run earlier on this page shows the bug.

## The exploration with `--protocol-cover` (Milestone 257)

`hydo explore --seeds 1-24 --protocol-cover --workers 1` walks every route of the fake in every
step. Its probe run showed that step 0 calls the four routes and step 1 calls `products` and
`add_item`. On the buggy image it took 1,263 s, and 23 of 24 schedules failed, each reproduced:

| seed | token | finding |
|---|---|---|
| 3 | `H=api.place_order@0-1:drop` | the double order, `[api.order_placed_once] ... took effect 2 times`; minimal schedule `disk=none dseed=1d0b14e4db018fed \| run H=api.place_order@0-1:drop`, `dfad6f0d632c239a` twice |
| 4 | `H=api.get_order@0-1:reset` | the stuck spinner: `browser.no_page_errors`, `browser.busy_ends`, `browser.failures_shown`; `992e7e0eadfa53f1` twice |
| 10 | `H=api.get_order@0-1:status=500` | the stuck spinner again |
| 21 | `H=api.place_order@0-1:delay=10000` | the double order again |

The other 19 failures were the tests' own assertions or timeouts under API faults. The same 24
seeds on the fixed image failed only by `browser.tests_pass`: no double order, and every watched
property of the page held.
