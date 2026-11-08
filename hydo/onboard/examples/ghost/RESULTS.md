# Ghost: results

The second round of onboardings (user decision, 2026-11-08) measures whether the fixes of the
first round (Milestones 178 to 184) made onboarding a business application easier, and finds
what is still missing. The project: **Ghost 6.75.0** (open-source publishing and newsletters,
Node.js, MySQL 8), run unmodified from its official Docker image, with both of its outside
providers called through their official SDKs over HTTPS:

- **Mailgun** (newsletters): Ghost's `mailgun.js` client posts each batch of recipients to
  `https://api.mailgun.net/v3/<domain>/messages`; Ghost's own mail (staff notifications) goes the
  same way through `nodemailer-mailgun-transport`.
- **Stripe** (readers' donations, Ghost's "tips"): Ghost's `stripe-node` registers a webhook
  endpoint and a billing portal configuration when the operator enters the keys, creates a
  product and a price at the first donation, creates a Checkout session per donation, and records
  the donation when Stripe's signed `checkout.session.completed` webhook arrives.

**Why Ghost.** It is widely deployed, and its core flows depend on a third-party HTTPS API: a
newsletter is only sent when Mailgun has accepted it, and a donation only exists when Stripe's
webhook says so. It has a documented Docker setup with a database, and its business rules are
easy to state and worth checking: each issue reaches each subscriber once, nobody who left gets a
later issue, every batch Ghost records as sent reached the provider, every paid donation is in
the books exactly once. It is not a shop (the first round had Medusa), it runs in two containers
(fast to iterate on), and its newsletter sending was rewritten in 2026 (resumable sends after a
crash, recipient accounting), so a fault test has something to say about it. The two providers
have different shapes: Mailgun is an API Ghost calls and never hears back from; Stripe's results
arrive by signed webhook.

**Where it ran.** Every run and exploration ran on **the build machine with the KVM engine**
(`--backend kvm`, `/dev/kvm` opened by the login user, no root, no kernel module). The images
were built on the bare-metal machine and copied to the build machine, because the build machine
had 7 to 16 GB free (shared with other sessions) and a build writes the root tree and three
image files (the disk image alone is 3.7 GB); only one image (1.7 GB on disk, sparse) was kept
there. The `hydo` is this tree's (`main` at
`d7c58e65`): a Linux release built from it with `hydo/dist/linux/build-release.sh` and installed
with `install.sh --no-module`, plus `guest/hydo-world-tls` and `guest/hydo-sql`, which the
release does not ship yet (open items 58 to 60). On `main` the release script itself stops:
`guest/hydo-world`'s pin in `hydo/dist/linux/guest-programs.sha256` is still 0.2.0's, and the
tree's `hydo-world` has other bytes since Milestone 181 ("a guest program built here is not its
pinned build"). Open item 59 says the tree's releases "still ship 0.2.0's hydo-world"; in fact
the script refuses to build. The copy used here accepted the tree's own `hydo-world` in its pin
file (not committed); this example uses `tls:`, so its images stage `hydo-world-tls` anyway.

## The short answers

- **Did the first round's fixes remove its friction?** Mostly. HTTPS to both providers was two
  `tls:` lines and worked at the first run with the three Node.js libraries involved; no image
  needed a writable root or a tmpfs; the first run of the first image passed; the business checks
  are SQL files and one-line comparisons between MySQL and the fake providers' record; signed
  webhooks and fake objects let the real Stripe flow run. Where a run failed for a reason the
  verdict did not give, `hydo inspect` found the cause in one or two calls without rebuilding.
- **What was found?** One real defect, already known upstream: **a donation is recorded once per
  webhook delivery**, so Stripe's retries and duplicate deliveries record one payment several
  times (Ghost pull request 30118, open, describes it and is not in 6.75.0). Without any fault,
  with Ghost's Docker defaults (no mail server configured), every donation was recorded six times.
  And a property by design: **a newsletter batch whose acceptance answer is lost goes out again**
  (Mailgun has no idempotency key; Ghost retries a failed send up to six times). No check of Ghost's
  crash-resumable sending failed under the kills, pauses, power cuts and clock steps tried,
  though few of them landed inside a send.
