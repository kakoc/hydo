# hydo-assert for Rust

[hydo](https://github.com/kakoc/hydo) runs a whole system in a deterministic virtual machine,
injects faults (process kills, network faults, power cuts, disk errors) and replays every
failure bit for bit. This crate lets a Rust program that runs under hydo say, from the inside,
what must be true and what should happen. hydo collects what the program reports, and an
exploration (many runs with different faults) reports each property as passed, failed (with a
schedule that replays the failure) or never exercised.

The crate has no dependencies. Outside hydo it does almost nothing: the records go to standard
error (or nowhere), and fault points stay off.

## Install

```toml
[dependencies]
hydo-assert = "0.2"
```

## A complete program

A small key/value store that declares three properties and checks them:

```rust
use hydo_assert::{declare, details};
use std::collections::HashMap;

declare! {
    static READ_BACK: always("kv.read_back", "every acknowledged write reads back with its value");
    static OVERWRITE: sometimes("kv.overwrite", "a write replaced an existing value");
    static LOST_REPLY: buggify("kv.lost_reply", "the write lands but its reply is lost");
}

fn main() {
    hydo_assert::catalog(); // list every property, also the ones that never run
    let mut store = HashMap::new();
    hydo_assert::setup_complete(); // the system is ready: hydo may start injecting faults

    for (key, value) in [("a", 1), ("b", 2), ("a", 3)] {
        let old = store.insert(key, value);
        OVERWRITE.check(old.is_some(), details! {"key": key});
        if LOST_REPLY.fire(details! {"key": key}) {
            continue; // the client never hears back (only when hydo turns this point on)
        }
        let acked = store.get(key) == Some(&value);
        READ_BACK.check(acked, details! {"key": key, "value": value});
    }
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
| `always_or_unreachable` | the condition is true every time it is checked | it is false once |
| `sometimes` | the condition is true at least once in an exploration | it is never true |
| `reachable` | the code is reached at least once in an exploration | it is never reached |
| `unreachable` | the code is never reached | it is reached |

Declare properties with `declare!`, so that the catalog lists them before `main` runs:

```rust
use hydo_assert::{declare, details};

declare! {
    static STORED: always("kv.stored", "every acknowledged write is stored");
    static PARSES: always_or_unreachable("kv.log_parses", "every complete log line parses");
    static RETRIED: sometimes("kv.retried", "a client retried a request");
    static RECOVERED: reachable("kv.recovered", "the server replayed a non-empty log");
    static UNKNOWN: unreachable("kv.unknown_command", "the server got a command it does not know");
}

fn main() {
    STORED.check(true, details! {"key": "a"}); // returns the condition
    PARSES.check(true, ());                    // `()` means no details
    RETRIED.check(false, ());                  // a false `sometimes` writes nothing
    RECOVERED.hit(details! {"entries": 3});
    let command = "GET";
    if command != "GET" && command != "SET" {
        UNKNOWN.hit(details! {"command": command});
    }
}
```

Or declare and check in one place with the macros. Their records also carry the file and line:

```rust
use hydo_assert::{hydo_always, hydo_reachable, hydo_sometimes};

let (len, cap) = (12, 10_000);
hydo_always!(len <= cap, "kv.log_size" => "the log stays under its cap", {"len": len});
hydo_sometimes!(len > 10, "the log holds more than ten entries"); // no id: the message is the id
hydo_reachable!("kv.compacted" => "the log was compacted");
```

`hydo_always_or_unreachable!` and `hydo_unreachable!` work the same way. A property can also be
declared at run time with `hydo_assert::always(id, message)` and the other functions of the same
names; it joins the catalog when the call runs.

### Compare two values

`cmp` checks `left op right` and puts both values and the operator into the details, so a
failure shows them:

```rust
use hydo_assert::{always, hydo_always_cmp};

let log_size = always("kv.log_size_cmp", "the log stays under its cap");
let size = 12_u64;
log_size.cmp(size, "<=", 10_000, ()); // details: {"left":12,"op":"<=","right":10000}
hydo_always_cmp!(size, "<=", 10_000, "kv.log_cap" => "the log stays under its cap");
```

The operator is one of `<`, `<=`, `==`, `!=`, `>=` and `>`.

### Check that something happens in time (eventually)

An `eventually` property says that after this point, the condition becomes true within a
deadline. For example: after a restart, the server answers within 5 seconds. Time inside a hydo
guest is deterministic, so the verdict is the same on every replay.

```rust
use hydo_assert::eventually;
use std::time::{Duration, Instant};

let back = eventually("kv.back", "the server answers within 5 s of its start", Duration::from_secs(5));
let started = Instant::now();
let answers = || started.elapsed() >= Duration::from_millis(20); // stands for a real request
assert!(back.await_cond(answers, Duration::from_millis(5), ())); // polls every 5 ms

// Or mark the start and the end yourself:
let mut watch = back.begin();
// ... the server comes up ...
watch.done(()); // or watch.expire(()) when you know it will not
```

`await_cond` blocks the thread. In async code use `await_async`, which takes the check as a
closure that returns a future and works with any runtime:

```rust
use hydo_assert::eventually;
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let back = eventually("kv.back_async", "the server answers within 5 s", Duration::from_secs(5));
    let in_time = back
        .await_async(|| async { true }, Duration::from_millis(100), tokio::time::sleep, ())
        .await;
    assert!(in_time);
}
```

### Add a fault point (BUGGIFY)

A fault point is a place where your program misbehaves on purpose: it drops a reply, fails a
write, waits too long. Normally the point does nothing (`fire` returns false). hydo can turn
points on for a run, so that faults happen where only your program can make them. FoundationDB
calls this technique BUGGIFY.

```rust
use hydo_assert::{buggify, hydo_buggify};
use std::time::Duration;

