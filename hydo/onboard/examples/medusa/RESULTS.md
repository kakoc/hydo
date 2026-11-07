# Medusa: results

A typical business application onboarded end to end the way an outside user would: Medusa 2.18.0
(open-source commerce, Node.js), PostgreSQL 15 and Redis 7, payments through Stripe. Medusa was
chosen because it is what hydo's most likely user runs: a web API in front of a database and a
queue, a third-party payment API called over HTTPS with certificate checks, and business rules
about money and stock that nobody wrote down as tests. The work started from Medusa's own
deployment guide (a Compose file, `medusa build`), then followed the onboarding skill and
`hydo guide`, with `hydo compose`, `hydo build`, `hydo run` and `hydo explore`.

Everything ran on **the build machine with the KVM engine** (`--backend kvm`): a 13th Gen Intel
Core i7-13700HX laptop, NixOS, Linux 6.6.35, `/dev/kvm` opened by the login user, no root and no
kernel module, as an ordinary Linux user would. The `hydo` used is the Linux release built for the
next version (with `bin/hydo-kvm` and `guest/bzImage-kvm`, installed with `install.sh --no-module`
into a clean prefix). The first exploration ran with that release's own `hydo`; the final image
and every fingerprint below come from this tree's `hydo` (built with the two fixes below, static,
next to the release's engine and guest files).

How long each phase took (wall time on 2026-11-07, UTC; the person doing it knew hydo well, so
an outside user would need longer for the debugging in friction items 2 to 4):

| phase | at | took |
|---|---|---|
| start: read the skill, the guide, Medusa's Docker guide and its Stripe provider | 15:25 | |
| the shop's image built (`app/build-app.sh`; one TypeScript error in the seed script) | 15:35 | 10 min |
| `hydo compose`, the images fetched, the first `hydo build` (23 s) | 15:38 | 3 min |
| first run: `ERROR service start (medusa)` | 15:39 | 1 min |
| first green run (four causes found, seven rebuilds of about 90 s each) | 15:48 | 9 min |
| every check written and green, the same fingerprint twice | 15:58 | 10 min |
| first finding, by hand (a capture answered 500 after it took effect) | 16:01 | 3 min |
| exploration started; first blind finding (finding 1, seed 2, reported as an oversell) | 16:04, about 16:10 | 6 min |
| exploration done: 20 schedules, 2 failures, both shrunk and reproduced | 16:15 | 11 min |
| both failures explained with debug checks (one turned out to be a check that judged too early) | 16:35 | 20 min |
| the final image: checks corrected, every recorded schedule twice, a second exploration | 16:40 to 17:03 | |

So: 23 minutes to the first green run, 36 minutes to the first finding by hand, about 45 minutes
to the first finding found blind.

## What was hunted

The shop's own rules, as an owner would state them (nothing here is a Medusa test or a documented
Medusa guarantee; each is a check in `hydo/checks/`):

| check | the rule | how it is read |
|---|---|---|
| `final_acked_orders_exist` | a checkout the storefront saw acknowledged has its order | `$HYDO_ACKED` (cart ids) against `order_cart` in PostgreSQL |
| `final_stock_not_oversold` | never more hoodies sold than stocked; units sold = units reserved = the level's reserved count | three SQL sums over the order and inventory modules |
| `final_orders_paid_once` | every live order has exactly one live payment, its PaymentIntent was created at Stripe and not cancelled there | SQL (order -> payment collection -> payment `data.id`) against the fake Stripe's record |
| `final_no_hold_without_order` | no customer's money stays held at Stripe without an order: every intent Medusa authorized belongs to a live order or was cancelled | the record's authorizations minus the orders' intents minus cancellations |
| `eventually_money_matches_stripe` | within 120 s after the faults end, Medusa's books and Stripe agree: captured on both sides or neither, the same number of refunds, never more refunded than captured | per payment, SQL counts and sums against the record's captures and refunds |