- **What is still missing?** In order: the diagnosis of a service that dies in the recovery boot;
  request fields beyond the reference in the fake's record (Mailgun's multipart body is not read
  at all); and a fingerprint difference on the KVM engine that no recorded level explains ("What
  hydo lacked", below).

## What was hunted

The site owner's rules (none is a Ghost test or a documented Ghost guarantee); each is a check in
`hydo/checks/`:

| check | the rule | how it is read |
|---|---|---|
| `final_acked_issue_has_newsletter` | a publication Ghost acknowledged has its newsletter | `hydo-sql check subset`: `$HYDO_ACKED` (post slugs) against posts joined with emails in MySQL |
| `final_one_copy_per_member.sql` | a member is a recipient of one issue at most once | SQL over `email_recipients` |
| `final_sent_counts_add_up.sql` | an issue marked sent counts the recipients its batches hold | SQL over `emails` and `email_recipients` |
| `final_no_send_after_unsubscribe` | a member whose unsubscribe Ghost acknowledged before an issue's publication started is not among its recipients | the clients' own order log against SQL (awk; the first version used Ghost's timestamps and was wrong, "False findings" below) |
| `eventually_newsletters_settle.sql` | within 180 s after the faults no issue is still pending or submitting | SQL over `emails` |
| `eventually_sent_batches_reached_mailgun` | every batch Ghost recorded as sent reached Mailgun | `hydo-sql check subset`: Ghost's per-batch Mailgun message ids against the fake Mailgun's record |
| `eventually_each_batch_sent_once` | Mailgun accepted at most one message per batch | the number of batches sent or being sent (SQL) against the number of messages Mailgun accepted (the record) |
| `eventually_donation_recorded_once` | every donation Stripe completed is in Ghost's books exactly once | `hydo-sql check exactly-once`: the fake Stripe's completed checkout sessions (by donor e-mail) against `donation_payment_events` |

The workload (`hydo/workload/`): `parallel_driver_publish` (an editor publishes a post with a
newsletter to every member), `parallel_driver_audience` (a member joins, or one of the first 15
leaves every newsletter), `parallel_driver_donate` (a reader starts a donation; the fake Stripe
completes the checkout at once and sends the signed webhook), `anytime_retry_failed` (the operator
presses "retry" on every failed newsletter, through a staff session). `first_seed` sets up the
site's owner, the client's Admin API key (Ghost's built-in Zapier integration's), connects Stripe
with API keys (Ghost then registers its webhook at the fake Stripe), and adds 15 members. All are
one Node.js client, `helper_client.mjs`, run with the Ghost image's own `node`
(`hydo-oci ghost node ...`). Newsletters go out in batches of 5 recipients
(`bulkEmail__batchSize`; Mailgun takes 1000, so a 15-member list is one batch in production and
three here).

## The project

| file | what it is |
|---|---|
| `docker-compose.yaml` | Ghost as its team runs it after the official image's documentation: `ghost:6.75.0-alpine`, `mysql:8.0.44`, Mailgun for newsletters and Ghost's own mail, the url `https://...` (Ghost runs behind a TLS proxy; it refuses to boot with Stripe connected on a plain-http url) |
| `hydo/hydo.yaml` | from `hydo compose ../docker-compose.yaml --out hydo.yaml --images-dir images` (no warning), hand changes marked `hand:`; the fakes `mailgun` (3 routes) and `stripe` (8 routes, 2 objects, a signed webhook), both with `tls:` |
| `hydo/workload/`, `hydo/checks/` | the commands and checks above; `helper_sql.conf` from `hydo sql-gen --dialect mysql --endpoint 'db=root:example@tcp(10.89.0.12:3306)/ghost' --workloads none` |
| `hydo/fetch-images.sh`, `hydo/hydo-compose.lock.json` | from `hydo compose`: the two images by digest |

```sh
cd hydo && sh fetch-images.sh            # both images by digest (network, once)
hydo build hydo.yaml                     # 2.5 to 6 minutes; root 2176 MiB, data 1536 MiB
hydo run --backend kvm "disk=none | run"
hydo explore --backend kvm --seeds 1-20 --strategy process --protocol-faults --workers 2
```

Hand changes besides the fakes: `guest.ram_mib: 2048` (the importer guessed 1280 for MySQL 8 and
Node.js; raised before the first run, so whether 1280 is enough was not tried); a readiness check
that asks Ghost's site API (Ghost opens its port while it still boots and answers 503 there); the
workload folder and a test folder mounted into Ghost's image so the client can run with its
`node`; `security__staffDeviceVerification: "false"` (a staff login from a new device waits for
an e-mailed code, which no one reads in a test); the batch size.

### The fake providers, and what the first round's fixes did for them

- **`tls:`** (Milestone 180): `tls: {hosts: [api.mailgun.net]}` and `tls: {hosts:
  [api.stripe.com]}`. No CA, no front service, no trust settings by hand: `mailgun.js`,
  `nodemailer-mailgun-transport` and `stripe-node` all trusted the image's CA through
  `NODE_EXTRA_CA_CERTS` at the first run.
- **Fake objects** (Milestone 181): at every start after the first (after a kill, and in the
  recovery boot after a power cut) Ghost updates the webhook endpoint it registered, by its id
  (`POST /v1/webhook_endpoints/{id}`); `save:` on the creating route and `load:` on the updating
  one answer with the endpoint created earlier, also across a power cut (the objects are carried
  with the fake world). The same is written for the donation product (`GET /v1/products/{id}`),
  which Ghost reads only when a donation price already exists in its database without an active
  product; no run got there.
- **Signed webhooks** (Milestone 181): Ghost verifies `Stripe-Signature` with the secret Stripe
  returned when Ghost created the endpoint (the fake answers `whsec_hydotest`; `sign: {header:
  Stripe-Signature, secret: whsec_hydotest}` signs `{ts}.{body}` as Stripe does). The whole
  donation path ran as in production, and the finding below depends on it.
- **The record as a source** (Milestone 182): the batch and donation rules are one
  `hydo-sql check` line each, comparing a MySQL query with a fake's record. The fake Mailgun answers
  a message with the id `<seq>@mg.example.com`; Ghost keeps it per batch, so the batches it
  recorded as sent can be matched with what Mailgun accepted.

## Without faults

The final image (`explore-ob-ghost-disk.img`, sha256 `6eaa165a…`; the committed folder differs
from it only in one comment line of `checks/final_no_send_after_unsubscribe`, so a rebuild gives
other image bytes and fingerprints), `disk=none | run`, three
times: OK `d5ad06f191aae29c` (ctl `46fe00775c39e8a6`): 25 commands: 11 publications (each to 15
to 19 members, three or four batches), one join and three leaves, one donation, five operator
retries (nothing to retry), and the seed; every check holds. A run takes 47 to 54 s of wall
time (15 to 21 s of guest time); a worker's boot to its snapshot 33 to 43 s.

## What was found

### 1. A donation is recorded once per webhook delivery (known upstream: Ghost pull request 30118)

Ghost's handler of `checkout.session.completed` for a donation saves a `donation_payment_events`
row and then e-mails the staff ("you received a tip"). Nothing remembers which checkout session or
event it has handled, so every delivery of the same event saves another row. Stripe delivers an
event again whenever the endpoint does not answer 2xx, and documents that an endpoint may receive
an event more than once. Three ways to get there, each found by the check
`eventually_donation_recorded_once`:

- **No fault, Ghost's Docker defaults.** The official image has no mail server configured
  (transactional mail goes to `127.0.0.1:587`). The staff e-mail fails after the row is saved, the
  handler answers 500, Stripe retries, each retry saves the row again and fails again: one $5
  donation is in the books six times (the fake retries five times; Stripe retries for up to three
  days). This was the first run with donations, on the image before Ghost's mail was configured
  (`84bce1fc…`), `disk=none | run`, twice `9ed2444c91345b13` (ctl `e40a0329744c1f2e`): `stripe
  d13@example.com is in ghost 6 times`. The cause came from one `hydo inspect logs <run> ghost
  --at end` (57 s, no image change): `Failed to send email. Reason: connect ECONNREFUSED
  127.0.0.1:587` and `"POST /members/webhooks/stripe/" 500`, five times.
- **Mail configured, the mail provider fails.** With Ghost's mail through Mailgun's API (the
  final image), `disk=none | run H=mailgun.transactional@0-12:status=500`, twice
  `6774796b63f455e5` (ctl `3ca7d23c4ac7f5a9`): six times again.
- **Stripe delivers the event twice.** `disk=none | run H=stripe.checkout_session@0-12:dup`,
  twice `e5fec8e77b90eadb` (ctl `d77d996cc66c04d1`): twice.

Found blind by the second exploration (seed 20 of 20), shrunk (81 runs) to one protocol fault: `disk=none dseed=6711e6dcb424bc7a | run H=mailgun.transactional@2-12:status=500
W=:p1,r1000,cparallel_driver_publish+parallel_driver_donate,2.1.0.162907746,4.3.1.877474827,5.3.0.1291858687,7.4.0.1221422008,10.6.0.1116456911,11.7.0.464411223,12.7.1.1506272181,13.8.1.1985160783,15.9.1.2073737124,16.9.1.1145668394,17.10.0.1887964205,19.11.0.563818162`,
twice `39f395eedeb85d71` (five donations, recorded 5 or 6 times each).

Ghost's pull request 30118 ("Fixed unpaid asynchronous donations being recorded", open, its
migration dated 2026-11-05, not in 6.75.0) says: "nothing on the donation path recognised a
session it had already handled. Stripe retries a webhook whenever delivery is not acknowledged, so
one payment could surface as several donations, each with its own staff notification", and adds
the checkout session id with a unique index. So no upstream report was drafted. What hydo adds:
the reproduction without any fault on the documented Docker setup, where a missing mail
configuration turns every donation into six.

The same rule failed once more in the explorer for another reason: shrunk from seed 8 to
`H=stripe.checkout_session@2-3:lost` (the fake never delivers the webhook), twice
`95f0559777187ef7`, "d3@example.com is in ghost 0 times". That is the boundary of Ghost's design
rather than a defect: Ghost learns of a donation only from the webhook and never asks Stripe about
the sessions it created, so a webhook Stripe gives up on is a donation Ghost never records. Real
Stripe retries for three days and lists undelivered events in its dashboard; the check reports the
case as "missing" so the two kinds are told apart.

### 2. A newsletter batch whose acceptance was not heard is sent again (by design)

Ghost's batch sender retries a failed Mailgun request up to six times, ten seconds apart, whatever
the error. Mailgun's API has no idempotency key, so when Mailgun accepted a batch and the answer
did not arrive (a 500 after the effect, a reset connection), every member of that batch gets the
issue again. `eventually_each_batch_sent_once`:

| schedule | verdict | fp | ctl |
|---|---|---|---|
| `disk=none \| run H=mailgun.messages@0-12:status=500,effect=yes` | VIOLATION: Mailgun accepted 44 messages for 36 batches sent or being sent: 8 extra | five runs: `e922311754d0d30c` three times, `c7393aac8517f6ee` twice ("A fingerprint difference", below) | `6a17e61e4563afec` all five |
| `disk=none \| run H=mailgun.messages@0-12:reset` (twice) | VIOLATION: 44 messages for 36 batches: 8 extra | `4fdb7b1ae7053e88` | `7796cf90c324acdd` |
| `disk=none \| run H=mailgun.messages@0-12:drop` (twice) | OK: no batch went out twice | `9f758deafcc1caac` | `a66cfc68035220af` |

This is a trade-off rather than a bug: without a way to ask Mailgun whether it accepted a batch,
a sender chooses between at most once (an issue may not reach a batch) and at least once (it may
reach it twice), and Ghost chose at least once for errors it saw during the request. For a crash
between Mailgun's answer and Ghost's write it chose the other way: a batch left `submitting` is
never sent again, and the issue is marked failed for the operator (`batch-sending-service.js`,
"not re-sending — marking parent email as failed for operator review"). Worth a sentence in Ghost's
documentation; not drafted as a report.

### Not found

- No check of the sending failed: no issue stuck pending or submitting, no batch recorded as
  sent that Mailgun never accepted, no member twice among an issue's recipients, no recipient
  count off, and no member who had left got a later issue, in the 20 process schedules of the
  second exploration (kills and pauses of Ghost and MySQL, link and one-way faults, clock steps,
  power cuts with disk faults, protocol faults), the 24 schedules of the kill sweep (Ghost and
  MySQL each killed before each of the 12 rounds) and the hand-run kill and power cut
  (`K=ghost@5+2` twice `fab23f6b04027954`, `disk=cut | run C=8` twice `a79a9542875b2793`, both
  OK). The first exploration's kills and power cuts do not count: on that image Ghost could not
  start again at all (below). A limit of this result: an issue's three batches go out within
  about 0.1 s of guest time after its publication, so a kill or cut placed between rounds rarely
  lands inside a send; the windows that Ghost's resume logic is written for (a batch accepted and
  not yet recorded) were not aimed at. On hvx, `--more-faults` (timed kills inside a round) would
  be the tool for that.
