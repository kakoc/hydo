# hydo-assert: the Python assertion SDK

A Python program running under hydo (the deterministic hypervisor in this repository) uses this
package to say what must be true and what should happen at least once. It writes the same
console records as the Go SDK (`hvx/stack/hydoassert`), byte for byte, with the same
deduplication rules, so `hydo_assert_report`, the harnesses and `explore.py` read them
unchanged. The wire format and the verdict rules are in `hvx/ASSERTIONS.md`.

- Distribution name `hydo-assert`, import name `hydo_assert`.
- Standard library only; Python 3.8 or later; thread-safe and safe to call from asyncio code.
- Not published. Install it from this directory (`pip install ./sdk/python`), or put
  `sdk/python/src` on `PYTHONPATH`, or copy `src/hydo_assert/` into the guest image (the
  `hvx/onboard/examples/python-kv` example does that).

## Use

```python
import hydo_assert as ha

# Declare at module level: the catalog is complete once the module is imported.
ACKED = ha.always("kv.acked_durable", "every acknowledged PUT reads back with its value")
PARSES = ha.always_or_unreachable("kv.log_parses", "every complete log line parses on replay")
SIZE = ha.always("kv.log_size", "the log holds at most 10000 entries")
REPLAYED = ha.sometimes("kv.recovered", "the server replayed a non-empty log at start")
TORN = ha.reachable("kv.torn_tail", "the log ended in a partial line")
BAD = ha.unreachable("kv.bad_command", "the server got a command it does not know")
BACK = ha.eventually("kv.back", "the server answers within 5 s", 5.0)        # seconds or timedelta
LOST = ha.buggify("kv.put.lost_reply", "the PUT is logged but its reply is lost")

ha.catalog()                                       # once, early: the HYDO-ASSERT-DECL lines
ACKED.check(got == want, {"key": k, "got": got})   # returns bool(cond)
REPLAYED.check(n > 0, {"entries": n})
TORN.hit({"bytes": len(tail)})
BAD.hit()
SIZE.less_than_or_equal_to(n, 10000)               # = SIZE.cmp(n, "<=", 10000)
if LOST.fire({"key": k}):                          # BUGGIFY fault point
    return
BACK.await_cond(lambda: ping() == "PONG", poll=0.2)   # blocks; one record
w = BACK.begin(); ...; w.done()                    # or: anchor now, condition true now / w.expire()
ha.setup_complete()                                # HYDO-SETUP-COMPLETE
```

In asyncio code use `await BACK.await_async(cond, poll)`: `cond` may be a plain function or an
`async def`, and the wait between checks is `asyncio.sleep`. Every other call is a short write
to the console under one process-wide lock that is never held across a blocking call or an
`await`, so it is safe from coroutines and from threads.

| Go SDK (`hydoassert`) | Python (`hydo_assert`) |
|---|---|
| `Always(id, msg)`, `AlwaysOrUnreachable`, `Sometimes`, `Reachable`, `Unreachable` | `always(id, msg)`, `always_or_unreachable`, `sometimes`, `reachable`, `unreachable` (return an `Assertion`) |
| `a.Check(cond, d)`, `a.Hit(d)` | `a.check(cond, details=None)`, `a.hit(details=None)` |
| `a.Cmp(left, op, right, d)` | `a.cmp(left, op, right, details=None)`, and the named helpers `a.greater_than`, `a.greater_than_or_equal_to`, `a.less_than`, `a.less_than_or_equal_to(left, right, details=None)` |
| `Eventually(id, msg, within)`, `l.Await(cond, poll, d)`, `l.Begin()`, `w.Done(d)`, `w.Expire(d)` | `eventually(id, msg, within)`, `l.await_cond(cond, poll, details=None)`, `await l.await_async(cond, poll, details=None)`, `l.begin()`, `w.done(details=None)`, `w.expire(details=None)` |
| `Buggify(id, msg)`, `p.Fire(d)` | `buggify(id, msg)`, `p.fire(details=None)` |
| `Catalog()`, `SetupComplete()` | `catalog()`, `setup_complete()` |
| `D{...}` | any mapping (a `dict`); `hydo_assert.D` is `dict` |

The naming follows the Rust SDK (`sdk/rust`, branch `rust-sdk`): the same lower-case names,
`await_cond`/`await_async` for Go's `Await`, `emit_line(tag, value)` as the extension point for
other record kinds.

## Records, deduplication, catalog

- Records go to `/dev/ttyS0` (opened once, `O_WRONLY|O_APPEND|O_NOCTTY`, one `write` per
  record). `HYDO_ASSERT_OUT=<path>` writes to that existing file, `stderr` to standard error
  (plain logging outside hydo), `off` nowhere, and `logging` (Python only) logs each record line
  at INFO on the logger `hydo_assert`. If the file cannot be opened, standard error is used.
