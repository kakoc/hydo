# hydo-assert: the Rust assertion SDK

A Rust program running under hydo (the deterministic hypervisor in this repository) uses this
crate to say what must be true and what should happen at least once. It writes the same console
records as the Go SDK (`hydo/stack/hydoassert`), byte for byte, with the same deduplication
rules, so `hydo_assert_report`, the harnesses and `explore.py` read them unchanged. The wire
format and the verdict rules are in `hydo/ASSERTIONS.md`.

The crate uses only the standard library. It is not published; depend on it by path or git:

```toml
[dependencies]
hydo-assert = { path = "../hydo/sdk/rust" }          # or { git = "...", package = "hydo-assert" }
```

## Use

```rust
use hydo_assert::{declare, details, hydo_always, hydo_buggify, hydo_reachable, hydo_sometimes};
use std::time::Duration;

declare! {   // named properties, in the catalog before main, in this order
    static ACKED: always("kv.acked_durable", "every acknowledged write reads back with its value");
    static RECOVERED: sometimes("kv.recovered", "the server replayed a non-empty log");
    static BACK: eventually("kv.back", "the server answers within 5 s", Duration::from_secs(5));
    static LOST_REPLY: buggify("kv.put.lost_reply", "the write lands but the reply is lost");
}

fn main() {
    hydo_assert::catalog();                      // once, early, in one process: HYDO-ASSERT-DECL lines
    ACKED.check(got == want, details! {"key": k, "got": got});
    hydo_always!(len <= cap, "kv.log_size" => "the log stays under its cap", {"len": len});
    hydo_sometimes!(retried, "a request needed a retry");      // no id given: the message is the id
    hydo_reachable!("kv.torn_tail" => "the log ended in a partial line");
    hydo_assert::hydo_always_less_than_or_equal_to!(len, cap, "kv.log_size2" => "len <= cap");
    if LOST_REPLY.fire(()) { return; }          // or inline: hydo_buggify!("id" => "message")
    BACK.await_cond(|| ping().is_ok(), Duration::from_millis(100), ());
    hydo_assert::setup_complete();               // HYDO-SETUP-COMPLETE
}
```

| Go SDK (`hydoassert`) | Rust (`hydo_assert`) |
|---|---|
| `Always(id, msg)`, `AlwaysOrUnreachable`, `Sometimes`, `Reachable`, `Unreachable` | the same names in lower case (run-time declaration, registered at once), `declare!` entries, or the call-site macros `hydo_always!`, `hydo_always_or_unreachable!`, `hydo_sometimes!`, `hydo_reachable!`, `hydo_unreachable!` |
| `a.Check(cond, d)`, `a.Hit(d)` | `a.check(cond, d)`, `a.hit(d)`; `d` is a `Details`, `details!{..}`, `()` or `None` |
| `a.Cmp(left, op, right, d)` | `a.cmp(left, op, right, d)` (any ordered value, not only `i64`); macros `hydo_always_cmp!`, `hydo_sometimes_cmp!` and the named `hydo_always_greater_than!`, `..._greater_than_or_equal_to!`, `..._less_than!`, `..._less_than_or_equal_to!`, and the same four for `hydo_sometimes_` |
| `Eventually(id, msg, within)`, `l.Await(cond, poll, d)`, `l.Begin()`, `w.Done(d)`, `w.Expire(d)` | `eventually(..)`, `declare!{.. eventually(..)}` or `hydo_eventually!(within, "id" => "msg")`; `l.await_cond(cond, poll, d)`, `l.await_async(cond, poll, sleep, d).await`, `l.begin()`, `w.done(d)`, `w.expire(d)` |
| `Buggify(id, msg)`, `p.Fire(d)` | `buggify(..)`, `declare!{.. buggify(..)}`, `p.fire(d)`, or inline `hydo_buggify!("id" => "msg")` |
| `Catalog()`, `SetupComplete()` | `catalog()`, `setup_complete()` |
| `D{...}` | `details!{...}`, `Details`, alias `D` |

## Records, deduplication, catalog

- Records go to `/dev/ttyS0` (opened once, `O_APPEND|O_NOCTTY`, one `write` per record).
  `HYDO_ASSERT_OUT=<path>` writes to that existing file, `stderr` to standard error (plain
  logging outside hydo), `off` nowhere. If the file cannot be opened, standard error is used.