- Every publication Ghost acknowledged had its newsletter, also after power cuts (MySQL 8 with its
  durable defaults).

### False findings on the way (both my own mistakes, both found without rebuilding)

The first exploration (20 process schedules) failed 14 times with three signatures, and none was
Ghost's fault:

- **`service cannot recover (ghost)` (7 schedules) and `ERROR ghost restart failed` (4).** Shrunk
  to a power cut alone (`disk=cut dseed=4127cc1347fae6ba | run C=12 W=...`, twice
  `0f2a49e69137e414`). hydo's diagnosis said "a check failed ... No process failed", although
  Ghost had died in the recovery boot. `hydo inspect events <run> --kind exit` showed `ghost
  exited: status 2` at `recovery:77593`, and `hydo inspect logs <run> ghost --at end --backend kvm`
  showed why: `Cannot run Ghost without SSL when Stripe is connected. Please update your url config
  to use "https://"`. My Compose file had `url: http://...`; Ghost lets the operator connect Stripe
  on such a site and keeps running, and refuses to start at the next restart. The fix was the
  url `https://...` and the header a TLS proxy adds (`X-Forwarded-Proto: https`) on the client's
  requests, the readiness check and the fake's webhook. A real operator could hit this too through
  the Admin API (Ghost accepts the keys and the site stops at the next restart); whether Ghost's
  admin screen prevents it was not checked.
