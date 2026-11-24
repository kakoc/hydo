# Emulators of outside services

This page explains the design of `emulators:`, a catalog of ready-made emulators of outside
services (Milestone 242). The reference is the section on ready-made emulators in `hydo guide
outside-services`.

## The problem

A system calls outside services: a payment provider, an email service, object storage. Inside
hydo's virtual machine those services cannot be reached. Until now a project declared each one as
a fake (`fakes:`): every route, its answer template, its saved objects, its webhooks. A fake of
Stripe needs about ten routes with answers in Stripe's format before the system's own code
accepts them.

Projects exist that answer like a provider: an emulator. An emulator answers like the provider,
but it never fails like one. hydo's fault proxy makes the failures: an error status, a slow
answer, a lost answer after the effect (`H=` tokens). The design therefore runs an emulator as a
service inside the virtual machine and puts hydo's proxy in front of it.

## The candidates

The table lists the emulators that were checked in November 2026. "Fit" says whether the
emulator suits a first version: a pinned image, no account or token, small, and no network
access at run time.

| Emulator | Provides | License | Runtime | Image size | State | Fit |
|---|---|---|---|---|---|---|
| stripe-mock 0.206.0 | Stripe's HTTP API, from Stripe's OpenAPI description | MIT | one static Go program | 9.6 MB | none: every answer is a fixture | chosen for `stripe` |
| Mailpit 1.33.1 | an SMTP server and an HTTP API over the received mail | MIT | one static Go program | 14 MB | in memory and a temporary SQLite file | chosen for `smtp` |
| rclone 1.76.0, `rclone serve s3` | an S3 server over a directory | MIT | one static Go program | 32 MB | files under the directory | chosen for `s3` |
| Versity S3 Gateway | an S3 server over a directory | Apache-2.0 | one Go program | not measured | files and extended attributes | not chosen: it checks request signatures, and hydo's proxy rewrites the `Host` header that the signature covers |
| vercel-labs/emulate | Stripe, Resend, S3, GitHub, Slack, Clerk, Twilio and others, with state | Apache-2.0 | TypeScript on Node.js 24, from npm | no published image | in memory, seeded from YAML | later: it needs an image built from npm with Node.js, and its Stripe has state and webhooks that stripe-mock lacks |
| LocalStack | AWS | changed in March 2026 | Python | about 1 GB | in memory | not chosen: its image needs an account's token since March 2026 |
| MinIO | S3 | AGPL-3.0 | one Go program | about 100 MB | files | not chosen: no published images since October 2025, the repository is archived |

hydo ships none of these programs. `hydo build` fetches each project's own published image from
Docker Hub by the digest that hydo pins, so no license list of a hydo release changes.

## The manifest form

A project writes one entry per outside service, a format-1 addition:

```yaml
emulators:
  - use: stripe                  # https://api.stripe.com
    ref: form:metadata[order]    # the business reference of a request, for the record
  - use: s3
    buckets: [receipts]
  - use: smtp
    name: email
```

The keys are `use` (the catalog entry), `name` (default: the `use` value), `port`, `ref`, `tls`
(Stripe only), `buckets` and `durable` (S3 only), and `description`. A new top-level list keeps the format of
`fakes:` as it is: a fake needs `port` and `routes`, and an emulator has neither.

## What an entry becomes

The validator expands each entry before it checks the manifest, so every later part sees an
ordinary manifest:

- A service `<name>_emulator` with `image:` (the file
  `emulators/<emulator>-<version>-<arch>.docker.tar` next to the manifest), `entrypoint:` (the
  emulator's command line on its own port), `ready: tcp:<port>` and `port:`. The emulator
  services come first in `services:`, so that they run before the system starts.
- A proxy `<name>` with `service: <name>_emulator` on the port that the system calls. The
  ports are 12111 for Stripe, 1025 for SMTP, and 9000 for S3. The Stripe proxy has the provider's routes (`payment_intent`,
  `charge`, `refund` and others), its idempotency header, and
  `tls: {hosts: [api.stripe.com]}`. The S3 proxy has a route for each type of request. The SMTP
  proxy is a TCP proxy: one connection is one request.
- For S3, the data directory `/data/<name>` with the buckets, and the variables
  `AWS_ENDPOINT_URL_S3`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and `AWS_REGION` in `env:`,
  where the manifest does not set them. The AWS SDKs read `AWS_ENDPOINT_URL_S3`.

The expansion removes `emulators:` and leaves `_emulators` in the validated data. It holds
the expanded names and the image pins. Two readers use it. The explorer draws no fault on an emulator's
service: the service stands for the outside world, whose faults are `H=` tokens on its proxy.
`hydo build` fetches each missing image with `hydo-fetch-image`'s code before it builds.

## How the system reaches the emulator

A Stripe SDK calls `https://api.stripe.com` and checks the certificate. The proxy's `tls:` gives
that name a certificate of the image's own test CA, maps the name to the proxy in `/etc/hosts`,
and makes the guest trust the CA (`hydo guide outside-services`, section "HTTPS"). The SDK needs
no change. An S3 client reads the endpoint from `AWS_ENDPOINT_URL_S3`. An SMTP client is
configured with `127.0.0.1:1025`, as for any mail relay.

## Idempotency at the proxy

