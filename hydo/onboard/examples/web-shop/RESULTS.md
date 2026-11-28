# Results: the web-shop example

This page records the runs that proved `browser:` (Milestone 253), the walk (Milestone 258), and
both on the Mac engine (Milestone 284, the last section). The design is
`docs/design/web-apps.md`, and `hydo guide web-apps` is the how-to.

## What is in the example

| file | what it is |
|---|---|
| `app/` | A React 19 shop built with Vite: a product list, a cart, a checkout, and an order page. `serve.mjs` serves the built app and passes `/api/*` on to the API, as a production web server does. |
| `e2e/` | The shop's Playwright tests (`@playwright/test` 1.65.1): three tests in two files, and an ordinary `playwright.config.ts` with a `webServer`. |
| `hydo.yaml` | `browser:` with the tests, a fake `api` with four routes, and one check over the fake's record. |
| `build.sh` | `npm ci` in `app/` and `e2e/`, and both builds of the app. |
| `hydo-walk.yaml` | The same app and fake with `browser.explore`: hydo walks through the pages, and no test runs (Milestone 258). |
| `walk-repro/` | The minimal walk of the stuck order page as a Playwright test, as hydo wrote it. |

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
  `browser:`"). `hydo build`'s own fetch of a missing image did not run here, because the file
  was already there; it ran in Milestone 254 (the section "The image fetch of
  `hydo build`").
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

## The image fetch of `hydo build`

In Milestone 254, on a GitHub runner (ubuntu-24.04, run 43690697894 of the workflow `tmp-web-kvm`),
`browser/` did not exist, and `hydo build hydo.yaml --set variant=buggy --set screens=all`
fetched the image itself:

```text
hydo: fetching the image of browser (mcr.microsoft.com/playwright@sha256:766137e2d9caad4744187e53cdc89320ba6f2eb57cff097374020944b44dd0a9, x86_64) into /mnt/w/web-shop/browser/playwright-v1.65.1-noble-amd64.docker.tar
fetch_image: layer 1/7 sha256:26538842dfbf
...
fetch_image: mcr.microsoft.com/playwright@sha256:766137e2d9ca -> /mnt/w/web-shop/browser/playwright-v1.65.1-noble-amd64.docker.tar.part (7 layers, config sha256:12708c20bc00)
hydo-build: service browser from image /mnt/w/web-shop/browser/playwright-v1.65.1-noble-amd64.docker.tar (mcr.microsoft.com/playwright:v1.65.1-noble, config sha256:12708c20bc00)
```

The config digest is the one in `docs/BUILD-PINS.md`. The file that `hydo-fetch-image` fetched
for the hvx runs on this page is 1,012,681,216 bytes with sha256 `6c535d9b038b3d8d…`. That build then
stopped at `HYDO_IPROUTE2 not found`, because the workflow had not run `hydo setup` for the
unpacked release (AGENTS.md open item 98).

## Runs on KVM (Milestone 254)

The example ran on GitHub's runners with the KVM engine, in two runs of the workflow
`tmp-web-kvm`. The seven hosts were three Intel Xeon 6975P-C, two AMD EPYC 7763, an EPYC 9V45,
and an EPYC 9V74. Every host built the image `--set variant=buggy --set screens=all` with the same bytes
(`028b1d5957c784c5…`). Each schedule ran two or three times on each host:

| schedule | verdict | Intel | AMD |
|---|---|---|---|
| `disk=none \| run` | OK | `aeeddec40e376597` | `079609ace3862b46` |
| `disk=none \| run H=api.add_item@1-2:delay=1000` | VIOLATION `browser.tests_pass` | `219021f0d8672b2b` | `a49c1cf2ef161478` |
| `disk=none \| run H=api.get_order@0-1:status=500` | VIOLATION `browser.no_page_errors` | `ac4a12df281ec556` | `1cd86738d0cc3e6f` |
| `disk=none \| run H=api.place_order@0-1:drop` | VIOLATION `api.order_placed_once` | `3c570752469652b4` | `88db5112508eb1f0` |

The verdicts are the hvx verdicts on this page. Every run that ended gave its schedule's fingerprint,
and the AMD models agree with each other. The screenshots in each run's `screens/` had the same
bytes on all seven hosts.

With a clean run's screenshots in `hydo-screens/` and `--set screens=compare`, the clean schedule
passed on every host with `browser.screens_match` held. Under `H=api.get_order@0-1:status=500`
it failed. The order page showed the spinner in place of the order's title. The finding counted
5,505 changed pixels of 921,600 (0.60 %), inside the box from x=15, y=56 to x=311, y=138.

Some runs first stopped in the guest kernel's early boot with `SPIN`, before the agent started
(13 of 85, on Intel and on AMD). `hydo run` ran each again in the engine's rewind mode, and 10
of them then ended with the schedule's fingerprint. The cause is not found; docs/HISTORY.md
Milestone 254 has the details.

## The walk of Milestone 258: no test, both planted bugs found blind

`hydo-walk.yaml` is the same app and the same fake API with `browser.explore` in place of the
tests: hydo walks through the pages itself (docs/design/ui-exploration.md). No Playwright test
runs. Each of 4 workload steps takes 10 actions, drawn from the schedule's `I=` seed and the
page's accessibility tree. The image is the buggy variant, built by `hydo build hydo-walk.yaml
--set variant=buggy` on the bare-metal machine, and every run used hvx.

**One walk, one run.** `disk=none | run I=2` gave `941a176ff3a25e7c` twice: OK, 5 invariants
held, and the fake took 3 orders, each once. The walk opened the shop, added three products,
went back, reloaded, opened the cart and placed the order in step 0, and went on. The run took
65 s of wall time.

**The double order, directed.** With the same walk, `disk=none | run I=2
H=api.place_order@0-1:drop` loses the answer to the order of step 0 (`a99fbffc163e2f50`):

```text
VIOLATION [api.order_placed_once] api.place_order: ref cart-1767225603742-441263 took effect 3
times (records 6,9,19)
```

**Both bugs, blind.** `hydo explore --seeds 1-20 --protocol-faults --workers 1` drew `H=` tokens
on the fake's four routes and a walk for every schedule. It ran 20 schedules in 40 minutes of
wall time (a mean of 19.2 s per run), and 4 failed. Each reproduced, and each was shrunk:

| seed | minimal schedule | verdict | fingerprint, two runs | shrink runs |
|---|---|---|---|---|
| 7 | `disk=none dseed=63cbe1e459320dd7 \| run H=api.get_order@3-4:reset I=a0d28305209409f6,n2,x0.0,x1.0,x2.0` | VIOLATION `browser.no_page_errors`: "The page /orders/o7 left a promise rejection unhandled during the test "walk step 3": HttpError: HTTP 502 for /api/orders/o7" | `2eb637951be51766` | 32 |
| 8 | `disk=none dseed=9e5651b0ef953636 \| run H=api.place_order@2-4:reset I=7e1f0a19d3e49979,x0.0,x0.1,x1.0` | VIOLATION `api.order_placed_once`: one cart took effect 4 times | `3185435799cf68dc` | 44 |
| 12 | `disk=none dseed=943ff9fc99de8f03 \| run H=api.place_order@2-3:reset,p=50 H=api.products@2-3:status=500,effect=yes,p=25 I=93667175d6c2e640,n4,x0.0,x0.1` | VIOLATION `api.order_placed_once`: 2 times | `0ae952a0e752ca29` | 44 |
| 17 | `disk=none dseed=808475f02ee37363 \| run H=api.place_order@2-3:reset I=37ce1f8dfefa7c4b,n4,x1.0,x2.0` | VIOLATION `api.order_placed_once`: 3 times | `06d6731eb67a2008` | 40 |

Seed 7 is the order page's planted bug: the walk added a product, opened the cart and placed the
order while the order read's connection was reset. Its run also failed `browser.busy_ends` and
`browser.failures_shown`: the spinner stayed with no error on the screen. Seeds 8, 12 and 17 are
the planted double order: the walk placed an order while `place_order`'s connection was reset,
and the client sent the order again without a key. The shrinker cut seed 7's walk from 40
actions to 5. None of the 16 clean runs was vacuous: each passing step of a walk counts as
acknowledged work.

**Images of later code.** The exploration above ran on an image built before the last changes
to the walker: the recording, the format of the acknowledged writes, the size bound and the
place of a clock step's comment in `repro.spec.ts`. An image of later code has other bytes, so
other fingerprints, and its runs take other times.

- On an image of the code before the last two of those changes, each minimal schedule ran twice
  with one fingerprint: seeds 7, 8 and 12 failed as found (`d80d86bf54f21525`,
  `d1a958789ad8faab`, `e916d9b6a70fd4ac`), and seed 17 passed (`fabc1311c86bba62`): its walk now
  places the order where the reset of `place_order` does not catch it. A timing-sensitive
  finding depends on its image's bytes, as Milestone 221 saw for `hydo versions`.
- On an image of this milestone's final code: seed 7 gave `9cd5aa4edbe5c332` twice, with the
  same sentence; seed 8 gave `d5559e9f96a59f8b` twice, one cart ordered 4 times; and `disk=none
  | run I=2` gave `013f27dc85f842ce` twice, OK. The exploration of this image is below.

**The evidence of a failing walk.** On the image before the last two changes, each minimal
schedule ran once more with `,rec`; on the final image seed 7's did (`8bea8ca7eefc5cc3`). Each
kept its verdict, and each run's `screens/` had the walk as `repro.spec.ts`, the video of the last
step (`walk-step-3.webm`, 29,508 bytes for seed 7) and its Playwright trace
(`walk-step-3.trace.zip`, 7,386 bytes). `hydo report --html` of seed 7's recording run shows the
video under "The walk", with the trace's command and the test.

**The walk as a test, without hydo.** Every run leaves `screens/repro.spec.ts`. The one of seed
7's minimal schedule is `walk-repro/stuck-order-page.spec.ts`: 5 actions, with the reset of the
order read as a `page.route` handler in step 3. On this Mac, against `app/serve.mjs` and a local
stand-in for the fake API, Playwright 1.65.1 ran it: it failed on the buggy build ("uncaught
exceptions and unhandled rejections in the page") and passed on the fixed one. Without hydo the
reset reaches the page as `Failed to fetch`, where hydo's reset made `serve.mjs` answer 502.

**The exploration of the final image.** The same `hydo explore --seeds 1-20 --protocol-faults
--workers 1` on the image of this milestone's final code took 24 minutes (a mean of 19.1 s per
run) and found the same 4 failures, seeds 7, 8, 12 and 17, each reproduced:

| seed | minimal schedule | fingerprint, two runs |
|---|---|---|
| 7 | `disk=none dseed=63cbe1e459320dd7 \| run H=api.get_order@3-4:reset I=a0d28305209409f6,n2,x0.0,x1.0,x2.0` | `9cd5aa4edbe5c332` |
| 8 (and 17) | `disk=none dseed=9e5651b0ef953636 \| run H=api.place_order@2-4:reset,p=50 I=7e1f0a19d3e49979,n8,x0.0,x0.1,x0.2` | `e2257adfecbf4a4a` |
| 12 | `disk=none dseed=943ff9fc99de8f03 \| run H=api.place_order@2-3:reset,p=50 H=api.products@2-3:status=500,effect=yes,p=25 I=93667175d6c2e640,n4,x0.0,x0.1` | `8b73b00d7084c42f` |

Seeds 8 and 17 had one signature, so one shrink. At its end `hydo explore` replayed the three
minimal walks with `,rec`; each kept its verdict, and each `replays/rec-<k>/screens/` held the
walk's test, the last step's video and its trace.

## On the Mac engine (Milestone 284)

Both manifests run on an Apple Silicon Mac since Milestone 284. The image's root file system is on
its disk (`guest.root: disk`, the default with `browser:`), so the 2.4 GB of the Playwright
image do not have to fit in guest RAM. The runs used this tree's programs over the 0.3.4 Mac
release on an M2 Pro, one VM at a time, with images built by `hydo build <manifest> --set
variant=buggy` at the fingerprint epoch 3:

- `hydo.yaml`: `explore-ob-web-shop-arm64.cpio.gz` (830,910 bytes, sha256 `7f7024ca…`) and its
  disk `a70204c1…`, 3,423,600,640 bytes (a root of 3,200 MiB and a data disk of 64 MiB).
- `hydo-walk.yaml`: `explore-ob-web-shop-walk-arm64.cpio.gz` (`bf6dd144…`) and its disk
  `9d2d78ca…`.

| image | schedule | verdict | fingerprint | runs | wall time |
|---|---|---|---|---|---|
| tests | `disk=none \| run` | OK: 3 tests passed, 6 invariants held | `e49b289c2d4f34b7` | 2 | 56 s, 50 s |
| tests | `disk=none \| run H=api.add_item@1-2:delay=1000` | VIOLATION `browser.tests_pass`: Expected "2", Received "0" | `15e26130aad215fd` | 2 | 49 s, 48 s |
| walk | `disk=none \| run I=2` | OK: 5 invariants held, the fake took 1 order once | `b76ad58b5c0ae26b` | 2 | 56 s, 44 s |

The verdicts are hvx's. The fingerprints are the Mac engine's own, and the walk of `I=2` takes
other actions than on hvx, because the pages' timing differs between the engines.

These images are of the fingerprint epoch 3. Rebuilt at the epoch 4 (disks `bc0ad90a…` and
`e253d638…`), each schedule kept its verdict with one fingerprint over two runs:
`disk=none | run` `7da8112398ae9137`, the delay `24c270f6f56a9d85`, the walk `I=2`
`6a73a4def523f095`, and seed 7's minimal schedule below `f956bda2f5aaa2c8`.

**Both planted bugs, blind.** `hydo explore --seeds 1-20 --protocol-faults --workers 1` on the
walk image ran 20 schedules, their shrinks and the recording replays in 79 minutes. That is 104
runs, with a mean of 40.8 s per run in the engine (on hvx, 19.1 s). Two schedules failed, each reproduced and
shrunk; both minimal schedules gave one fingerprint on the explorer's two runs:

| seed | minimal schedule | verdict | fingerprint | shrink runs |
|---|---|---|---|---|
| 7 | `disk=none dseed=63cbe1e459320dd7 \| run H=api.get_order@3-4:reset I=a0d28305209409f6,n7,x0.0-6,x1.0-6,x2.0-6` | VIOLATION `browser.no_page_errors`: "The page /orders/o10 left a promise rejection unhandled during the test "walk step 3": HttpError: HTTP 502 for /api/orders/o10" | `18de0fcd41170aeb` | 29 |
| 17 | `disk=none dseed=808475f02ee37363 \| run H=api.place_order@2-4:reset I=37ce1f8dfefa7c4b,x0.0-9,x1.0-9,x2.0-4,x3.0-6,x3.8,x3.9` | VIOLATION `api.order_placed_once`: one cart took effect 4 times | `bec314ece467e3ba` | 49 |

Seed 7 is the order page's planted bug, and seed 17 the double order. The schedules of seeds 7
and 17 are the ones hvx drew, because the explorer draws them from the seed alone. Seeds 8 and 12,
which failed on hvx, passed here: the walk's pages answer at other moments of the guest's clock,
so the reset of `place_order` did not catch an order. The 104 runs had 89 different schedules,
and no schedule gave two fingerprints. At its end `hydo explore` replayed both minimal walks with
`,rec`; each kept its verdict, and each `replays/rec-<k>/screens/` held `repro.spec.ts`, the last
step's video (64,238 and 69,579 bytes) and its trace.

## The filmstrip and the read-back of Milestone 334

`hydo-walk.yaml` gained `browser.read_back`: every order that `POST /api/orders` answered with
its id (`{"id":"o6"}`) must open on its own page, `/orders/<id>`, as "Order <id> confirmed". The
runs used hvx on the bare-metal machine, with this branch's `hydo` over the 0.4.0 Linux release,
at the fingerprint epoch 5. The image was built with `--set variant=buggy`.

| schedule | verdict | the read-back check | fingerprint |
|---|---|---|---|
| `disk=none \| run I=1` | OK: 6 invariants held, the fake took 3 orders once each | "every one of the 3 objects ... is shown, on 3 pages" (`/orders/o6`, `o18` and `o27`) | `5dac5c131187df81`, twice |

The run directory kept 47 frames (121 KB in all), one after the page opened in each step and one
after each of the 40 actions, and 3 frames of the read-back check. It kept no trace, because the
run passed. The copy after the verdict took about 15,000 of the run's 298,557 stops.

`hydo explore --seeds 1-10 --protocol-faults --workers 1` ran 10 schedules, the shrink and the
recording replay in 965 s, a mean of 24.5 s per run in the engine (19.1 s in Milestone 284: the
frames, the traces and the read-back check cost the rest).

- Seed 7 failed, as in Milestone 258: "The page /orders/o32 left a promise rejection unhandled
  during the test "walk step 3": HttpError: HTTP 502 for /api/orders/o32". Its run directory
  kept the filmstrip and the traces of its 4 steps (14 to 17 KB each). The 9 passing runs kept
  their filmstrips and no trace.
- The shrinker took 29 runs to `disk=none dseed=63cbe1e459320dd7 | run H=api.get_order@3-4:reset
  I=a0d28305209409f6,n7,x0.0-6,x1.0-6,x2.0-6`, the minimal schedule of Milestone 284, which gave
  `2073817890afc835` on both confirmation runs. The page of the exploration shows the filmstrip of
  the confirmation run `min-a` in the failure's details: 12 frames, the 7 actions of step 3
  among them. The recording replay `replays/rec-1/` kept the video, as before, and its own
  filmstrip and traces.
- The read-back check held in every run, with 1 to 4 orders each, also in seed 7, where the
  page of order o32 failed during step 3 and showed the order at the end.

## Answers in another order, double submits, and offline pages (Milestone 393)

`hydo-search.yaml` walks the builds with the search box (`app/dist-buggy-search`,
`app/dist-fixed-search`, from `VITE_SEARCH=1`). Its list asks `GET /api/search?q=` when the page
opens and at every change of the box, and the fake route `search` answers with a title that names
the query. `browser.watch.answers: true` adds `browser.newest_answer_shown`. The buggy build shows
whichever answer came last; the fixed build drops the answer of every search but the newest.

The runs used hvx on the bare-metal machine, with this branch's programs over the 0.7.0 Linux
release's guest files and the branch's `hydo-world` (`HYDO_WORLDBIN`, sha256 `d0138fbc…`), at the
fingerprint epoch 7. Each schedule ran twice from a fresh boot, one VM at a time, and gave one
fingerprint both times.

| image | schedule | verdict | fingerprint |
|---|---|---|---|
| search, buggy (`911e7d7f…`) | `disk=none \| run I=1` | OK, 7 properties held | `dfdf037d0c5c83f4` |
| search, buggy | `disk=none \| run H=api.search@0-4:order=2 I=1` | VIOLATION `browser.newest_answer_shown`: the page `/` shows the answer of `GET /api/search?q=` after the answer of `GET /api/search?q=%23tag` (3 times in the run) | `899f47669f8c705e` |
| search, fixed (`280f2e16…`) | the same | OK; 2 answers came after a newer one's, and the page showed the newer one; 1 was not judged | `e9cab0e71251cb5e` |
| search, buggy | `disk=none \| run N=browser@1~500:5000 I=1` | OK: the browser's link was cut 500 ms into step 1 for 5 s, and every page check held | `92692705fa6d2b41` |
| tests, buggy (`d398c493…`) | `disk=none \| run H=api.place_order@0-1:twice` | VIOLATION `api.order_placed_once`: one cart took effect 2 times (records 3 and 4) | `877f5ec0558fff8c` |
| tests, fixed (`644fbb76…`) | the same | OK: the second sending had the key and was a replay | `ecede369d2009748` |

**The blind exploration.** `hydo explore --seeds 1-20 --answer-faults --link-cuts --workers 1` on
the buggy search image ran 20 schedules in 802 s. Four failed, and every failure ran again with
its verdict:

- seed 7, `N=browser@0~2000:500 H=api.search@2-4:order=3 H=api.add_item@2-4:twice I=a0d28305…`:
  `browser.newest_answer_shown`, the planted search bug;
- seeds 14 and 16: `api.order_placed_once`, the planted order without a key, sent twice by
  `H=api.place_order@...:twice`;
- seed 18, `H=api.get_order@1-4:order=3`: `browser.no_page_errors` on `/checkout`. The held
  answer passed the client's time limit of 3 s, and the buggy page left the rejection unhandled,
  the second planted bug. It shrank in 47 runs to `H=api.get_order@1-4:order=3
  I=d94729ae6c47c1f6,x0.0-9,x1.0-5,x3.0-9`.

The explorer's process ended with SIGPIPE (exit status 141) after 1 h 18 min, during the shrinks,
with its first report written. `hydo shrink <explore dir> --all` (Milestone 387) finished them in
the same folder in 52 minutes. The search bug shrank in 49 runs to `disk=none dseed=0 | run
H=api.search@2-4:order=3 I=a0d28305209409f6,x0.2-9,x1.0-9,x2.0-9`: the link cut and the `twice`
rule went, and so did every action but two of step 0 and the ten of step 3. The order sent twice
shrank in 69 runs to `H=api.place_order@1-2:twice I=1bbd089acadda3c3,n7,...`, and seed 16 is one
problem with it.

**What did not move.** `hydo-walk.yaml` (the walk without the search box) built by this branch's
`hydo` and by `main`'s (`ae19e5a8`), both with the released `hydo-world`, gave one image,
`ddbef3ba…`. A first build from `main` differed only in two file modes, 664 against 644, because
its tree was unpacked under another umask (open item 78); with the modes matched the bytes are
equal. On that image `disk=none | run I=1` gave `d3ee8d5e7c289e8a` and `disk=none
dseed=9e5651b0ef953636 | run H=api.place_order@2-4:reset I=7e1f0a19d3e49979,x0.0,x0.1,x1.0` gave
`030ab3d6eec55b0d`, with this branch's hydo and with `main`'s alike. The builds of the app
without `VITE_SEARCH` are byte for byte the ones before.