- A process writes each (assertion, outcome) pair once: the first pass and the first failure,
  with the details of that evaluation. A `sometimes` that evaluated false is never written.
  `unreachable` is written with `ok:false`. A record whose details hold a NaN or an infinity
  is not written at all (as Go's `json.Marshal` fails on it).
- Details are JSON objects with keys in bytewise order, encoded exactly as Go's
  `encoding/json` encodes them (HTML characters and U+2028/U+2029 escaped, Go's float format;
  `f32` values as Go encodes a `float32`).
- The catalog (`catalog()`) lists, in this order: every `declare!` entry and every call-site
  macro, sorted by where it is written (file, line, column, then the order inside one
  `declare!`), and then the assertions declared at run time, in declaration order. The
  `declare!` entries and macro sites are registered before `main` by a constructor
  (`.init_array` on ELF, `__mod_init_func` on Mach-O; no allocation, no lock), so a property
  in code that never runs is still listed and reported as never evaluated.
- Call-site macros add `"loc":"<file>:<line>"` as the last field of their records (the
  parsers ignore unknown fields; ASSERTIONS.md allows new ones). Run-time and `declare!`
  assertions carry no `loc`, so their records are identical to the Go SDK's.

## BUGGIFY

Same decisions as the Go SDK: configured by `$HYDO_BUGGIFY`, or else the file
`$HYDO_BUGGIFY_FILE` (default `/tmp/hydo-buggify`), holding
`<seed-hex>[,<enable-permille>[,<fire-permille>]]` (defaults 250 and 250, FoundationDB's
values). It is read once, at the first evaluation of any point, which also writes
`HYDO-BUGGIFY {"seed":..,"enable_pm":..,"fire_pm":..,"enabled":[..]}`. A point is enabled
for the run when `splitmix64(seed ^ fnv1a(id)) % 1000 < enable`; its k-th evaluation fires
when `splitmix64(splitmix64(seed ^ fnv1a(id)) + k·0x9e3779b97f4a7c15) % 1000 < fire`. Each
point is also a `sometimes` property `buggify.<id>` ("fault point fired: <msg>"), recorded at
its first firing with `{"hit": k}`. Without a configuration every point returns false and has
no other effect. Under hydo the schedule token `B=<seed>` (`explore.py --buggify`) reaches the
file through the guest init: the stack's `stack-explore.sh` writes it, and an onboarded
project gets it with `buggify: true` in its `hydo.yaml` (`hydo/onboard/MANIFEST.md`).

## Eventually, async code

`await_cond` blocks the thread (it sleeps `poll` between checks). In async code use
`await_async`, which takes a closure returning the check's future and a closure returning a
sleep future, so it does not depend on a runtime:

```rust
live.await_async(|| async { client.ping().await.is_ok() }, Duration::from_millis(100),
                 |d| tokio::time::sleep(d), ()).await;
```

Every other call is a short, non-blocking-in-practice write to the console and is safe from
async tasks and threads (one global lock, held only while a record is formatted and written).

## Features

- `noop`: every record, catalog and lifecycle call compiles to nothing and every BUGGIFY point
  returns false. Conditions are still evaluated and returned. Use it for builds that must not
  touch the console; at run time `HYDO_ASSERT_OUT=off` does the same for the records (but
  leaves BUGGIFY working).
- `serde_json`: `From<serde_json::Value>` for `Value` and `Details`, so
  `a.check(ok, serde_json::json!({"k": v}))` works.

## Extension point for other record kinds

`hydo_assert::emit_line(tag, &value)` writes one line `<tag> <json>` through the same sink with
one write, for other JSON record kinds.

## Operation histories (`hydo_assert::history`)

The typed recorder for hydo's host-side history checkers (`hydo/HISTORIES.md`): `History::new("kv")`,
`.invoke(proc, f, &args) -> Op`, then `op.ok(&results)` / `op.ok_same()` / `op.fail()` /
`op.info()`; value helpers `enc`, `ver(v, version)`, `value_or_nil`, `list`, and the txn
micro-ops `append_mop`, `read_mop`, `read_result_mop`. Its lines are not JSON
(`HYDO-HIST <history> <process> <type> <time-ns> <f> <args>`), so it has its own sink: one write
per record to `/dev/console`, `HYDO_HIST_OUT=<path>|stderr|off`; `noop` turns it off.
`tests/history.rs` checks the exact lines (the same as the Go recorder's test).

## Building a static binary for a hydo guest

The guest runs any static x86_64 Linux binary. Rust's `x86_64-unknown-linux-musl` target
links one without a C toolchain (pinned: Rust 1.90.0):

```sh
rustup target add x86_64-unknown-linux-musl --toolchain 1.90.0
RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static" \
  cargo +1.90.0 build --release --target x86_64-unknown-linux-musl
```

`hydo/onboard/examples/rust-kv/` is a complete example: a key/value server using this crate,
its `build-rkv.sh` (two clean builds give the same sha256), and a `hydo.yaml`.

## Tests

- `cargo test` (also with `--features noop` and `--features serde_json`): the encoder, the
  BUGGIFY decisions and rates, the macros, the catalog order, the async eventually.
- `tests/conformance.sh`: builds `tests/go-conformance/main.go` against the Go SDK's own
  source files and `examples/conformance.rs`, runs both under three BUGGIFY configurations
  (seed in the environment, seed in the file, none) and requires their consoles and stdouts
  to be byte-identical. Needs `go` and `cargo`.

## Limits

- Constructors exist on Linux, Android, the BSDs, illumos/Solaris and Apple targets. Elsewhere
  (Windows, wasm) a static assertion joins the catalog only at its first evaluation.
- A macro site in a function the linker drops (an unused function in a dependency's object
  file that nothing references) is not registered; code that can run is always registered.
- The catalog order differs from Go's "declaration order" for `declare!`/macro sites across
  files (it is sorted by file and line, which is a pure function of the source); run-time
  declarations keep Go's order.
