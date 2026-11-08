# hydo-assert for Python

[hydo](https://github.com/kakoc/hydo) runs a whole system in a deterministic virtual machine,
injects faults (process kills, network faults, power cuts, disk errors) and replays every
failure bit for bit. This package lets a Python program that runs under hydo say, from the
inside, what must be true and what should happen. hydo collects what the program reports, and
an exploration (many runs with different faults) reports each property as passed, failed (with
a schedule that replays the failure) or never exercised.

The package uses only the standard library and needs Python 3.8 or later. Outside hydo it does
almost nothing: the records go to standard error (or nowhere), and fault points stay off.

## Install

```sh
pip install hydo-assert
```

The distribution is `hydo-assert`; the module is `hydo_assert`. In a guest image without pip,
copy the folder `hydo_assert` onto the program's `PYTHONPATH`.

## A complete program

A small key/value store that declares three properties and checks them:

```python
import hydo_assert as ha

READ_BACK = ha.always("kv.read_back", "every acknowledged write reads back with its value")
OVERWRITE = ha.sometimes("kv.overwrite", "a write replaced an existing value")
LOST_REPLY = ha.buggify("kv.lost_reply", "the write lands but its reply is lost")


def main():
    ha.catalog()  # list every property, also the ones that never run
    store = {}
    ha.setup_complete()  # the system is ready: hydo may start injecting faults

    for key, value in [("a", 1), ("b", 2), ("a", 3)]:
        old = store.get(key)
        store[key] = value
        OVERWRITE.check(old is not None, {"key": key})
        if LOST_REPLY.fire({"key": key}):
            continue  # the client never hears back (only when hydo turns this point on)
        acked = store.get(key) == value
        READ_BACK.check(acked, {"key": key, "value": value})


if __name__ == "__main__":
    main()
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
| `always_or_unreachable` | the condition is true every time it is checked | it is false once |
| `sometimes` | the condition is true at least once in an exploration | it is never true |
| `reachable` | the code is reached at least once in an exploration | it is never reached |
| `unreachable` | the code is never reached | it is reached |

Declare properties at module level, so that the catalog lists them once the module is
imported:

```python
import hydo_assert as ha

STORED = ha.always("kv.stored", "every acknowledged write is stored")
PARSES = ha.always_or_unreachable("kv.log_parses", "every complete log line parses")
RETRIED = ha.sometimes("kv.retried", "a client retried a request")
RECOVERED = ha.reachable("kv.recovered", "the server replayed a non-empty log")
UNKNOWN = ha.unreachable("kv.unknown_command", "the server got a command it does not know")

STORED.check(True, {"key": "a"})  # returns the condition
PARSES.check(True)  # the details are optional
RETRIED.check(False)  # a false `sometimes` writes nothing
RECOVERED.hit({"entries": 3})
command = "GET"
if command not in ("GET", "SET"):
    UNKNOWN.hit({"command": command})
```

### Compare two values

`cmp` checks `left op right` and puts both values and the operator into the details, so a
failure shows them:

```python
import hydo_assert as ha

LOG_SIZE = ha.always("kv.log_size", "the log stays under its cap")
size = 12
LOG_SIZE.cmp(size, "<=", 10000)  # details: {"left":12,"op":"<=","right":10000}
```

The operator is one of `<`, `<=`, `==`, `!=`, `>=` and `>`.

### Check that something happens in time (eventually)

An `eventually` property says "after this point, the condition becomes true within a deadline",
for example "after a restart the server answers within 5 seconds". Time inside a hydo guest is
deterministic, so the verdict is the same on every replay.

```python
import time

import hydo_assert as ha

BACK = ha.eventually("kv.back", "the server answers within 5 s of its start", 5.0)  # seconds
started = time.monotonic()


def answers():
    return time.monotonic() - started >= 0.02  # stands for a real request


assert BACK.await_cond(answers, poll=0.005)  # polls every 5 ms

# Or mark the start and the end yourself:
watch = BACK.begin()
# ... the server comes up ...
watch.done()  # or watch.expire() when you know it will not
```

`await_cond` blocks the thread. In asyncio code use `await_async`, whose condition may be a
plain function or a coroutine function:

```python
import asyncio

import hydo_assert as ha

BACK = ha.eventually("kv.back_async", "the server answers within 5 s", 5.0)


async def answers():
    return True


async def main():
    assert await BACK.await_async(answers, 0.1)


asyncio.run(main())
```

### Add a fault point (BUGGIFY)

A fault point is a place where your program misbehaves on purpose: it drops a reply, fails a
write, waits too long. Normally the point does nothing (`fire` returns false). hydo can turn
points on for a run, so that faults happen where only your program can make them. FoundationDB
calls this technique BUGGIFY.

```python
import time

import hydo_assert as ha

LOST_REPLY = ha.buggify("kv.lost_reply_point", "the write lands but its reply is lost")
SLOW_DISK = ha.buggify("kv.slow_disk", "a write takes 10 ms longer")

reply = "OK"
if LOST_REPLY.fire():
    reply = None  # misbehave: the client never gets the reply
if SLOW_DISK.fire({"key": "a"}):
    time.sleep(0.01)
print(reply)
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

```python
from hydo_assert import history as hh

kv = hh.History("kv")
write = kv.invoke(3, "write", "k1", hh.enc("v1"))  # client 3, before the request
# ... send the request, then record the outcome:
write.ok()  # it happened; fail(): it did not; info(): the outcome is unknown
read = kv.invoke(3, "read", "k1")
read.ok("k1", hh.value_or_nil("v1"))
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

- `HYDO_ASSERT_OUT`: a path of an existing file, `stderr`, `off`, or `logging` (each record
  logged at INFO on the logger `hydo_assert`) for the properties;
- `HYDO_HIST_OUT`: a path (created if missing), `stderr`, or `off` for the histories.

### Run a Python program in a hydo guest

Set `PYTHONHASHSEED=0` so that the program does not depend on a random hash seed, and
`PYTHONDONTWRITEBYTECODE=1` because the guest's root file system is read-only. A complete
project with a `hydo.yaml` is the python-kv example:
<https://github.com/kakoc/hydo/tree/main/hydo/onboard/examples/python-kv>.

## Reference

- The API: the docstrings of `hydo_assert` and `hydo_assert.history` (`help(hydo_assert)`).
- The records and how hydo judges each kind of property:
  <https://github.com/kakoc/hydo/blob/main/hydo/ASSERTIONS.md>.
- The history format and the checkers:
  <https://github.com/kakoc/hydo/blob/main/hydo/HISTORIES.md>.
- The same SDK for Rust, JavaScript and Go writes the same records, byte for byte:
  <https://github.com/kakoc/hydo/tree/main/sdk>.

Rules of this package:

- The catalog (`catalog()`) lists the properties in the order they were declared, which for
  module-level declarations is the import order. Call it once, early, in the process whose
  console hydo reads.
- Details are encoded as Go's `encoding/json` encodes the same values: keys in UTF-8 byte
  order, a tuple or a set as an array, `bytes` as base64, any other object as `str(obj)`.
  Details that are not a mapping are wrapped as `{"value": ...}`. A record whose details hold a
  NaN or an infinity is not written. The caller's dict is never changed.
- Every call is safe from threads and from asyncio code: one lock, never held across a blocking
  call or an `await`. After `os.fork()` the child keeps the parent's record of what was written.
- A history's process is an int or a token without spaces; its arguments are tokens (encode
  free-form values with `enc`). Versions are int64 values. A process has at most one operation
  in flight.

## License

Apache-2.0.