- **`no_send_after_unsubscribe` (3 schedules).** Shrunk to a clock step (`T=7:-2700`). The first
  version of the check was an SQL query that ordered an unsubscribe and an issue by Ghost's
  timestamps; a clock step back makes a later unsubscribe look earlier than an issue sent before
  it. The offending rows (`m5@example.com` in two issues sent before the step, gone after it) were
  in the console, inside the agent's frame of the check's output. The check now uses the order the
  clients saw (`/data/test/order.log`), which no clock moves.

### A fingerprint difference on the KVM engine (hydo, open)

`disk=none | run H=mailgun.messages@0-12:status=500,effect=yes` on the final image gave two
fingerprints in five runs from fresh boots (`e922311754d0d30c` three times, `c7393aac8517f6ee`
twice) with the same control transcript hash (`6a17e61e4563afec`), the same consoles and the same
verdict. `hydo diff` of two of them: "The two runs do not differ at any level compared, but their
fingerprints differ ... the difference is in what these levels do not record (the disk's
contents, guest memory, timing inside a stop)". Every other schedule recorded here gave one
fingerprint in two or three runs. This schedule is the one where Ghost's ten-second retry sleeps
overlap the whole workload; open item 45 (f) names one host-timing effect of the engine (a kick
lost for one 5 ms watchdog period) that might be related. Not investigated further; it does not
change any verdict here, but it means a recorded fingerprint of this schedule is not a reliable
regression value on the KVM engine.

