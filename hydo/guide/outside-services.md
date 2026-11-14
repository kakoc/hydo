# Outside services, protocol faults, and generated gRPC drivers

The full reference is in `hydo guide manifest`, in the sections `Fake outside services and
protocol faults`, `gRPC faults`, and `gRPC services: generated drivers`. Everything on this page is x86_64
only and needs a build from hydo's repository (`hydo guide capabilities`).

## Ready-made emulators of outside services

For a provider in this table, write one `emulators:` entry instead of a fake. hydo runs the
emulator as a service and puts a fault proxy of the same name in front of it. `H=` faults, the
record and the checks below then work as for a fake.

| `use:` | The emulator | The system calls | Its H= routes |
|---|---|---|---|
| `stripe` | stripe-mock 0.206.0 (MIT) | `https://api.stripe.com` (the SDK unchanged), or `http://127.0.0.1:12111` | `payment_intent`, `payment_intent_confirm`, `payment_intent_capture`, `payment_intent_cancel`, `charge`, `refund`, `customer`, `checkout_session`, `subscription`, `invoice`, `transfer`, `payout`, `other_post`, `delete`, `read` |
| `smtp` | Mailpit 1.33.1 (MIT) | SMTP at `127.0.0.1:1025`; Mailpit's HTTP API at `127.0.0.1:8025` | `conn` (one connection = one message) |
| `s3` | rclone 1.76.0, `rclone serve s3` (MIT) | `$AWS_ENDPOINT_URL_S3` (`http://127.0.0.1:9000`), path-style | `create_bucket`, `put_object`, `post`, `delete`, `head`, `read` |

```yaml
emulators:
  - use: stripe
    ref: form:metadata[order]    # the business reference in the record (an extractor, as for a fake)
  - use: s3
    buckets: [receipts]          # buckets that exist from the start
  - use: smtp
    name: email                  # the proxy's name, which H= and the checks use (default: the use: value)
```

Each entry also takes `port:` (the port the system calls) and, for `stripe`, `tls: false` (only
the plain port). An entry makes a service `<name>_emulator`. The explorer draws no fault on it;
the outside world's faults are `H=` tokens on the proxy. `hydo build`, also with `--check`,
fetches each emulator's image by its pinned digest into `emulators/` next to the manifest: keep
that folder out of git.
The `s3` entry sets `AWS_ENDPOINT_URL_S3`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and
`AWS_REGION` in `env:` unless the manifest sets them. An image service of the system sets them in
its own `env:`. The S3 server accepts requests with any signature or none.

What the emulators do and do not do:

- stripe-mock checks each request against Stripe's API description and answers with Stripe's
  example objects. It keeps no state: a read returns an example, not what the system created. The
  proxy keeps Stripe's idempotency: a repeat of an `Idempotency-Key` gets the first answer and is
  recorded as a replay (`fx=0`), so `hydo-world check at-most-once stripe.payment_intent` counts
  real double charges only. stripe-mock sends no webhooks; declare a fake for them.
- Mailpit keeps the mail it received. A check reads it from its API, for example
  `GET http://127.0.0.1:8025/api/v1/search?query=subject:"Receipt o7"` (`messages_count`).
- The S3 server keeps objects as files under `/data/<name>`.
- An emulator's state is inside the guest: a power cut loses what it held in memory. The record of
  the proxy survives a cut; a check over an emulator's own state is valid in runs without a cut.

The example `hydo/onboard/examples/checkout` in hydo's repository uses all three.

## Fakes: outside services the system calls

A payment provider, an email API or a webhook sender cannot be reached from the guest. Declare it
as a fake: hydo serves it inside the guest (the static program `/bin/hydo-world`) and records
every request in `/tmp/hydo/world.log`.

```yaml
fakes:
  - name: payments                       # not a service name; H= tokens and checks use it
    port: 9001                           # the system must be configured to call 127.0.0.1:9001
    idempotency: header:Idempotency-Key  # where a request's idempotency key is
    ref: json:order                      # the business reference a request is about
    routes:
      - name: charge
        method: POST
        path: /v1/charges
        status: 200
        headers: {Content-Type: application/json}
        body: '{"id":"ch_{seq}","order":"{ref}","amount":{json:amount}}'
        idempotent: true                 # a repeat with the same key gets the first answer, no new effect
        webhook:                         # after the effect, call the system back
          url: http://127.0.0.1:8080/webhooks/payments
          body: '{"type":"charge.succeeded","order":"{ref}"}'
          delay_ms: 20
```