stripe-mock keeps no state, so it answers a retry with the same `Idempotency-Key` as a new
payment. Stripe answers it with the first payment. Without a change, the record would show a
double charge for a system that retries correctly. Proxy routes therefore take
`idempotent: true`, a format-1 addition. A repeat of a key that took effect gets the service's
first answer and does not reach the service. The record shows it as a replay (`fx=0 out=replay
orig=<seq>`), as on a fake. The Stripe proxy's create routes have it. The stored answers are not
carried across a power cut: a repeat after a cut reaches the service again and is still recorded
as a replay.

## State and seeds

stripe-mock has no state: its answers are fixtures with the request's own values, and its IDs are
random. hydo's guest makes the random numbers the same in every run, so the IDs are the same in
every replay. Mailpit starts empty. The S3 server starts with the buckets of `buckets:`; a system
that needs objects at the start writes them in `provision:` or a `first_` command.

An emulator's state lives in the guest. A power cut (`cf=`) loses what the emulator held in memory
and what it did not flush, which the real provider would keep. Checks over the record of hydo's
proxy survive a cut, because hydo-world carries the record into the recovery boot. Checks over an
emulator's own state, such as Mailpit's mailbox, are valid in runs without a cut. The S3 server's
objects survive a cut with `durable: true` (the next section).

## Objects that survive a power cut

Real object storage keeps every object that it acknowledged. rclone 1.76.0 does not: its local
backend (`backend/local/local.go`) writes an object's file and closes it without an `fsync`, and
no option of the backend makes one. `rclone serve s3` writes an object under a temporary name and
renames it into place, both through rclone's file layer, before it answers. The file's data and
the rename then wait in the guest's page cache and journal for the next writeback. A power cut of
the guest loses them. So the GreptimeDB example of Milestone 343 could not run `cut`.

Four ways were compared:

| Way | What it changes | Chosen |
|---|---|---|
| An option of rclone | nothing: rclone 1.76.0 has none | no |
| A mount option such as `sync` | the whole data disk of every service: `sync` is a flag of the file system, not of a bind mount | no |
| A sync in hydo's proxy before it forwards an answer | `hydo-world`, a pinned guest program, so every image's bytes at its next re-pin | no |
| ext4's attribute `S` on the emulator's folders | one command in the emulator's `setup:`, for an entry that asks for it | yes |

With `durable: true`, the `setup:` of an `s3` entry ends with `&& chattr +S /data/<name>
/data/<name>/<bucket> ...`, with busybox's `chattr`. On a file with the attribute, every
`write(2)` returns after the kernel wrote the data and the inode and flushed the disk's write
cache (`generic_write_sync`). In a folder with it, a create, a rename, an unlink, and a mkdir commit
the journal before they return (`IS_DIRSYNC`). New files and folders inherit the attribute. So the
temporary file, its rename and a DELETE's unlink are on the disk before rclone answers, and the
proxy forwards the answer after that. A copy is a read and a PUT, and a multipart upload that
completes is a rename, so both are covered too.

The key is opt-in, a format-1 addition. Without it the setup is the one of Milestone 242, so an
image built without the key keeps its bytes and its fingerprints. Every write costs a flush of the
disk's cache, and a multipart upload's parts are written as they come.

What `durable: true` does not cover:

- A multipart upload that is not completed. rclone keeps the upload's record in memory
  (`cmd/serve/s3/multipart.go`), so the emulator of the recovery boot does not know the upload.
  Real object storage keeps it. No run tested this case.
- The metadata that rclone keeps in memory (`storeObjectMeta` in `cmd/serve/s3/backend.go`),
  such as an object's user metadata (`x-amz-meta-*`). A restart of the emulator loses it, with or
  without a cut.

The proof is `hydo/onboard/examples/checkout/hydo-s3-cut.yaml` (Milestone 354, `docs/HISTORY.md`).

## Conditional writes

Clients of S3 make optimistic concurrency with two headers. `If-None-Match: *` creates an object
only if the key is free. `If-Match: <etag>` replaces an object only if it is still the version
read. Rust's `object_store` sends them for `PutMode::Create` and `PutMode::Update`. In November
2026 the pinned rclone 1.76.0 was tested with them under Docker, directly and through hydo's
proxy (Milestone 329):

| Request | rclone's answer |
|---|---|
| `If-None-Match: *`, a new key | 200 |
| `If-None-Match: *`, an existing key | 412 `PreconditionFailed` |
| `If-Match` with the object's ETag | 200 |
| `If-Match` with another ETag | 412 `PreconditionFailed` |
| `If-Match` on a missing key | 404 |

`object_store` 0.14.2 turned the 412 answers into `AlreadyExists` and `Precondition`, through the
proxy as directly. In 30 rounds of 8 parallel writers with each header on one key, one writer won
each round. So the emulator needs no re-pin for conditional writes.

A system's own path for a lost race runs only when another writer wins. The s3 proxy therefore
has two routes before `put_object`: `put_if_absent` takes a `PUT` with `If-None-Match`, and
`put_if_match` a `PUT` with `If-Match` (the route key `if_header:`). Each answers an `H=` rule
`status=412` with S3's XML (`errors:` on an `http` proxy's route), without reaching rclone.

## Left for later

- More providers. vercel-labs/emulate covers many with state and webhooks; it needs an image
  with Node.js 24 built and pinned by hydo.
- Webhooks: stripe-mock sends none. A fake with `webhook:` still covers a provider's webhooks.
- A record of SMTP messages by reference: the SMTP proxy records connections, not messages.
- An emulator image's own version choice (`version:`): each hydo pins one version.
