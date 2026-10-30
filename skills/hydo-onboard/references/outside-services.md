# Outside services, protocol faults and generated gRPC drivers

Full reference: the sections "Fake outside services and protocol faults", "gRPC faults" and
"gRPC services: generated drivers" of `hydo guide manifest`. Everything on this page is x86_64
only and needs a build from hydo's repository (capabilities.md).

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
templates: `{seq}`, `{ref}`, `{idem}`, `{json:a.b}`, `{header:X}`, `{query:x}`, `{path:x}`.
`protocol: grpc` makes a cleartext HTTP/2 gRPC fake (`path` is `/pkg.Service/Method`, the answer
is `message:` in hex protobuf and `code:`).

Find the outside services in the project's configuration (base URLs, API keys, webhook
endpoints) and point each base URL at its fake. Ask the user which answers each route gives, or
take them from the provider's API documentation.

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
at a time. The record's checks are in checks.md ("Side effects at fake outside services");
`hydo-world check calls <proxy>` reports what gRPC clients saw versus what the server did.

## Generated gRPC drivers (`hydo-grpc-gen`)

For a system whose inputs are gRPC endpoints, do not write one client per RPC:

1. Find the `.proto` files (or a compiled descriptor set, or a server with reflection enabled).
   If the service has no `.proto` (a hand-written service descriptor), write one that matches
   it exactly: package, service and method names, request and response types.
2. Write a hints file from the API's documentation and validation code: `target`, a
   `deadline_ms` like the real clients', and per RPC a `weight`; `range`/`choice`/`len` for
   fields the server validates; `unique: true` for ids and idempotency keys the client chooses;
   `from: <Rpc>.<field>` for ids an earlier response returns; `retry: {attempts: N}` for RPCs
   real clients retry; `outcome_id:` for the field that identifies an operation in
   `$HYDO_ACKED`.
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
5. Checks stay hand-written. `hydo-grpc call <rpc> '<json>'` (exit 0 ok, 1 definite failure, 2
   unknown) is a read-back for them.

Outcomes come from the gRPC status: OK is ok; INVALID_ARGUMENT, NOT_FOUND, ALREADY_EXISTS,
FAILED_PRECONDITION and similar are definite failures; DEADLINE_EXCEEDED, UNAVAILABLE, INTERNAL
and similar are unknown (`classify:` in the hints overrides a code per RPC). Limits: cleartext
HTTP/2 only (no TLS or auth metadata). Example in hydo's repository: `hydo/grpcgen/demo/`.