let lost_reply = buggify("kv.lost_reply_point", "the write lands but its reply is lost");
let mut reply = Some("OK");
if lost_reply.fire(()) {
    reply = None; // misbehave: the client never gets the reply
}
if hydo_buggify!("kv.slow_disk" => "a write takes 10 ms longer") {
    std::thread::sleep(Duration::from_millis(10));
}
println!("{reply:?}");
```

To let hydo turn the points on, add `buggify: true` to the project's `hydo.yaml` and explore
with `hydo explore --buggify`. Each run then gets a seed that turns some points on, and a point
that is on fires on some of its calls. To try it outside hydo, set
`HYDO_BUGGIFY=<seed in hex>,<points on, per mille>,<calls that fire, per mille>`; for example
`HYDO_BUGGIFY=2a,1000,1000` makes every point fire every time.

### Record an operation history

hydo can check a whole history of operations after a run. It checks that every read is
consistent with the writes before it (linearizability per key), and that no list append goes
missing. Your workload's
client records each operation twice: when it starts (`invoke`) and when the outcome is known.

```rust
use hydo_assert::history::{enc, value_or_nil, History};

let kv = History::new("kv");
let write = kv.invoke(3, "write", &["k1", &enc("v1")]); // client 3, before the request
// ... send the request, then record the outcome:
write.ok_same(); // it happened; `fail()`: it did not; `info()`: the outcome is unknown
let read = kv.invoke(3, "read", &["k1"]);
read.ok(&["k1", &value_or_nil(Some("v1"))]);
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

The cargo feature `noop` compiles every record to nothing and makes every fault point return
false, for builds that must not write to the console at all.

### Build a program for a hydo guest

A hydo guest runs static Linux programs. Rust's musl targets build one without a C toolchain:

```sh
rustup target add x86_64-unknown-linux-musl
RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static" \
  cargo build --release --target x86_64-unknown-linux-musl
```

Use `aarch64-unknown-linux-musl` for hydo on a Mac. A complete project with a `hydo.yaml` is
the rust-kv example:
<https://github.com/kakoc/hydo/tree/main/hydo/onboard/examples/rust-kv>.

## Reference

- The API: <https://docs.rs/hydo-assert>.
- The records, and how hydo judges each property type:
  <https://github.com/kakoc/hydo/blob/main/hydo/ASSERTIONS.md>.
- The history format and the checkers:
  <https://github.com/kakoc/hydo/blob/main/hydo/HISTORIES.md>.
- The same SDK for Python, JavaScript and Go writes the same records, byte for byte:
  <https://github.com/kakoc/hydo/tree/main/sdk>.

Rules of this crate:

- The catalog (`catalog()`) first lists the properties of `declare!` and of the macros, sorted
  by where they are written (file, line, column). Then it lists the ones declared at run time, in
  the order they were declared. Call it once, early, in the process whose console hydo reads.
- Details are JSON objects with their keys in byte order. A record whose details hold a NaN or
  an infinity is not written.
- Every call is safe from threads and async tasks: one lock, held only while a record is
  written.
- The macros register their properties before `main` on Linux, Android, the BSDs, illumos and
  Apple targets. On other targets a property joins the catalog at its first check.
- The feature `serde_json` converts `serde_json` values into details.
- `minimize(id, msg)` and `maximize(id, msg)` declare a steered value: a number that says how
  close a run came to failing, for `hydo explore --steer <id>`. Its `value(v)` writes a
  `HYDO-STEER` record for the first number and for each number better than all earlier ones, at
  most 64 per id in a process. Only this SDK has them so far.

  ```rust
  let queue = hydo_assert::minimize("queue.len", "the test fails when the queue is empty");
  queue.value(18);
  ```

## License

Apache-2.0.