## The explorations

All on the final image except the first, with the KVM engine on the build machine:

| exploration | runs | wall | failures | signatures |
|---|---|---|---|---|
| the image with the plain-http url, `--seeds 1-20 --strategy process --protocol-faults --workers 2` | 20, 14 re-runs, 25 shrink, 4 minimal | 806 s, 93 runs an hour | 14 of 20, all reproduced | `service cannot recover (ghost)` 7 and `ERROR ghost restart failed` 4 (the http url, above); `no_send_after_unsubscribe` 3 (the timestamp check, above) |
| final, the same options, `--workers 1` (beside the hand runs) | 20 and the shrinks of 2 | 1228 s, 94 runs an hour | 2 of 20, both reproduced | `donation_recorded_once`: duplicated (seed 20, finding 1), missing (seed 8, a lost webhook) |
| final, `--sweep kill --protocol-faults --workers 2` | 24 (Ghost and MySQL, each before each of the 12 rounds) | 239 s, 363 runs an hour | 0 | |

A run takes 15 to 21 s of engine time (47 to 54 s wall for `hydo run`, which boots fresh); a
worker's boot to its snapshot 33 to 43 s. `--more-faults` (timed kills and power cuts at the k-th
disk flush) is refused on the KVM engine before any VM starts, with the reason; the kill sweep was
run instead.