Extractors for `idempotency:` and `ref:`: `header:<Name>`, `json:<dotted.path>`,
`query:<name>`, `form:<name>`, `path:<name>`, `pb:<field number>` (gRPC), `body`. Answer
templates: `{seq}`, `{uuid}`, `{ref}`, `{idem}`, `{json:a.b}`, `{header:X}`, `{query:x}`, `{path:x}`.
`protocol: grpc` makes a cleartext HTTP/2 gRPC fake (`path` is `/pkg.Service/Method`, the answer
is `message:` in hex protobuf and `code:`).

The system can read back what it created, such as a payment's currency or its status after a
capture. Then give the creating route
`save: {key: 'pi_{seq}', fields: {currency: '{form:currency}'}}`, and the reading route
`load: '{path:id}'`. Its answer then uses `{obj:currency}`; an unknown id is answered 404. When the system checks the provider's webhook signature, give the webhook
`sign: {header: <its header>, secret: <the endpoint secret the system is configured with>}`
(HMAC-SHA256 of `{ts}.{body}`, value `t={ts},v1={hmac}`; `payload:`, `value:` and
`encoding: base64` change the scheme). `hydo guide manifest` has the details.

Find the outside services in the project's configuration (base URLs, API keys, webhook
endpoints) and point each base URL at its fake. Ask the user which answers each route gives, or
take them from the provider's API documentation.

## Fakes from an OpenAPI description

A provider, or a team's own internal service, often publishes an OpenAPI description (a YAML or
JSON file of its operations). When no ready-made emulator covers it, write its fake from that file
instead of route by route:

```sh
hydo fake from-openapi api/payouts.yaml --name payouts --port 9001 --stateful --ref json:order \
  --add-to hydo/hydo.yaml
```

- Each operation becomes a route. Its answer is the spec's example, or a body made from the
  answer's schema with no random value.
- Where the schema allows, top-level fields are templates. A new resource's `id` is `{seq}`,
  `{uuid}` or `po_{seq}`, and the `id` of `/payouts/{id}` is `{path:id}`. A field that the request
  sends is `{json:x}` or `{form:x}`.
- `--stateful` adds `save:` to a create (`POST /payouts` that answers an `id`) and `load:` to the
  routes about one resource (`/payouts/{id}`). A read then answers what the create stored, and an
  unknown id gets 404.
- A header parameter whose name holds `idempotency` becomes the fake's `idempotency:`. The routes
  that declare it get `idempotent: true`.
- The spec's error answers go into each route's `errors:`. An `H=` rule `status=<code>` then
  answers the provider's own error body, for example
  `H=payouts.create_payout@3-4:status=409`. `hydo explore --protocol-faults` also draws these
  statuses.

Without `-o FILE` or `--add-to` it prints the entry. `--ops createPayout,getPayout` keeps only
those operations, `--tls` adds `tls:` for the first server's host, and `--ref` names the business
reference that the checks group by. Each operation it cannot map is one line on stderr:

```text
hydo fake: not mapped: uploadReceipt (POST /receipts): the request body is binary (application/octet-stream)
```

It refuses Swagger 2.0, `callbacks:`, a `oneOf` without a discriminator, binary bodies, answers in
XML only, a `$ref` to another file, and a parameter inside a path segment. The written entry is an
ordinary fake: read it, then edit what the system needs, such as a status it checks.
`hydo guide manifest`, section `Fakes from an OpenAPI description`, lists every rule. The example
`hydo/onboard/examples/openapi-payouts` in hydo's repository uses it.

## HTTPS: a provider whose SDK cannot be pointed at plain HTTP

