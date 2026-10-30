# SDKs: assertions inside the programs, and tests written in code

All SDKs live in hydo's source repository and are not published to package registries yet.
Depend on them by path or git, or copy them into the project. The wire format and the verdict rules are in `hvx/ASSERTIONS.md` of hydo's repository.

## Assertion SDKs (in the services and in commands)

They write the same console records byte for byte, with the same deduplication and BUGGIFY
decisions, so the explorer and the reports read them unchanged. Properties: `always`,
`always_or_unreachable`, `sometimes`, `reachable`, `unreachable`, `eventually`, and BUGGIFY
fault points.

| language | where in hydo's repository | how a project uses it |
|---|---|---|
| Go | `hvx/stack/hydoassert` (import `hydostack/hydoassert`); history recorder `hvx/stack/hydohist` | copy or `replace` the module by path |
| Rust | `sdk/rust` (crate `hydo-assert`, standard library only; macros `hydo_always!`, `hydo_buggify!`, `declare!`; `hydo_assert::history`) | `hydo-assert = { path = ".../sdk/rust" }` |
| Python 3.8+ | `sdk/python` (distribution `hydo-assert`, import `hydo_assert`, standard library only, thread- and asyncio-safe); history recorder `hydo_assert.history` | `pip install ./sdk/python`, or copy `src/hydo_assert/` into the image |
| JavaScript / TypeScript (Node.js) | `sdk/js` (package `hydo-assert`, CommonJS and ESM, types, no dependencies); history recorder `hydo-assert/history` | `"hydo-assert": "file:.../sdk/js"`; in the image copy `package.json`, `index.js`, `index.mjs` (and `history.js`, `history.mjs` for the recorder) to `node_modules/hydo-assert/` |
| shell (busybox sh) | `hvx/stack/hydoassert/hydo_assert.sh` | source it; no deduplication, no BUGGIFY |

Use assertions for properties only the program can see (a code path that must never run, a
retry that should sometimes succeed, a recovery path that should be reached). Check files cover
what the outside can observe. `buggify: true` in `hydo.yaml` turns on the BUGGIFY seed for
programs that carry fault points.

## The Rust test SDK: the whole test as one Rust program

`sdk/rust-test` (crate `hydo-test`) writes the same plan that the `hydo/` folder describes, from
Rust code: a `system()` builder on the host (services, files, data disk, knobs, faults, composer
options, regressions), and drivers and checks as functions:

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

`cargo install --path sdk/rust-test` installs `cargo-hydo`; then `cargo hydo plan`, `cargo hydo
build`, `cargo hydo run "disk=none | run"`, `cargo hydo explore` call the installed `hydo`.
Results map to the composer's exit codes: a driver's `Err(fail!(..))` is a failed operation,
`Err(violation!(..))` fails the run; a check's `Err(violation!(..))` is a violation and
`Err(fail!(..))` "cannot be evaluated now". Not yet in the Rust SDK: importing Compose files and
fakes. Example in hydo's repository: `hvx/onboard/examples/redis-composed-rs/`. Other languages
have no test SDK yet; use the `hydo/` folder.