## Time

Wall time on 2026-11-08 (UTC), from the first look at Ghost:

| phase | at | took |
|---|---|---|
| start: the guide, the skill, the first round's results; Ghost chosen; its newsletter and Stripe code read (v6.75.0) | 06:38 | 12 min |
| this tree's Linux release built on the bare-metal machine (the stale `hydo-world` pin stopped it once; worked around) and installed on both machines | 06:43 to 06:55 | 12 min, in parallel |
| `hydo compose` (no warning), `fetch-images.sh` (17 s), the client, the workload and seven checks, `hydo build --check` OK at the first try | 06:47 to 06:53 | 6 min |
| first image (147 s), copied to the build machine (40 s), first run: OK | 06:58 | |
| **first green run** | 06:59 | **21 min** |
| Stripe donations added (staff session, fake Stripe with objects and a signed webhook) | 07:00 to 07:06 | 6 min |
| **first finding** (one donation recorded six times, the first run with donations), explained with `hydo inspect` in one call | 07:08 | **30 min** |
| the first exploration (14 failures: my http url and my timestamp check), both explained with `hydo inspect` and the console | 07:16 to 07:33 | 17 min |
| the final image; hand schedules twice each; the second exploration | 07:35 to 08:05 | |
| **first finding found blind** (the duplicate donation, seed 20 of the second exploration, which started at 07:43) | 07:57 | **79 min** |