Most providers are HTTPS only, and their SDKs call a fixed host name and check the certificate
(Stripe's SDKs call `https://api.stripe.com`). Do not build a CA, a TLS front, or trust settings
by hand: give the fake `tls:` with the names the SDK calls, and leave the SDK's base URL alone.

```yaml
fakes:
  - name: stripe
    port: 9101
    tls: {hosts: [api.stripe.com]}   # HTTPS on <the fake's address>:443 for these names
```

The build does four things:

- It makes a test CA and a certificate for those names. Both are reproducible and valid for any
  guest clock.
- It maps the names to the fake in `/etc/hosts`, the guest's and every image service's.
- It adds the CA to the system bundles of the guest and of every image.
- It sets `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`, `NODE_EXTRA_CA_CERTS`, and `NIX_SSL_CERT_FILE`.
  Node.js, Python's requests and httpx, Go, OpenSSL, curl, and Ruby then trust the CA.

The record and `H=` rules are the same as over plain HTTP. These are not covered:

- the own `cacerts` of a Java program (import the CA with `keytool` in `setup:`);
- a program with a built-in CA list that no variable replaces;
- certificate pinning.

The image needs `hydo-world-tls` (`hydo guide manifest`, section `HTTPS for fakes and
proxies`). A proxy takes `tls:` too.

## Proxies: protocol faults in front of a real service

```yaml
proxies:
  - name: redisp
    port: 6380           # the system talks to this port instead of the service's
    service: redis       # forward to that service's port (or to: 127.0.0.1:6379)
    protocol: tcp        # tcp | http | grpc
```

## Protocol faults: the `H=` token

`H=<target>@<from>-<to>:<rule>[,<rule>...]` on a fake or proxy (`<target>` is `payments` or
`payments.charge`) while rounds `[from, to)` run. Request rules: `status=<code>` (an error
answer; `retry-after=<s>` adds the header), `delay=<ms>`, `drop` (no answer), `reset`. Webhook
rules: `dup`, `reorder`, `late=<ms>`, `lost`. Modifiers: `p=<1..100>` (a share of the matching
requests), `effect=yes|no` (did the world still act: `status=500,effect=yes` = "charged, then
failed to say so"). On a gRPC proxy also `status=<CODE NAME>`, `reset=conn`, `cut=<n>`,
`hold=<n>`, `dir=req`, and call selection by `rpc=<Method>` or `call=<a>-<b>`.

List `protocol` in `faults:`. `hydo explore --protocol-faults` (and `--grpc-faults` for gRPC
proxies) draws these tokens; `--sweep protocol` tries every rule kind on every target, one round
at a time. The record's checks are in `hydo guide checks` ("Side effects at fake outside services");
`hydo-world check calls <proxy>` reports what gRPC clients saw versus what the server did.

## Generated gRPC drivers (`hydo-grpc-gen`)

For a system whose inputs are gRPC endpoints, do not write one client per RPC:

1. Find the `.proto` files (or a compiled descriptor set, or a server with reflection enabled).
   If the service has no `.proto` (a hand-written service descriptor), write one that matches
   it exactly: package, service, and method names, and request and response types.
2. Write a hints file from the API's documentation and validation code. It has these keys:
   - `target`, and a `deadline_ms` like the real clients';
   - per RPC a `weight`;
   - `range`, `choice`, or `len` for fields the server validates;
   - `unique: true` for ids and idempotency keys that the client chooses;
   - `from: <Rpc>.<field>` for ids that an earlier response returns;
   - `retry: {attempts: N}` for RPCs that real clients retry;
   - `outcome_id:` for the field that identifies an operation in `$HYDO_ACKED`.
3. Run, from hydo's repository (it needs Go):

   ```sh
   hydo/onboard/hydo-grpc-gen <proto dir> --hints hints.yaml --out <dir>
   ```

   It writes `<dir>/hydo-grpc` (one static client), `<dir>/grpc/` (the schema and plan),
   `<dir>/workload/parallel_driver_<rpc>` (one composer command per RPC) and
   `<dir>/hydo.grpc.yaml` (the `root.files`, `env:` and `test:` lines to merge into
   `hydo/hydo.yaml`). Copy the commands into `hydo/workload/`.
4. Check the plan: `hydo-grpc gen <rpc> 5` prints generated requests (do they look valid?),
   `hydo-grpc list` lists the RPCs.
5. Checks stay hand-written. `hydo-grpc call <rpc> '<json>'` is a read-back for them. It exits 0
   on success, 1 on a definite failure, and 2 on an unknown outcome.

Outcomes come from the gRPC status:

- `OK` is a success.
- `INVALID_ARGUMENT`, `NOT_FOUND`, `ALREADY_EXISTS`, `FAILED_PRECONDITION`, and similar codes
  are definite failures.
- `DEADLINE_EXCEEDED`, `UNAVAILABLE`, `INTERNAL`, and similar codes are unknown.

`classify:` in the hints overrides a code per RPC. Limits: cleartext
HTTP/2 only (no auth metadata; a fake with `tls:` serves gRPC over TLS, the generated client
does not speak TLS). Example in hydo's repository: `hydo/grpcgen/demo/`.
