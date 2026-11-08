# SDKs: assertions inside the programs, and tests written in code

The assertion SDKs are published: `hydo-assert` on crates.io, PyPI, and npm, and the Go module
`github.com/kakoc/hydo/sdk/go`. The Rust test SDK is not published; depend on it by path or git.
The wire format and the verdict rules are in `hydo/ASSERTIONS.md` of hydo's repository.

## Assertion SDKs (in the services and in commands)

They write the same console records byte for byte. They make the same deduplication and BUGGIFY
decisions, so the explorer and the reports read them unchanged. Properties: `always`,
`always_or_unreachable`, `sometimes`, `reachable`, `unreachable`, `eventually`, and BUGGIFY
fault points; a numeric comparison is `cmp(left, op, right)` on an `always` or `sometimes`
property (Rust also `hydo_always_cmp!`).

| language | where in hydo's repository | how a project uses it |
|---|---|---|
| Go 1.22+ | `sdk/go` (module `github.com/kakoc/hydo/sdk/go`, package `hydoassert`); history recorder `hydohist` in the same module | `go get github.com/kakoc/hydo/sdk/go@v<version>` once published, or `replace` the module by path |
| Rust | `sdk/rust` (crate `hydo-assert`, standard library only; macros `hydo_always!`, `hydo_buggify!`, `declare!`; `hydo_assert::history`) | `hydo-assert = "<version>"` once published, or `hydo-assert = { path = ".../sdk/rust" }` |
| Python 3.8+ | `sdk/python` (distribution `hydo-assert`, import `hydo_assert`, standard library only, thread- and asyncio-safe); history recorder `hydo_assert.history` | `pip install hydo-assert` once published, or `pip install ./sdk/python`, or copy `src/hydo_assert/` into the image |
| JavaScript / TypeScript (Node.js) | `sdk/js` (package `hydo-assert`, CommonJS and ESM, types, no dependencies); history recorder `hydo-assert/history` | `npm install hydo-assert` once published, or `"hydo-assert": "file:.../sdk/js"`; in the image copy `package.json`, `index.js`, `index.mjs` (and `history.js`, `history.mjs` for the recorder) to `node_modules/hydo-assert/` |
| shell (busybox sh) | `hydo/stack/hydoassert/hydo_assert.sh` | source it; no deduplication, no BUGGIFY |

Use assertions for properties only the program can see (a code path that must never run, a
retry that should sometimes succeed, a recovery path that should be reached). Check files cover
what the outside can observe. `buggify: true` in `hydo.yaml` turns on the BUGGIFY seed for
programs that carry fault points.

## The Rust test SDK: the whole test as one Rust program

`sdk/rust-test` (crate `hydo-test`; not published to crates.io, so depend on it by path or git)
writes the same plan that the `hydo/` folder describes, from Rust code. A `system()` builder on
the host describes the services, files, data disk, knobs, faults, composer options, and
regressions. Drivers and checks are functions:

```rust
use hydo_test::prelude::*;

fn system() -> System {                 // runs on the host only, never in the guest
    System::new("shop")
        .file("bin/shopd", "/bin/shopd")
        .service(Service::new("shopd", "/bin/shopd --data /data/shop").ready("/bin/shopd ping").port(8080))
        .data(64, &["shop"])
        .steps(20)
        .faults(&[Fault::Kill, Fault::Pause, Fault::Cut])
}

#[driver(parallel)]
fn place_order(ctx: &Ctx) -> Result<()> {
    let id = ctx.id().to_string();
    hydo_test::exec(&["/bin/shopctl", "order", &id], std::time::Duration::from_secs(2))?;
    ctx.ack(&id)
}

#[check(eventually, within = 20)]
fn orders_settle(_ctx: &Ctx) -> Result<()> { Ok(()) }

hydo_test::main!(system);
```

`cargo install --path sdk/rust-test/cargo-hydo` installs `cargo-hydo`; then `cargo hydo plan`,
`cargo hydo build`, `cargo hydo run "disk=none | run"`, `cargo hydo explore` call the installed
`hydo`.
Results map to the composer's exit codes: a driver's `Err(fail!(..))` is a failed operation,
`Err(violation!(..))` fails the run; a check's `Err(violation!(..))` is a violation and
`Err(fail!(..))` means that the check cannot be evaluated now. Not yet in the Rust SDK: importing Compose files and
fakes. Example in hydo's repository: `hydo/onboard/examples/redis-composed-rs/`. Other languages
have no test SDK yet; use the `hydo/` folder.