The first round's Medusa: 23 minutes to the first green run, 36 to the first finding by hand,
about 45 to the first finding found blind. Here the first green run took about as long, but none
of it was debugging (most was building this tree's release); the first finding came earlier and
needed no debug image; the first blind finding came later, because the first exploration's 20
schedules were spent on two mistakes of mine (each explained in minutes, but each needed a new
image and a new exploration).

## Compared with Medusa's onboarding

| Medusa's friction (first round) | this round |
|---|---|
| A TLS front for Stripe: a hand-made CA valid at the guest's clock, a `socat` service, `/etc/hosts` lines, `NODE_EXTRA_CA_CERTS`, about 25 lines and an image | Gone: `tls: {hosts: [...]}` on each of two providers, one line each; the three Node.js libraries involved trusted it at the first run |
| 15 tmpfs mounts found by trial for a read-only image root | Not needed (Ghost writes only its content volume, MySQL its data directory), so `writable: true` was not exercised here |
| Four unexplained `ERROR service start` runs, each cause found by rebuilding with a `tee` wrapper (9 minutes, 7 rebuilds) | The first run passed. Later, Ghost dying in the recovery boot was not named by the diagnosis (it said "No process failed"), but `hydo inspect events` and `logs` gave the cause in two calls, about a minute each, with no rebuild |
| Reading the state behind a violation changed the image and lost the reproduction | `hydo inspect logs` explained the duplicate donations from the recorded run. The offending rows of an SQL check were in the console, but only inside the agent's frame of the check's output |
| Hand-written business checks: 136 lines of shell, SQL and awk plus 35 of helpers | 67 lines for eight checks: three `.sql` files (13 lines of SQL), three `hydo-sql check` lines comparing MySQL with a fake's record, one count of batches against accepted messages, and one hand-written order check (18 lines, because the rule cannot be read from Ghost's timestamps) |
| A stateless fake (a currency hard-coded in every answer) | `save:`/`load:`: the webhook endpoint Ghost updates by id at every restart is answered from what was created, also after a power cut |
| No signed webhooks (finding 2 there was partly an artifact of that) | Ghost verified the fake's `Stripe-Signature`; the finding depends on the real webhook path |
| `hydo compose` mapped Redis's `/data` volume onto PostgreSQL's files | No warning, nothing to fix |
| Command output stays in the guest | Unchanged; the client writes one line per operation to `/dev/console`, as in Medusa |
| 23 min to the first green run, 36 to the first finding by hand, 45 blind | 21, 30 and 79 minutes (above) |

## Friction log: what an outside user needed and where they looked

Each item: what was needed, where it was looked for, and what was done. No item was fixed in
hydo in this round.

1. **This tree's Linux release does not build.** `hydo/dist/linux/build-release.sh` on `main`
   stops at "a guest program built here is not its pinned build ... guest/hydo-world: FAILED":
   `hydo-world` changed in Milestones 180 and 181 and its pin did not move (open item 59 says the
   tree's releases still ship 0.2.0's `hydo-world`, which is not what the script does). Worked
   around by accepting the tree's bytes in a local copy of the pin file. The release also does not
   ship `hydo-world-tls` or `hydo-sql`, so a `tls:` fake or a `.sql` check needs both built by hand
   (open items 58 to 60); `hydo build` names the missing file.