- A process writes each (assertion, outcome) pair once: the first pass and the first failure,
  with the details of that evaluation. A `sometimes` that evaluated false is never written.
  `unreachable` is written with `ok:false`. A record whose details hold a NaN or an infinity is
  not written at all, but still counts as written (as in Go, where `json.Marshal` fails after
  the pair was marked).
- The catalog (`catalog()`) lists every declared assertion in declaration order: the order in
  which the declaring statements ran, which for module-level declarations is the import order.
  A module that is never imported contributes nothing; import it before calling `catalog()`.
  Call `catalog()` in the process whose console output is collected for each run (for a
  server started before the explorer's snapshot point, that is a client or the oracle, not the
  server; see the example).
- Details are encoded exactly as Go's `encoding/json` encodes the equivalent Go value: keys in
  UTF-8 byte order, `<`, `>`, `&`, U+2028 and U+2029 escaped, other non-ASCII text raw, Go's
  float format (`1e+21`, `1.5e-7`, `3` for `3.0`, `-0`). The caller's dict is never changed
  (Go adds `left`/`op`/`right`, `hit`, `elapsed_ms` to the caller's map; Python adds them to a
  copy, which gives the same record).
- Python values Go has no form for: a tuple is an array; `bytes` is a base64 string (as Go
  encodes `[]byte`); a set is an array sorted by encoded element; integer dict keys are written
  in decimal; any other object is the string `str(obj)`. A lone surrogate (an undecodable byte
  read with `surrogateescape`) is written as `�`, as the pinned guest Go 1.26.5 writes an
  invalid UTF-8 byte (Go 1.27 writes a raw U+FFFD instead).
- `details` that is not a mapping is wrapped as `{"value": details}`.
- After `os.fork()` the child keeps the parent's deduplication state (it is one program's
  history) and gets fresh locks.

## BUGGIFY

Same decisions as the Go SDK: configured by `$HYDO_BUGGIFY`, or else the file
`$HYDO_BUGGIFY_FILE` (default `/tmp/hydo-buggify`), holding
`<seed-hex>[,<enable-permille>[,<fire-permille>]]` (defaults 250 and 250, FoundationDB's
values), parsed as Go's `strconv.ParseUint` does (no `0x`, no sign, no spaces inside a field;
an unparsable seed leaves BUGGIFY off, an unparsable permille keeps its default). It is read
once, at the first evaluation of any point, which also writes
`HYDO-BUGGIFY {"seed":..,"enable_pm":..,"fire_pm":..,"enabled":[..]}`. A point is enabled
for the run when `splitmix64(seed ^ fnv1a(id)) % 1000 < enable`; its k-th evaluation fires
when `splitmix64(splitmix64(seed ^ fnv1a(id)) + k·0x9e3779b97f4a7c15) % 1000 < fire`. Each
point is also a `sometimes` property `buggify.<id>` ("fault point fired: <msg>"), recorded at
its first firing with `{"hit": k}`. Without a configuration every point returns False and has
no other effect. Under hydo the schedule token `B=<seed>` (`explore.py --buggify`) reaches the
file through the guest init: an onboarded project gets it with `buggify: true` in its
`hydo.yaml` (`hvx/onboard/MANIFEST.md`).

## Determinism

The records are a pure function of the program's evaluation sequence: no clocks (except the
guest's monotonic clock for `eventually`, which hvx makes deterministic), no host state,
catalog in declaration order, sorted keys. A Python program is itself deterministic under hydo:
the guest CRNG is seeded identically every run, so even the hash seed is the same; setting
`PYTHONHASHSEED=0` makes that independent of the CRNG. `PYTHONDONTWRITEBYTECODE=1` avoids
writing `.pyc` files on the guest's read-only root.

## Tests

- `python3 -m unittest discover -s sdk/python/tests`: the encoder (Go's float and string
  forms), deduplication, details not mutated, NaN, catalog order, 16 threads writing 2000
  evaluations each without an interleaved line, asyncio `eventually`, fork, BUGGIFY decisions
  and rates. Passes on Python 3.8.20 and 3.15.0.
- `sdk/python/tests/conformance.sh [python...]`: builds `tests/go-conformance/main.go` against
  the Go SDK's own source files and runs it and `tests/conformance.py` (the same calls) under
  eight BUGGIFY configurations: seed in the environment, seed in the file, none, and malformed
  ones (`  BEEF,07,+5\t`, `0xbeef`, a 65-bit seed, `ab, 300,1_0`, all points always firing).
  Consoles and stdouts must be byte-identical. `GO=<go binary>` selects the Go toolchain; with
  Go 1.26 or older the run also compares invalid UTF-8.

## Limits

- No call-site form that registers itself before it runs (the Rust SDK's macros do that with a
  linker section). A Python assertion is in the catalog once the statement that declares it has
  run, so declare at module level.
- `cmp` compares any two Python values that support the operator; values that cannot be
  compared evaluate to false. Go's `Cmp` takes only `int64`.
