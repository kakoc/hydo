# hydo-assert: the JavaScript assertion SDK (Node.js)

A Node.js program running under hydo (the deterministic hypervisor in this repository) uses this
package to say what must be true, what should happen at least once, what must happen within a
deadline, and where a dependency may misbehave. It writes the same console records as the Go
SDK (`hvx/stack/hydoassert`), byte for byte, with the same deduplication rules, so
`hydo_assert_report`, the harnesses and `explore.py` read them unchanged. The wire format and
the verdict rules are in `hvx/ASSERTIONS.md`.

The package has no dependencies, works from CommonJS and ESM, and ships TypeScript types. It
is not published (`"private": true`); depend on it by path:

```json
{ "dependencies": { "hydo-assert": "file:../hydo/sdk/js" } }
```

In a guest image, copy `package.json`, `index.js` and `index.mjs` to
`/<app>/node_modules/hydo-assert/` (see `hvx/onboard/examples/node-kv/hydo-mac.yaml`).

## Use

```js
const ha = require('hydo-assert');          // or: import * as ha from 'hydo-assert'

// Declare at module top level (load time), so the catalog knows every property up front.
const ACKED = ha.always('kv.acked_durable', 'every acknowledged write reads back with its value');
const RECOVERED = ha.sometimes('kv.recovered', 'the server replayed a non-empty log');
const TORN = ha.reachable('kv.torn_tail', 'the log ended in a partial line');
const OVERWRITE = ha.unreachable('kv.overwrite', 'a key is never overwritten with another value');
const LOG_SIZE = ha.always('kv.log_size', 'the log stays under its cap');
const BACK = ha.eventually('kv.back', 'the server answers within 5 s', 5000);
const LOST_REPLY = ha.buggify('kv.put.lost_reply', 'the write lands but the reply is lost');

ha.catalog();                                // once, early, in one process: HYDO-ASSERT-DECL lines
ACKED.check(got === want, { key: k, got });
RECOVERED.check(n > 0, { keys: n });
TORN.hit({ bytes });
OVERWRITE.hit({ key });
LOG_SIZE.lessThanOrEqualTo(size, cap);       // or LOG_SIZE.cmp(size, '<=', cap, details)
if (LOST_REPLY.fire({ key })) { socket.destroy(); return; }
await BACK.awaitCond(async () => (await ping()) === 'pong', 100);   // one record per episode
ha.setupComplete();                          // HYDO-SETUP-COMPLETE
```

| Go SDK (`hydoassert`) | JavaScript (`hydo-assert`) |
|---|---|
| `Always(id, msg)`, `AlwaysOrUnreachable`, `Sometimes`, `Reachable`, `Unreachable` | `always(id, msg)`, `alwaysOrUnreachable`, `sometimes`, `reachable`, `unreachable` |
| `a.Check(cond, d)`, `a.Hit(d)` | `a.check(cond, d)` (returns the condition as a boolean), `a.hit(d)` |
| `a.Cmp(left, op, right, d)` | `a.cmp(left, op, right, d)` (numbers or bigints), and the named helpers `a.greaterThan`, `a.greaterThanOrEqualTo`, `a.lessThan`, `a.lessThanOrEqualTo` (`(left, right, d)`; the same records) |
| `Eventually(id, msg, within)` | `eventually(id, msg, withinMs)` |
| `l.Await(cond, poll, d)` | `await l.awaitCond(cond, pollMs, d)` (cond may return a promise); `l.awaitCondSync(cond, pollMs, d)` blocks the thread |
| `l.Begin()`, `w.Done(d)`, `w.Expire(d)` | `l.begin()`, `w.done(d)`, `w.expire(d)` |
| `Buggify(id, msg)`, `p.Fire(d)` | `buggify(id, msg)`, `p.fire(d)` |
| `Catalog()`, `SetupComplete()` | `catalog()`, `setupComplete()` |
| `D{...}` | a plain object (or a `Map`) |
| (none) | `emitLine(tag, value)`: any other one-line record kind through the same sink; `toJSON(value)`: the Go encoding of a value |

## Records, deduplication, catalog

- Records go to `/dev/ttyS0`, opened once (`O_WRONLY|O_APPEND|O_NOCTTY`), one synchronous
  `write` per record (`fs.writeSync`), so a record is never interleaved with other console
  output and is on the console before the call returns. `HYDO_ASSERT_OUT=<path>` writes to that
  existing file, `stderr` to standard error (plain logging outside hydo), `off` nowhere (a
  no-op; BUGGIFY still works). If the file cannot be opened, standard error is used.
- A process writes each (assertion, outcome) pair once: the first pass and the first failure,
  with the details of that evaluation. A `sometimes` that evaluated false is never written.
  `unreachable` is written with `ok:false`. As in Go, a record whose details cannot be encoded
  (a NaN, an infinity, a cycle) is not written at all, and that outcome still counts as
  written.