The workload (`hydo/workload/`): `parallel_driver_checkout` (a guest checkout of the one scarce
variant, 5 in stock, through the store API: cart, line item, address, shipping, payment
collection, Stripe payment session, complete; a complete without an answer is clicked again up to
twice, as a customer would) and `parallel_driver_capture_refund` (the back office: capture one
order's payment and refund 10 of it through the admin API). Both are one Node.js client,
`helper_client.mjs`, run with the shop image's own `node` (`hydo-oci medusa node ...`).
`first_seed` creates the store with Medusa's own workflows (`app/src/scripts/hydo-seed.ts`) and
an operator account.

## The project

| file | what it is |
|---|---|
| `docker-compose.yaml` | the shop as its team runs it, after Medusa's Docker guide: `postgres:15-alpine`, `redis:7-alpine`, the Medusa backend built from `app/`, migrations before the server starts |
| `app/` | the backend image: Medusa's starter (`medusa-starter-default` at a pinned commit, Medusa 2.18.0) with a production `medusa-config.ts` (Redis event bus, workflow engine and locking; the Stripe provider; admin dashboard off) and the test's seed script; `build-app.sh` builds it with Docker and saves it into `hydo/images/` |
| `hydo/hydo.yaml` | from `hydo compose ../docker-compose.yaml --no-resolve`, hand changes marked `hand:`; since Milestone 178 the fake Stripe has `tls: {hosts: [api.stripe.com]}` and Medusa `writable: true` (until then `hydo/tls/` held a hand-made test CA, a TLS front ran as the service `stripe_tls`, and Medusa had 15 tmpfs mounts: "Milestone 178" below) |
| `hydo/workload/`, `hydo/checks/` | the commands and checks above; `helper_sql.sh` holds the SQL and record helpers |
| `hydo/known-errors.yaml` | the error kinds every boot prints (Redis's start-up warnings) |

Build and run (Linux, the KVM engine; Docker only to build the shop's image):

```sh
sh app/build-app.sh                               # medusa-shop:hydo -> hydo/images/ (network)
cd hydo && sh fetch-images.sh                     # postgres, redis by digest (network)
hydo build hydo.yaml
hydo run --backend kvm "disk=none | run"          # twice: the same fp=
hydo explore --backend kvm --seeds 1-20 --strategy process --protocol-faults --workers 2
```

The Medusa image is pinned by its id (`image_ref:`); `build-app.sh` resolves the npm dependencies
again on every build, so a rebuild gives other bytes and needs the new id in `hydo.yaml`.
`hydo.yaml` needs a `hydo` with this example's bracketed-name fix (hydo 0.1.0 refuses
`form:metadata[session_id]`; `header:Idempotency-Key` works there too, since Medusa sets the
key to the session id), and, since Milestone 178, a `hydo` that knows `tls:` and `writable:` with
`hydo-world-tls` next to its guest files (`HYDO_WORLD_TLS=1 hydo/crates/hydo-world/build.sh`;
the releases up to 0.2.0 do not ship it).

## Stripe over HTTPS: what a user had to build before Milestone 178

Medusa's Stripe provider (`@medusajs/payment-stripe`, stripe-node) calls
`https://api.stripe.com/v1/...`, checks the certificate, and has no option for another base URL.
hydo's fakes spoke plain HTTP on an address of their own. What it took (since Milestone 178 one
line does it: "Milestone 178" below):

1. A test CA and a leaf certificate for `api.stripe.com` (`openssl`, 4 commands). The leaf must be
   valid at the **guest's** clock, which starts at 2026-01-01 and can be stepped by `T=`: a
   certificate made today (2026-11-07) is "not yet valid" in the guest, so the certificates were
   made under `faketime 2025-01-01` with 20 years of validity (OpenSSL 3.0 has no `-not_before`).
   The key is committed, so images stay byte-reproducible.
2. A TLS front: the service `stripe_tls` from `alpine/socat` (a 4 MB image, pinned), `socat
   OPENSSL-LISTEN:443,fork,cert=...,key=... TCP:10.89.0.1:9101` (the fake).
3. `/etc/hosts`: `api.stripe.com` to the front's address.
4. Trust: `NODE_EXTRA_CA_CERTS=/hydo/tls/ca.pem` in Medusa's environment and the CA mounted into
   its image. Every runtime has its own variable (Go and OpenSSL: `SSL_CERT_FILE`, Python requests:
   `REQUESTS_CA_BUNDLE`, Java: a keystore), and a program that pins Stripe's certificate cannot be
   faked this way at all.

About 25 lines of `hydo.yaml`, three files and one more image; nothing in hydo's documentation
says how. The front is itself a service, so the explorer kills and pauses it (that models
"Stripe unreachable", which is a fair fault) and its traffic is not under `H=` (the rules act
behind it, at the fake).

**The minimal fix in hydo** (built in Milestone 178, as described here, with the trust added to
the bundles' rebuild directories and `NIX_SSL_CERT_FILE`): `fakes[].tls: {hosts: [api.stripe.com]}`. hydo-build
makes a CA and a leaf for those names with a key derived from the manifest (reproducible images),
valid from before `guest.clock_start` for decades; hydo-world terminates TLS itself (rustls) on
443 at the fake's address; `/etc/hosts` maps the names to it (the bridge's hosts file already maps
fakes' names); the CA is appended to the guest's trust bundle and to every image root's
(`/etc/ssl/certs/ca-certificates.crt`, which Go, OpenSSL, curl and Python's `certifi`-less clients
read), and `NODE_EXTRA_CA_CERTS`, `SSL_CERT_FILE` and `REQUESTS_CA_BUNDLE` are exported to every
service. The `H=` rules and the record are unchanged, because they act after TLS.

## Without faults

(The values of this section and of "What was found" are those of the M174 image, with the TLS
front and the tmpfs mounts; the image of Milestone 178 has its own, below.) The final image (`explore-ob-medusa-disk.img` sha256 `ee5bd528…`, built by this tree's `hydo`),
`disk=none | run`, twice: OK `8ef4e08e458e310b` (ctl `67b938a198eb2492`), 20
commands: 5 checkouts ordered (the stock), the rest refused for stock at the cart or at
"complete", 6 back-office capture-and-refund operations, every check holding (5 orders paid once,
3 captures and 6 refunds as in Medusa's books).

## What was found

### 1. Two refunds of one order at once duplicate the order's items (a Medusa bug)

Found blind by the first exploration (seed 2) and again, independently, by a later one (seed 1).
The check `final_stock_not_oversold` reported `oversold: sold=6 stocked=5 reserved=5
reservations=5`: the orders hold six hoodies, five exist, five are reserved. No order was placed
twice and no stock was taken twice. What happened, read from the database by a debug check (on a
debug image, where an exploration hit the same race; friction item 12 says why not on the
original run):

- two back-office refunds of the same order ran at the same time (two operators, or one
  double click; the workload's `parallel_driver_capture_refund` picks orders by seed, so two of
  them sometimes pick the same one);
- Medusa records a refund as an order change (a credit line) that makes a new version of the
  order. Each refund made its own change **with the same version number** (`order_change` has two
  rows for `(order, 3)`, created in the same millisecond), and confirming both wrote the order's
  line items twice at that version (`order_item` has two rows for the same item at version 3);
- from then on the order reads two hoodies instead of one. The fulfilment, totals and returns of
  that order work from the duplicated version, and the shop's stock no longer adds up (units
  sold > units reserved, and > stock when the shop was sold out).

Medusa does refuse a second order change while one is active (its message "... already has an
existing active order change"; refunds in the same runs were answered `400 Order (...) already
...`), so this is a check-then-act race: both requests passed the check before either change
existed. The state is permanent: the debug check
polled for 100 s after the faults and nothing changed. No fault is needed in principle; the
exploration's schedule needed a 10 s delay of one Stripe answer (`H=stripe.intent_create@2-3:
delay=10000,p=25`), which shifted the back office's requests until two refunds of one order
overlapped. Not checked against Medusa's issue tracker.

Minimal schedule (the explorer, 40 shrink runs; one protocol fault and the composition):

`disk=none dseed=d7071eb1f9d436b9 | run H=stripe.intent_create@2-3:delay=10000,p=25 W=:p2,r1000,cparallel_driver_checkout+parallel_driver_capture_refund,0.0.0.1979818538,1.0.1.313146279,2.1.0.1550708684,3.1.1.662703717,4.2.1.1656755180,5.2.1.1259629270,6.3.0.533815299,7.3.1.567436972,8.4.0.999925645,9.5.1.1598692993,10.5.1.1338576782,11.6.1.586489007,12.7.1.750492478,13.8.0.1968007426,14.8.1.784587567,15.9.1.1106177643,16.10.1.1206736569,17.10.0.2003773831,18.11.1.1586374655,19.11.0.373245736`

On the first exploration's image (the release's `hydo`, before the checks below were corrected)
it gave `VIOLATION [stock_not_oversold] oversold: sold=6 stocked=5 reserved=5 reservations=5`,
`48a6e34894db2924` twice. The final image differs (its checks changed), and with it the guest's
timing: there the same schedule passes (`3af77c857191b54b`, twice), and the race is hit instead by
the first exploration's seed 1, a power cut with a kill and a pause, which on the final image
gives `VIOLATION [stock_not_oversold] units sold and units reserved differ: sold=5 stocked=5
reserved=4 reservations=4`, `4904993815ecb0e2` (ctl `b5eca9720ea02931`) twice:

`disk=cut dseed=4127cc1347fae6ba | run K=medusa@0+2 N=redis-medusa@10-11:loss,72% C=12 P=redis@3+2 T=7:30 W=3e71cf9e,p2,q2,r2000`

The second exploration (on the final image) found it at seed 1 and shrank it to a kill of Medusa
at the start and a pause of Redis, no power cut and no Stripe fault (53 shrink runs), twice
`be3b9a6efa1c155a`, `VIOLATION [stock_not_oversold] units sold and units reserved differ: sold=5
stocked=5 reserved=4 reservations=4`:

`disk=none dseed=4127cc1347fae6ba | run K=medusa@0+2 P=redis@3+2 W=:p2,r2000,cparallel_driver_checkout+parallel_driver_capture_refund,0.0.0.1390654497,1.1.0.1862583854,2.2.1.483306153,3.2.1.1880567347,4.3.0.1274133211,5.4.0.2053632671,6.5.0.152079527,7.6.1.1668359829,8.6.1.671048092,9.7.1.896536366,10.7.1.2034059350,11.8.1.1058179917,12.8.1.2098012135,13.9.1.794156849,14.9.1.1878071759,15.10.0.531965259,16.11.1.940608456`

The faults only move the back office's requests in time; what the schedule needs is two
refunds of one order at once (rounds 6 to 9 run two `parallel_driver_capture_refund` each).
A dedicated reproducer without faults (two refunds of one order started in the same round) would
show it directly; it was not written.

### 2. A capture that took effect and was answered 500 is not in Medusa's books

`H=stripe.intent_capture@0-12:status=500,effect=yes` (Stripe captured the money and then
answered 500): Medusa records no capture, the operator sees the order unpaid, and Stripe holds
the captured money (`eventually_money_matches_stripe`: "captured at Stripe, not in Medusa").
Medusa sends the capture with an idempotency key (the capture id) and its own retry gets the
stored 500 again, as Stripe replays a stored answer. **This is partly an artifact of the fake**:
in production Stripe also sends `payment_intent.succeeded` to Medusa's webhook endpoint, and
Medusa's Stripe provider maps that event to a successful capture, so the books would heal; the
fake cannot send Stripe's signed webhooks (What hydo lacked, item 5). What remains real is that
Medusa treats a 5xx from a capture as "not captured" rather than "unknown", so until the webhook
arrives, or when it is lost, the books are wrong.

On the final image, twice: `VIOLATION [money_matches_stripe] not satisfied 122.36s after heal,
deadline 120s (last check rc=1: 3 disagreements between Medusa and Stripe over 5 payments)`,
`01ff126583889ab6` (ctl `5d3d0a44fa00a787`).

### 3. Not a finding: Medusa's books show a refund before Stripe has made it

The first exploration's second failure (seed 15, shrunk to a one-way loss of the TLS front's
answers to Medusa, `O=stripe_tls-medusa@8-12:resp:loss,81%`) was, at the end of the workload,
"3 refunds in Medusa, 1 at Stripe". Medusa writes a refund (or capture) row, then calls Stripe,
and deletes the row if the call fails; with the answers being lost the calls were still waiting
(stripe-node waits up to 80 s), and 10 to 30 s later the books and Stripe agreed again. The check
was a `final_` check and so judged a moment, which was wrong for a property that may lag: it is
now `eventually_money_matches_stripe` (within 120 s after the heal), and the same schedule passes.
A kill of Medusa inside that window would leave a refund in the books that Stripe never made;
no schedule of this session hit it. (The schedule below acts on the link to the TLS front
service, which the example no longer has since Milestone 178: the fake is reached on the bridge's
own address, and `H=stripe...` rules, which act after TLS, delay or lose its answers instead.)

On the final image, twice: OK `859b807cb16aed51` (ctl `1ed6608c41421120`):

`disk=none dseed=b988c2bd3620cc45 | run O=stripe_tls-medusa@8-12:resp:loss,81% W=:p2,r500,cparallel_driver_capture_refund+parallel_driver_checkout,0.0.0.771517841,1.1.1.1576723965,2.2.0.470640582,3.3.1.330295472,4.4.0.1313715018,5.5.1.1543865592,6.6.0.19579900,7.7.0.1883030719,8.8.1.1639347118,9.9.0.921056528,10.10.0.1626536734,11.11.0.1364476330`

### Not found

- `final_acked_orders_exist`, `final_orders_paid_once` and `final_no_hold_without_order` held in
  every run: no acknowledged order lost (PostgreSQL with its defaults survives every power cut),
  no order paid twice, and no money held at Stripe for a checkout that did not become an order,
  also under kills of Medusa during checkout. Medusa's cart completion is idempotent: a
  customer's second click on "complete" after a lost answer returned the same order.
- A real oversell (more units ordered than stock by separate checkouts) did not happen: the
  inventory module refused the sixth checkout at the cart or at "complete" in every run.
- Left open by design: a checkout refused at "complete" for stock leaves its PaymentIntent at
  Stripe uncancelled (every run without faults has one). Medusa never authorized it, so the check
  does not count it; with Stripe's real card flow the customer confirms the payment in the
  browser before "complete", so that intent holds the customer's money until Stripe's
  authorization expires. Whether that is a bug depends on the storefront; it is worth a question
  to the shop's owner.

## The explorations

`hydo explore --backend kvm --seeds 1-20 --strategy process --protocol-faults --workers 2
--errors-shrink 0` (kills, pauses, network and one-way faults on the services' links, clock steps,
power cuts with disk faults, and protocol faults at the fake Stripe; every failure re-run, the
first of each signature shrunk and run twice):

| image | runs | wall | failures | signatures |
|---|---|---|---|---|
| first (release `hydo`, `final_money_matches_stripe`) | 20 schedules, 2 re-runs, 40 + 30 shrink runs | 646 s | 2 of 20, both reproduced | `stock_not_oversold` (seed 2, finding 1); `money_matches_stripe` (seed 15, finding 3, a check that judged too early) |
| final (this tree's `hydo`, `eventually_money_matches_stripe`) | 20 schedules, 1 re-run, 53 shrink runs | 753 s | 1 of 20, reproduced | `stock_not_oversold` (seed 1, finding 1); no money disagreement left once the check waits |

A run takes 16 to 40 s of engine time (mean 24.5 s), a worker's boot to its snapshot 25 s; with
two workers on the build machine that is about 110 runs an hour. Every recorded failure re-ran
with the same fingerprint.

New error kinds besides Redis's start-up warnings (accepted in `hydo/known-errors.yaml`), each
written only while a fault was on and none a finding by itself: Medusa's `connect ECONNREFUSED`
and ioredis's "Unhandled error event" and "Reached the max retries per request limit" while Redis
was killed or cut off, `Connection ended unexpectedly` and PostgreSQL's "the database system is
starting up" around a PostgreSQL restart, `connect ETIMEDOUT` under network loss, and "An error
occurred in InitiatePayment during creation of stripe payment intent" under protocol faults on
the intent route. They are left unaccepted, for the shop's owner to triage with `--during`.

## Business rules: what was written by hand, and what hydo could offer

Everything that knows the shop's rules was written by hand: the five checks (136 lines of shell,
SQL and awk) with their helpers (35), the storefront and back-office client (117 lines of
JavaScript), the seed (128 lines of TypeScript, mostly Medusa's starter seed cut down) and the
fake Stripe (50 lines of `hydo.yaml`). hydo itself supplied the faults, the composition of the
two drivers, the record of what Stripe saw, the error-log scan that explained the first failures,
and the shrinking. The checks fall into three kinds, and each kind could be offered:

- **"acknowledged implies stored"** (`final_acked_orders_exist`): already a one-liner with
  `hydo-check subset` against `hydo-read postgres <dsn> "<query>"`, which takes any query, the
  cart-to-order join included; it was written in shell here because the other checks were. The
  guide could show such a source for an application whose tables an ORM made.
- **"our books equal the provider's"** (`final_orders_paid_once`, `final_no_hold_without_order`,
  `eventually_money_matches_stripe`): the most valuable and the most work. With a `world` source
  for `hydo-read` (the record's effects per route, by ref) and the record keeping chosen request
  fields (amount, currency), each becomes one `hydo-check equal` or `subset` between a SQL query
  and a route: "every captured payment in the books was captured at the provider and the other way
  round". A ready-made Stripe fake would also know which routes take money.
- **"the books are consistent with themselves"** (`final_stock_not_oversold`): units sold, units
  reserved and the inventory level's counter. This is domain knowledge hydo cannot guess; a skill
  section with commerce invariants to ask the owner about (sold <= stocked, sold = reserved,
  refunded <= captured, one payment per order, no order without payment, no hold without order)
  would have saved the thinking, not the SQL.

## Friction log: what an outside user needed and where they looked

Each item: what was needed, where it was looked for, and what was done. Items marked FIXED were
changed in hydo in their own commit, with a test.

1. **A local image in the Compose file.** The shop's backend is `build: ./app` with
   `image: medusa-shop:hydo`, as most applications' Compose files are. `hydo compose` tried to
   resolve that tag at Docker Hub and stopped (`HTTP 401`); `--no-resolve` imported it, but then
   no image was pinned, and the generated `fetch-images.sh` would download the shop's image from
   Docker Hub (it does not exist there) and every arm64 image too (on a Linux machine). The images
   were fetched by hand and `fetch-images.sh` rewritten with digests taken from a second import
   without the shop. Not fixed: a service with `build:` should not be resolved; its `image:`
   should point at the file `docker save` writes, and `fetch-images.sh` should say so.
2. **Why a service did not start.** The first run ended `ERROR service start (medusa)` and nothing
   else: the run directory has the console and the control transcript, not the service's own
   output. The guide's hazards and the skill say to "read the console files that `hydo run`
   names", which do not contain it. The service's command was wrapped in
   `(...) 2>&1 | tee /dev/console` and the image rebuilt, three times (Medusa, then PostgreSQL),
   to learn the three causes below. Not fixed (it changes the controller's transcripts): on a
   start or restart failure the controller could print the last lines of `/tmp/hydo/<svc>.log`.
3. **A read-only image root.** Medusa's CLI writes `~/.config/medusa` (configstore), and
   `medusa db:migrate` writes an empty schema snapshot into `node_modules/@medusajs/<module>/
   dist/migrations/` of every module that has none. In Docker the container's writable layer
   takes both; under hydo the image root is read-only and the error is `ENOENT` (not `EROFS`). Fix
   by hand: a tmpfs on `/root` and fourteen tmpfs mounts, one per module, found by trial (two
   rebuilds; the list came from `docker run` of the image). The guide's hazards say only "the
   root filesystem is read-only"; an image that writes into its own install directory is not
   mentioned. FIXED in Milestone 178: `writable: true` gives an image service a container's
   writable root (its layer on the data disk); the 15 mounts are gone.
4. **Redis took PostgreSQL's files.** Next, every connection to PostgreSQL failed with
   `could not open file "global/pg_filenode.map": Permission denied`. The import had mapped
   Redis's `redis_data:/data` to the data disk's root ("shared by every service"), and the Redis
   image's entrypoint chowns everything under `/data` to its own user, PostgreSQL's directory
   included. Found with a debug command that listed the data disk from the guest (uid 999 on
   PostgreSQL's files). FIXED: `hydo compose` gives such a volume its own directory on the data
   disk when another service keeps a volume there (commit "hydo compose: a volume on /data gets its
   own directory ..."). Left: an image whose working directory is `/data` without any volume
   still sees the shared root.
5. **Stripe over HTTPS.** The section above: a TLS front, a test CA valid at the guest's clock,
   `/etc/hosts`, `NODE_EXTRA_CA_CERTS`. Nothing in `hydo guide outside-services` says what to do
   when the provider's SDK cannot be pointed at plain HTTP, which is the normal case. FIXED in
   Milestone 178: `tls: {hosts: [api.stripe.com]}` on the fake, and the guide's topic says so.
6. **Stripe's form fields.** stripe-node sends `metadata[session_id]=...`; `ref:
   form:metadata[session_id]` was refused ("is not an extractor"). FIXED (commit "Fakes:
   bracketed form and query names are extractors"); before the fix the idempotency key, which
   Medusa sets to the session id, served as the ref.
7. **A stateless fake.** A refund failed inside Medusa (`Cannot read properties of undefined
   (reading 'toUpperCase')`): Medusa reads the intent's `currency` from what Stripe answered at
   authorization, and a fake's answer to `GET /v1/payment_intents/{id}` can only use the request,
   not the intent created earlier. Every answer about an existing intent now names `eur` by hand.
   Found through `errors: true`, which printed Medusa's error line as a new error kind: the most
   useful debugging aid of the whole onboarding.
8. **Command output.** A workload command's output stays in the guest
   (`/tmp/hydo/compose/log.<entry>`); the run directory does not have it. The drivers write one
   short line each to `/dev/console`, as the NATS example's commands write their records.
9. **Running the client.** The test client is JavaScript and the only Node.js in the guest is the
   shop image's. `hydo-oci medusa node /hydo/workload/helper_client.mjs` works once the workload
   folder is mounted into the image (`{src: /hydo/workload, dst: /hydo/workload}`); nothing says
   that a command can use an image's interpreter this way. A consequence: the client runs inside
   Medusa's network namespace, so `N=`/`O=` faults on Medusa's port do not touch the client's own
   requests (the faults on Medusa's links to PostgreSQL, Redis and the TLS front still do).
10. **Seed data.** The store (region, stock location, shipping, a publishable key, a product with
   stock) is made by Medusa's workflows in a script compiled into the image (`medusa exec`), as the
   starter's seed is; the test client reads the ids from `/data/shop/seed.json`. A first command
   (`first_seed`) ran it once into the snapshot, with no friction.
11. **Error kinds at start.** With `errors: true` every run reports Redis's five start-up warnings
   as new kinds until they are accepted (`hydo/known-errors.yaml`). `hydo errors accept` writes
   the login name of whoever accepts into the file (`by:`), which then goes into a public
   repository; it was replaced by hand.
12. **Reading the state behind a violation.** The oversell's verdict says only the counts. To see
   which order and which rows, a debug check that dumps the tables to the console was added and
   the image rebuilt; but any change of the image changes the guest's timing, and the minimal
   schedule then passed (the race did not happen). The explanation came from a second exploration
   of the debug image, where another schedule hit the same race. A check's earlier output lines
   (the ids) are not in the run directory either: only its last line reaches the verdict and the
   `HYDO-ASSERT` record. Not fixed.
13. **A check that judged a moment.** `final_money_matches_stripe` reported books that disagree
   with Stripe while Medusa's calls to Stripe were still waiting (finding 3). The guide's checks
   page explains `eventually_`, but nothing warns that a cross-system comparison needs it; the
   check was changed to `eventually_` after the debug runs.
14. **KVM.** Nothing. Medusa (Node.js 22), PostgreSQL, Redis and socat ran on the KVM engine
   without a change, each verdict twice with one fingerprint; a run takes 40 to 50 s of wall
   time, as the login user, with no module and no root.

## What hydo lacked, ranked by how much it blocked

1. **The reason a service failed to start.** Four of the first five runs ended in
   `ERROR service start (medusa)` with no cause in the run directory; each cause cost a rebuild
   with a `tee /dev/console` wrapper. Printing the last lines of the service's log with the
   verdict would have saved most of the time to the first green run.
2. **TLS for fakes.** Every real provider is HTTPS with certificate checks and an SDK that cannot
   be pointed elsewhere; a user must build a CA, a front and trust settings per runtime (section
   "Stripe over HTTPS"). The minimal fix is described there: a `tls:` list of host names on a
   fake, a generated CA installed into the guest and every image root, termination inside
   hydo-world. BUILT in Milestone 178 (below).
3. **Looking at a run's final state without changing the image.** Explaining finding 1 needed
   the database rows behind the verdict; adding a query to the image changed the timing and lost
   the reproduction (friction item 12). A way to run a read-only command against a finished run
   (the data disk kept with the run directory and `hydo inspect <run dir> -- psql ...` in a VM
   restored from the run's end, or an `--after` command that is not part of the image) would have
   explained it in one step.
4. **A writable layer for image services.** Applications write into their own install
   directories (Medusa: `~/.config`, migration snapshots in `node_modules`), which Docker allows
   and hydo's read-only image root does not. Fifteen tmpfs mounts found by trial. An opt-in
   overlay (`writable: true`, upper directory on a tmpfs) would make an image behave as in Docker.
   BUILT in Milestone 178 as `writable: true`, but as a full copy of the image on the data disk
   rather than an overlay (no guest kernel of every engine has overlayfs), so it survives a kill
   and a power cut (as far as flushed) as a container's layer does.
5. **A payment-provider fake with memory and signed webhooks.** hydo-world answers from the
   request only, so a retrieved object cannot carry what was created (the currency had to be hard
   coded; amounts are not returned at all), and its webhooks cannot carry Stripe's
   `Stripe-Signature` (an HMAC of the timestamp and body with the endpoint secret), so Medusa's
   webhook endpoint would reject them. Without webhooks the books never heal after a lost
   answer, which is part of why finding 2 appears (in production
   `payment_intent.succeeded` would mark a capture whose answer was lost). Objects that keep their
   fields across calls, a status per object, and a webhook signature template would make a Stripe
   fake writable once; a ready-made one for the common providers would make it free.
6. **Checks across a store and the outside world.** All five business checks are hand-written
   SQL plus awk over hydo-world's record (136 lines, plus 35 lines of helpers). `hydo-read` reads
   PostgreSQL, but the fake's record is not a `hydo-read` source, so `hydo-check` cannot compare
   "captured at Stripe" with "captured in Medusa"; and the record keeps no request fields
   besides the ref and the idempotency key, so a refund's amount cannot be checked against the
   capture. A `world` source for `hydo-read` (`hydo-read world stripe.intent_capture --effects`)
   and a `record:` list of extra fields per route would turn three of the five checks into one
   line each.
7. **Importing a Compose file with a local build.** `build:` services stop the import or, with
   `--no-resolve`, unpin every image (friction item 1).
8. **Command output in the run directory.** Workload commands' logs stay in the guest; only what
   they write to the console comes out.

## Milestone 178: the fake Stripe over `tls:`, Medusa with `writable: true`

Two of the gaps above were built into hydo (MANIFEST.md "HTTPS for fakes and proxies" and "A
writable root for image services") and this example was rewritten to use them. What the manifest
lost: the service `stripe_tls` (the `alpine/socat` image), the folder `hydo/tls/` with its
hand-made CA, three `root.files`, two `/etc/hosts` lines, `NODE_EXTRA_CA_CERTS`, the mount of the
CA and the 15 tmpfs mounts of Medusa (`/root` and 14 migration folders). What it gained, two
lines:

```yaml
  - name: medusa
    writable: true                     # Medusa writes ~/.config and node_modules, as in Docker
fakes:
  - name: stripe
    tls: {hosts: [api.stripe.com]}     # HTTPS on 10.89.0.1:443 for the name stripe-node calls
```

hydo-build makes the image's test CA and a certificate for `api.stripe.com`, maps the name to the
bridge's address in `/etc/hosts` (the manifest's own hosts file gets the line appended), appends
the CA to the system bundle of every image (Medusa's Alpine `/etc/ssl/certs/ca-certificates.crt`
among them) and sets `NODE_EXTRA_CA_CERTS`, which stripe-node's runtime reads; `hydo-world-tls`
terminates TLS 1.3 at `10.89.0.1:443` and hands the requests to the fake. Medusa's tree (1152 MiB
with the layer's margin) is on the data disk, `/data/.hydo/layers/medusa`, bound over its root.

Everything on the build machine with the KVM engine, the tree's `hydo` over the Linux release
candidate's guest files, plus `hydo-world-tls` (sha256 `63320b8c…`, the same bytes built on this
Mac and on the bare-metal machine). The shop's image was rebuilt (`app/build-app.sh` resolves
the npm dependencies again: image id `5b952a43…`, so every fingerprint below is new); the hydo
image was built on the bare-metal machine twice with the same bytes (`explore-ob-medusa-disk.img`
`9f0668e4…`, root 576 MiB, data 2432 MiB), each schedule twice where it says so:

| schedule | verdict | fp | ctl |
|---|---|---|---|
| `disk=none \| run` (twice) | OK, 20 commands, "5 payments: 3 captures and 6 refunds at Stripe, as in Medusa" | `4b07c58beaa6fd5c` | `bd824264a88d5309` |
| `disk=none \| run H=stripe.intent_capture@0-12:status=500,effect=yes` (twice; finding 2) | VIOLATION [money_matches_stripe] 3 disagreements over 5 payments | `2d8daafea4e389f4` | `bfd0428f6dd0947c` |
| the first exploration's seed 1, `disk=cut dseed=4127cc1347fae6ba \| run K=medusa@0+2 N=redis-medusa@10-11:loss,72% C=12 P=redis@3+2 T=7:30 W=3e71cf9e,p2,q2,r2000` (twice; finding 1) | VIOLATION [stock_not_oversold] sold=5 stocked=5 reserved=4 reservations=4 | `ddabd0b859a810ba` | `a073a27d7dbcc7a6` |
| the second exploration's minimal schedule (`K=medusa@0+2 P=redis@3+2 W=:...`, above) | OK (the guest's timing moved: no two refunds of one order overlap) | `9a422bb2617ca8fd` | `0b2fa1b8ae37e941` |
| `disk=none \| run K=medusa@4+2` (the layer kept across a kill; Medusa's restart runs its migrations again into it) | OK | `e40d19bf1c78a266` | `77bdd8327420410a` |
| `disk=cut \| run C=8` (the recovery boot binds the layer as the disk kept it) | OK | `8c803ee45b5e6599` | `338345fe497d24c1` |
| `disk=cut \| run K=medusa@2+2 C=8` | OK | `da0d184080b17c88` | `46d8e22ac7588b05` |

A run takes 38 to 57 s, as before. Both findings were also found blind again
(`hydo explore --backend kvm --strategy process --protocol-faults --workers 2 --errors-shrink 0`):
seeds 1 to 20 found nothing (245 s); seeds 21 to 80 (1176 s, 184 runs an hour) found three
failures, all reproduced:

- `money_matches_stripe` (seeds 52 and 64; finding 2's family), shrunk in 90 runs to
  `disk=none dseed=4745106db7fdc25e | run H=stripe.intent_capture@0-12:reset H=stripe.intent_cancel@10-12:status=500 W=:p2,r500,cparallel_driver_capture_refund+parallel_driver_checkout,0.0.0.2116393074,1.1.0.1987793828,2.2.1.647859549,3.3.0.1440783945,4.4.1.54239457,5.5.1.1670639316,6.6.0.477404641,7.7.1.1408698898,8.8.1.902105084,9.9.0.1145656850,10.10.1.1175202143,11.11.0.1004297808`,
  twice `e09a37fec3a25f27`: a capture whose connection is reset after Stripe acted (`reset` keeps
  the effect) stays out of Medusa's books, as a 500 does;
- `stock_not_oversold` (seed 69; finding 1), shrunk in 46 runs to
  `disk=none dseed=a7b6a8effd8b1e8d | run P=medusa@1+1 T=3:300 O=postgres@6-7:req:loss,48% H=stripe.intent_get@1-3:status=429,retry-after=2,p=25 W=:p2,r2000,cparallel_driver_capture_refund+parallel_driver_checkout,0.0.0.46293696,1.0.0.753303122,2.1.1.18134727,3.2.0.704739967,4.3.1.486965657,5.4.0.1153323400,6.5.1.780701343,7.6.1.1262413356,8.6.0.1056841611,9.7.0.392360314,10.8.0.101060845,11.8.0.349740133,12.9.0.351350258,13.10.0.58512317,14.10.0.1330516783,15.11.0.1880335487`,
  twice `ea66d1122593413e` (`sold=5 stocked=5 reserved=4 reservations=4`): the faults move the
  back office's requests until two refunds of one order overlap.

The exploration's fault space is smaller by the TLS front's service and links (`stripe_tls`,
`stripe_tls-medusa`); faults on the way to Stripe are now only the `H=` rules at the fake (they
act after TLS) and the network faults of Medusa's own link.
