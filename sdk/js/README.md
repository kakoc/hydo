# hydo-assert for JavaScript (Node.js)

[hydo](https://github.com/kakoc/hydo) runs a whole system in a deterministic virtual machine,
injects faults (process kills, network faults, power cuts, disk errors) and replays every
failure bit for bit. This package lets a Node.js program that runs under hydo say, from the
inside, what must be true and what should happen. hydo collects what the program reports, and
an exploration (many runs with different faults) reports each property as passed, failed (with
a schedule that replays the failure) or never exercised.

The package has no dependencies, works from CommonJS and from ES modules, ships TypeScript
types and needs Node.js 16 or later. Outside hydo it does almost nothing: the records go to
standard error (or nowhere), and fault points stay off.

## Install

```sh
npm install hydo-assert
```

In a guest image without npm, copy `package.json`, `index.js` and `index.mjs` (and
`history.js` and `history.mjs` for histories) to `node_modules/hydo-assert/` next to the
program.

## A complete program

A small key/value store that declares three properties and checks them:

```js
const ha = require('hydo-assert'); // or: import * as ha from 'hydo-assert';

const READ_BACK = ha.always('kv.read_back', 'every acknowledged write reads back with its value');
const OVERWRITE = ha.sometimes('kv.overwrite', 'a write replaced an existing value');
const LOST_REPLY = ha.buggify('kv.lost_reply', 'the write lands but its reply is lost');

ha.catalog(); // list every property, also the ones that never run
const store = new Map();
ha.setupComplete(); // the system is ready: hydo may start injecting faults

for (const [key, value] of [['a', 1], ['b', 2], ['a', 3]]) {
  const old = store.get(key);
  store.set(key, value);
  OVERWRITE.check(old !== undefined, { key });
  if (LOST_REPLY.fire({ key })) {
    continue; // the client never hears back (only when hydo turns this point on)
  }
  const acked = store.get(key) === value;
  READ_BACK.check(acked, { key, value });
}
```

Run it with `HYDO_ASSERT_OUT=stderr` to see what it reports. It writes these lines (under hydo
they go to the guest's console, where hydo reads them):

```text
HYDO-ASSERT-DECL {"t":"always","id":"kv.read_back","msg":"every acknowledged write reads back with its value"}
HYDO-ASSERT-DECL {"t":"sometimes","id":"kv.overwrite","msg":"a write replaced an existing value"}
HYDO-ASSERT-DECL {"t":"sometimes","id":"buggify.kv.lost_reply","msg":"fault point fired: the write lands but its reply is lost"}
HYDO-SETUP-COMPLETE
HYDO-ASSERT {"t":"always","id":"kv.read_back","ok":true,"msg":"every acknowledged write reads back with its value","details":{"key":"a","value":1}}
HYDO-ASSERT {"t":"sometimes","id":"kv.overwrite","ok":true,"msg":"a write replaced an existing value","details":{"key":"a"}}
```

A process writes each outcome of a property once: the first pass and the first failure, with
the details of that evaluation. That is why the later passes of `kv.read_back` write nothing.

## How to

### Declare properties and check them

A property has an id (stable across versions of your program) and a message for people. Its
kind says what hydo expects:

| kind | the property holds when | hydo reports a failure when |
|---|---|---|
| `always` | the condition is true every time, and it is checked at least once | it is false once, or never checked |
| `alwaysOrUnreachable` | the condition is true every time it is checked | it is false once |
| `sometimes` | the condition is true at least once in an exploration | it is never true |
| `reachable` | the code is reached at least once in an exploration | it is never reached |
| `unreachable` | the code is never reached | it is reached |

Declare properties at the top level of a module, so that the catalog lists them once the
module is loaded:

```js
const ha = require('hydo-assert');

const STORED = ha.always('kv.stored', 'every acknowledged write is stored');
const PARSES = ha.alwaysOrUnreachable('kv.log_parses', 'every complete log line parses');
const RETRIED = ha.sometimes('kv.retried', 'a client retried a request');
const RECOVERED = ha.reachable('kv.recovered', 'the server replayed a non-empty log');
const UNKNOWN = ha.unreachable('kv.unknown_command', 'the server got a command it does not know');

STORED.check(true, { key: 'a' }); // returns the condition
PARSES.check(true); // the details are optional
RETRIED.check(false); // a false `sometimes` writes nothing
RECOVERED.hit({ entries: 3 });
const command = 'GET';
if (command !== 'GET' && command !== 'SET') {
  UNKNOWN.hit({ command });
}
```

### Compare two values

`cmp` checks `left op right` and puts both values and the operator into the details, so a
failure shows them:

```js
const ha = require('hydo-assert');

const LOG_SIZE = ha.always('kv.log_size', 'the log stays under its cap');
const size = 12;
LOG_SIZE.cmp(size, '<=', 10000); // details: {"left":12,"op":"<=","right":10000}
```

The operator is one of `<`, `<=`, `==`, `!=`, `>=` and `>`; the values are numbers or bigints.

### Check that something happens in time (eventually)

An `eventually` property says "after this point, the condition becomes true within a deadline",
for example "after a restart the server answers within 5 seconds". Time inside a hydo guest is
deterministic, so the verdict is the same on every replay.

```js
const ha = require('hydo-assert');

const BACK = ha.eventually('kv.back', 'the server answers within 5 s of its start', 5000); // ms
const started = Date.now();
const answers = async () => Date.now() - started >= 20; // stands for a real request

(async () => {
  const inTime = await BACK.awaitCond(answers, 5); // polls every 5 ms
  if (!inTime) throw new Error('late');

  // Or mark the start and the end yourself:
  const watch = BACK.begin();
  // ... the server comes up ...
  watch.done(); // or watch.expire() when you know it will not
})();
```

`awaitCond` keeps the event loop running between checks, and its condition may return a
promise. `awaitCondSync` blocks the thread instead, for scripts that have nothing else to do.

### Add a fault point (BUGGIFY)

A fault point is a place where your program misbehaves on purpose: it drops a reply, fails a
write, waits too long. Normally the point does nothing (`fire` returns false). hydo can turn
points on for a run, so that faults happen where only your program can make them. FoundationDB
calls this technique BUGGIFY.

```js
const ha = require('hydo-assert');

const LOST_REPLY = ha.buggify('kv.lost_reply_point', 'the write lands but its reply is lost');
const SLOW_DISK = ha.buggify('kv.slow_disk', 'a write takes 10 ms longer');

let reply = 'OK';
if (LOST_REPLY.fire()) {
  reply = null; // misbehave: the client never gets the reply
}
const delay = SLOW_DISK.fire({ key: 'a' }) ? 10 : 0;
setTimeout(() => console.log(reply), delay);
```

To let hydo turn the points on, add `buggify: true` to the project's `hydo.yaml` and explore
with `hydo explore --buggify`. Each run then gets a seed that turns some points on, and a point
that is on fires on some of its calls. To try it outside hydo, set
`HYDO_BUGGIFY=<seed in hex>,<points on, per mille>,<calls that fire, per mille>`; for example
`HYDO_BUGGIFY=2a,1000,1000` makes every point fire every time.

### Record an operation history

hydo can check a whole history of operations after a run: is every read consistent with the
writes before it (linearizability per key), or do list appends ever go missing? Your workload's
client records each operation twice: when it starts (`invoke`) and when the outcome is known.

```js
const hh = require('hydo-assert/history'); // or: import * as hh from 'hydo-assert/history';

const kv = hh.history('kv');
const write = kv.invoke(3, 'write', 'k1', hh.enc('v1')); // client 3, before the request
// ... send the request, then record the outcome:
write.ok(); // it happened; fail(): it did not; info(): the outcome is unknown
const read = kv.invoke(3, 'read', 'k1');
read.ok('k1', hh.valueOrNil('v1'));
```

Then name the history in the project's `hydo.yaml`:

```yaml
checks:
  - id: kv.linearizable
    type: linearizable
    model: register
    history: kv
    msg: every read sees the latest acknowledged write
```

`hydo guide checks` describes the checks and their options.

### Choose where the records go

Under hydo the records go to the guest's console (`/dev/ttyS0` for properties, `/dev/console`
for histories), which hydo reads even when the program's standard output goes to a log file.
Outside hydo, where the console cannot be opened, they go to standard error. Two variables
change this, and each is read once, at the first record:

- `HYDO_ASSERT_OUT`: a path of an existing file, `stderr`, or `off` for the properties;
- `HYDO_HIST_OUT`: a path (created if missing), `stderr`, or `off` for the histories.

### Run a Node.js program in a hydo guest

A complete project with a `hydo.yaml` for hydo on a Mac is the node-kv example:
<https://github.com/kakoc/hydo/tree/main/hydo/onboard/examples/node-kv>.

## Reference

- The API: the TypeScript declarations `index.d.ts` and `history.d.ts`, with a comment on every
  call (your editor shows them).
- The records and how hydo judges each kind of property:
  <https://github.com/kakoc/hydo/blob/main/hydo/ASSERTIONS.md>.
- The history format and the checkers:
  <https://github.com/kakoc/hydo/blob/main/hydo/HISTORIES.md>.
- The same SDK for Rust, Python and Go writes the same records, byte for byte:
  <https://github.com/kakoc/hydo/tree/main/sdk>.

Rules of this package:

- The catalog (`catalog()`) lists the properties in the order they were declared. Call it
  once, early, in the process whose console hydo reads.
- Details are encoded as Go's `encoding/json` encodes the same values: object and `Map` keys in
  UTF-8 byte order, a `Set` or a typed array as an array, a `Uint8Array` or `Buffer` as base64,
  a bigint as an integer, `undefined` and functions left out. Details that are not an object
  are wrapped as `{"value": ...}`. A record whose details hold a NaN, an infinity or a cycle is
  not written.
- Every record is one synchronous write, so it is on the console before the call returns. The
  ES module entry re-exports the CommonJS one, so a program that uses both has one state; a
  worker thread has its own.
- A history's process is an integer or a token without spaces; its arguments are tokens
  (encode free-form values with `enc`). Versions are int64 values (pass a bigint beyond 2^53).
  A process has at most one operation in flight.

## License

Apache-2.0.