- `catalog()` writes one `HYDO-ASSERT-DECL` line per declared assertion, in declaration order
  (each call writes all of them again, as Go's `Catalog()` does). Declare at module top level,
  so a property in code that never runs is still in the catalog and is reported as never
  evaluated. Under hydo the explorer's runs start from a snapshot taken after the services are
  up, so a long-running server's start-up catalog is not in each run's console: let a
  short-lived process that runs in every run (the oracle, a client) call `catalog()` too, and
  declare the properties in one module that every program loads (as `node-kv/props.cjs` does).
- "Per process": a worker thread (`worker_threads`) loads its own copy of the module, with its
  own deduplication table. The ESM entry re-exports the CommonJS module, so a program that uses
  both `require` and `import` has one state.

### How details are encoded (exactly as Go's `encoding/json`)

| JavaScript value | written as | Go value it matches |
|---|---|---|
| object / `Map` | object, keys sorted by their UTF-8 bytes (not by JavaScript's UTF-16 order) | `map[string]any` |
| array, `Set`, typed arrays other than `Uint8Array` | array | slice |
| `Uint8Array`, `Buffer` | base64 string | `[]byte` |
| number | shortest digits; exponent form below 1e-6 and from 1e21 (`1e+21`, `1.5e-7`); `-0` as `-0` | `float64` (and Go integers, which print the same) |
| bigint | decimal integer | `int64`, `uint64` |
| string | `"`, `\` escaped; `\b \f \n \r \t`; other control characters `\u00xx`; `<` `>` `&` as `<` `>` `&`; U+2028/U+2029 escaped; a lone surrogate as `�` | `string` (invalid UTF-8 as `�`) |
| `true`, `false`, `null` | the same | |
| a value with `toJSON()` (e.g. `Date`) | its result | |
| `undefined`, function, symbol | left out of objects, `null` in arrays (as `JSON.stringify`) | |
| NaN, ±Infinity, a cycle | the record is not written | `json.Marshal` fails |
| details that are not an object (a string, an array) | `{"value": ...}` | |

An absent or empty details object gives no `details` field (Go's `omitempty`). `cmp` and the
comparison helpers add `left`/`op`/`right`, `eventually` adds `elapsed_ms`/`within_ms`, and a
fired fault point adds `hit`: each to the caller's own object (or `Map`) when one is given, as
the Go SDK writes into the caller's map.

**Go version note.** The conformance test pins Go 1.26.5, the version this repository's Go
programs are built with. Go 1.27 changed one detail of `encoding/json`: invalid UTF-8 is written
as a raw U+FFFD character instead of the escape `�`. This package writes the escape (the
Go 1.26 form); everything else is the same for both Go versions.

## Eventually

`eventually(id, msg, withinMs)` declares a bounded-liveness property. One episode = one anchor
and one deadline, measured on the monotonic clock (`process.hrtime`; deterministic under hydo):

- `await l.awaitCond(cond, pollMs, d)` anchors now, evaluates `cond` (sync or async) every
  `pollMs` (with `setTimeout`, so the event loop keeps running) until it is true or the
  deadline has passed, writes one record and resolves to the outcome. The last evaluation
  happens at or after the deadline, so a condition that becomes true exactly then still counts.
- `l.awaitCondSync(cond, pollMs, d)` does the same and blocks the thread between evaluations
  (`Atomics.wait`): only for scripts that have nothing else to do.
- `w = l.begin()` anchors now; `w.done(d)` records "true now" (ok if in time), `w.expire(d)`
  records a miss. Only the first of them counts for one watch.

## BUGGIFY

Same decisions as the Go SDK, bit for bit: configured by `$HYDO_BUGGIFY`, or else the file
`$HYDO_BUGGIFY_FILE` (default `/tmp/hydo-buggify`), holding
`<seed-hex>[,<enable-permille>[,<fire-permille>]]` (defaults 250 and 250, FoundationDB's values;
parsed as Go parses it: hex seed without `0x`, Go's whitespace trimming, a bad enable or fire
value keeps the default). It is read once, at the first evaluation of any point, which also
writes `HYDO-BUGGIFY {"seed":..,"enable_pm":..,"fire_pm":..,"enabled":[..]}`. A point is enabled
for the run when `splitmix64(seed ^ fnv1a(id)) % 1000 < enable`; its k-th evaluation fires when
`splitmix64(splitmix64(seed ^ fnv1a(id)) + k·0x9e3779b97f4a7c15) % 1000 < fire` (64-bit
arithmetic with `BigInt`). Each point is also a `sometimes` property `buggify.<id>`
("fault point fired: <msg>"), recorded at its first firing with `{"hit": k}`. Without a
configuration every point returns false and has no other effect. Under hydo the schedule token
`B=<seed>` (`explore.py --buggify`) reaches the file through the guest init: an onboarded
project gets it with `buggify: true` in its `hydo.yaml` (`hvx/onboard/MANIFEST.md`).

## Tests

- `npm test` (`node --test test/unit.test.js`): what the Go program cannot express (cycles,
  `undefined`, `Map`/`Set`, `Date`, async liveness, the sink switches) and the BUGGIFY rates
  over 400 seeds.
- `test/conformance.sh`: builds `test/go-conformance/main.go` against the Go SDK's own source
  files (Go 1.26.5 by default, through `GOTOOLCHAIN`) and runs it and `test/conformance.js`
  (once through the CommonJS entry, once through the ESM entry) under five BUGGIFY
  configurations (seed in the environment, seed in the file, none, odd enable value, invalid
  seed); the consoles and stdouts must be byte-identical. The calls cover every type, dedup,
  comparisons, NaN/±Inf records, eventually, fault points sharing one details object, and
  details with nested maps and arrays, bytes, 64-bit integers, floats at every formatting
  boundary, HTML characters, control characters, U+2028/U+2029, astral characters (key order),
  and invalid UTF-8. Needs `go` and `node`.

## Example

`hvx/onboard/examples/node-kv/` is a complete example for the Mac backend: a key/value server
(ESM) and its client and oracle (CommonJS) using this package, the pinned aarch64 Node.js and a
`hydo-mac.yaml`.

## Limits

- Node.js only (it uses `fs`, `Buffer` and `process.hrtime`); not for browsers or Deno/Bun as
  tested targets. Node 16 or later (BigInt, `Atomics.wait`).
- Each worker thread has its own deduplication table and console file descriptor.
- No call-site helpers that capture the file and line (the Rust SDK's macros do); declare
  properties with explicit ids.