2. **`fetch-images.sh` fetches the arm64 images on Linux too** (Medusa's friction item 1, still):
   about 400 MB more, harmless here.
3. **The operator's actions need a staff session.** Ghost's Admin API key (the integration
   token) may not list or retry newsletters (`403 ... API tokens do not have permission`); the
   client sets the site's owner up and logs in, and a staff login from a new device waits for an
   e-mailed code unless `security__staffDeviceVerification` is off. Ghost-specific; found from the
   first run's console.
4. **A webhook URL must be an address.** `url: http://ghost:2368/...` is refused ("url must be
   http://<bridge address 10.89.x.y>:<port>/<path>") although the image's `/etc/hosts` maps the
   service names; the address from the service's `addr:` works. A one-line fix of the manifest;
   the message could name the service's address.
5. **Ghost refuses to boot with Stripe on a plain-http url**, and only at its next start
   ("False findings" above). Found by the first exploration, explained with `hydo inspect`.
6. **The diagnosis does not cover a service that dies in the recovery boot.** For the verdict
   `service cannot recover (ghost)` the diagnosis printed "a check failed ... No process failed"
   and gathered no evidence; the console only said `ghost DIED`. `hydo inspect events --kind exit`
   then showed the exit status, and `hydo inspect logs ... --at end` the reason.
7. **`hydo inspect` needs `--backend kvm` for a KVM run**, although the run directory records its
   engine: the first call answered "the run was recorded by the kvm engine and this hydo runs
   hvx" with the command to use (`try`). `hydo repro` needs it too. `HYDO_BACKEND=kvm` avoids both.
8. **A check that orders events by the application's timestamps is wrong under clock faults**
   ("False findings" above). Nothing in `hydo guide checks` or `workloads` warns about it; a
   sentence there would have saved one exploration.
9. **An SQL check's offending rows are not in the verdict or the report.** `hydo-sql none` prints up
   to five rows, but only its last line ("db: 2 rows where none may be") reaches the assertion's
   details, the verdict and `report.md`; the rows were in the console only inside the agent's
   frame of the check's output (Medusa's friction item 12, for `.sql` checks).
10. **The fake Mailgun cannot see who a batch went to.** `mailgun.js` sends `multipart/form-data`;
   `form:` reads only URL-encoded bodies, so no extractor reaches `to` or `v:email-id`, and the
   record keeps no request field beyond the reference (open item 60 c). The duplicate-send check
   compares counts (messages accepted against batches) instead of members, and the fake answers
   with `<seq>@mg.example.com` (real Mailgun answers a templated id for a per-recipient
   Message-Id, which Ghost does not store) so that batches can be matched with sends.
11. **`--more-faults` is not available on the KVM engine** (its power cut at the k-th disk flush
   needs hvx). The explorer says so before starting a VM, with the alternative.
12. **Free disk on the build machine.** It went from 16 GB to 7 GB free while other sessions
   worked; images were built on the bare-metal machine and only one was kept on the build machine.
13. **KVM.** Ghost (Node.js 22), MySQL 8 and the TLS fakes ran without a change; every schedule
   but one gave one fingerprint twice (the exception: "A fingerprint difference" above).

## What hydo lacked, ranked by how much it blocked

1. **The cause of a service's death in the recovery boot, in the diagnosis.** Eleven of the first
   exploration's 20 schedules failed because Ghost could not start again; the verdict said
   "service cannot recover (committed data unrecoverable)", which reads like data loss, and the
   diagnosis said no process failed. `hydo inspect` found it, but only for a user who knows to
   look past the diagnosis. The diagnosis could treat "cannot recover" and "restart failed" like a
   start failure: the process, its status, its log.
2. **Request fields in the fake's record, and multipart bodies.** The most important newsletter
   rule ("each member gets each issue once") could not be checked per member: the record has no
   recipients, and `form:` does not read `multipart/form-data`, which several provider SDKs send
   (Mailgun's, and file uploads generally). A `record:` list of fields per route and a multipart
   parser in the extractors would make it one `hydo-sql check exactly-once` line (both change
   `hydo-world`'s pinned bytes: open item 60 c).
3. **Fingerprints that stay the same on the KVM engine.** One schedule of eleven gave two
   fingerprints with identical transcripts, consoles and verdicts. Not blocking here, but a
   regression saved with `# expect: fp=` from it would fail at random.
4. **A buildable release from the tree.** The release script stops on `main`, and the release does
   not ship `hydo-world-tls` or `hydo-sql`, which `tls:` and `.sql` checks need. An outside user
   following the guide with today's release cannot use either.
5. **The full output of a failing check in the run's results.** The rows that explain an SQL
   check's failure are printed and then lost except inside a raw console frame.
6. **A warning about timestamps in checks under clock faults**, in the checks guide.
7. **The engine taken from the run** for `hydo inspect` and `hydo repro`.
