# Fakes from OpenAPI descriptions

This page explains the design of `hydo fake from-openapi` (Milestone 249). The command writes a
fake outside service, a `fakes:` entry of hydo.yaml, from an OpenAPI 3.0 or 3.1 description. The
reference for users is `hydo guide outside-services`, and the reference for every rule is the
section `Fakes from an OpenAPI description` of `hydo guide manifest`.

## The problem

A fake answers a system's calls to an outside service inside hydo's virtual machine. Until now a
project wrote each route by hand: its method and path, its answer in the provider's format, the
objects it keeps, its idempotency. Milestone 242 added ready-made emulators for three providers.
Most outside services have no emulator: smaller providers, a partner's API, a team's own internal
services. Many of them publish an OpenAPI description, which already names every operation, its
answers with examples and schemas, its error answers, and its headers.

## Generate once, then own the entry

The command writes the fake once, and the manifest keeps the written entry. The alternative was a
fake that names a spec file, which hydo would expand at every build. The written entry was chosen
for three reasons:

- An image's bytes depend only on the manifest. A change of the generator does not move a recorded
  fingerprint.
- The entry is readable and a project edits it: a status its system checks, a field the spec
  leaves out.
- Every later part of hydo (hydo-world, the checks, `H=`, the explorer) sees an ordinary fake.

The first comment lines of the entry name the command, the spec's title and version, and the first
16 hex digits of its sha256. A project can then generate it again and compare.

## The answers have no random value

A run must be the same in every replay, so a generated answer holds no random value. A body comes
from the spec's example when there is one, and else from the schema, with fixed values: the zero
UUID, the date `2026-01-01`, the minimum of a number. Values that must differ between requests are
hydo-world's templates:

- `{seq}` is the record number of the request, so a create answers a new `id` each time.
- `{uuid}` is a new template: a UUID made from `{seq}`. It was added for APIs whose ids are UUIDs,
  because `{seq}` alone is not a valid UUID.
- `{json:x}`, `{form:x}`, `{path:x}` and `{query:x}` repeat what the request sent, so a client
  that compares the answer with its request accepts it.

Only top-level fields become templates. A field that the request sends is echoed only when the
request schema requires it. An optional field that a request leaves out would render as an empty
value, which breaks a number in JSON.

## Create-then-read pairs

With `--stateful` the command finds a create (`POST /things` that answers an `id`) and the routes
about one of its resources (`/things/{id}` and `/things/{id}/...`). The create gets `save:` with
the echoed fields, and the other routes get `load:`. A read then answers what the create stored,
and an unknown id is answered 404, as Milestone 181's objects do for hand-written fakes. A `PUT`
saves its echoed fields again. A `PATCH` only loads: a field that a `PATCH` leaves out would
otherwise be saved as empty. A `DELETE` loads and does not remove the object, because hydo-world
has no removal.

A route below a resource whose answer has the same schema as the resource's `GET`, such as
`POST /transfers/{id}/cancel`, answers that resource: its `id` is `{path:id}`. A route below a
resource with another schema, such as `POST /transfers/{id}/returns`, creates a new one.

## Error answers by status

A provider's documented error answers matter to a system that reads them: a `409` with the type
`idempotency_error` is handled differently from a `500`. Routes therefore take `errors:`, a
format-1 addition. An `H=` rule `status=<code>` answers the body of the exact status, else of its
class, else of `default`. A route without `errors:` answers as before, so no recorded run
changes. The explorer's protocol faults also draw the exact statuses that a route lists, besides
hydo's four (500, 502, 503 and 429).

## What it refuses

Some parts of an OpenAPI description have no fake equivalent, or no single right answer. The
command refuses each such operation with one line, and maps the others:

| The spec has | Why it is refused |
|---|---|
| Swagger 2.0 | another format; a converter makes OpenAPI 3 from it |
| `callbacks:` | the provider calls the system back; a fake's `webhook:` does that, written by hand |
| a `oneOf` without a discriminator, where the answer needs it | no alternative is more right than another |
| a binary body (`application/octet-stream`, `format: binary`, images, `multipart/form-data`) | hydo-world's answers are text templates, and it cannot read an upload's fields |
| an answer in XML only | the templates and the checks read JSON and form bodies |
| a `$ref` to another file | the command reads one file |
| a parameter inside a path segment (`/files/{name}.json`) | hydo-world matches whole segments |

An optional field that cannot be made, such as a `oneOf` inside an optional property, is left out
of the answer, and the operation is mapped.

## Left for later

- Response headers, such as `Location` after a `201`, are not generated.
- Nested fields are not echoed. For example, `amount.value` of Adyen's transfers stays a fixed
  value.
- The business reference (`ref:`) is not inferred; `--ref` names it.
- Webhooks in OpenAPI 3.1's `webhooks:` are not mapped to a fake's `webhook:`.
