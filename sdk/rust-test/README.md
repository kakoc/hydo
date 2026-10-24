# hydo-test: a hydo test as one Rust program

A team writes its test in Rust: a `system()` function that describes the system (services,
files, data disk, knobs, allowed faults, composer options, saved regressions), and the test's
operations and properties as ordinary functions with an attribute:

```rust
use hydo_test::prelude::*;

fn system() -> System {                  // runs on the HOST only
    System::new("shop")
        .file("bin/shopd", "/bin/shopd")
        .service(Service::new("shopd", "/bin/shopd --data /data/shop").ready("/bin/shopd ping").port(8080))
        .data(64, &["shop"])
        .steps(20)
        .test(TestParams { parallel: Some(4), ..Default::default() })
        .faults(&[Fault::Kill, Fault::Pause, Fault::Cut])
}

#[driver(parallel)]
fn place_order(ctx: &Ctx) -> Result<()> {
    let id = ctx.id().to_string();                       // unique per command in a run
    hydo_test::exec(&["/bin/shopctl", "order", &id], std::time::Duration::from_secs(2))?;
    ctx.ack(&id)                                         // the client saw it acknowledged
}

#[check(eventually, within = 20)]
fn orders_settle(_ctx: &Ctx) -> Result<()> { Ok(()) }   // Err(fail!(..)) = not yet

hydo_test::main!(system);
```

The binary has two lives:

- **Host:** `<bin> plan --guest-binary <path> --out .hydo/plan.json` calls `system()` and writes
  hydo's plan: the manifest format of `hvx/onboard/MANIFEST.md` as JSON (format 1, with
  `hydo_version`), with one `test.commands` or `test.checks` entry per attributed function,
  run as `/bin/<bin> driver <name>` or `/bin/<bin> check <name>`. Functions are listed in a
  fixed order (role, kind, name), never in link order, so the plan is a pure function of the
  program.
- **Guest:** hydo's composer runs those subcommands. `system()` is never called there.

Kinds: `#[driver(first | parallel | serial | singleton | anytime | eventually | finally)]` with
optional `timeout = <s>`, `name = "..."`, `msg = "..."`; `#[check(final | always | eventually)]`
with optional `every = <s>` (always), `within = <s>`, `poll = <s>`, `after = "start"`
(eventually), `msg = "..."`. MANIFEST.md "The test composer" says when each kind runs.

Results map to the composer's exit statuses: a driver's `Ok` is 0, `Err(fail!(..))` 1 (the
operation failed: recorded, normal under faults), `Err(violation!(..))` 100 (the run fails); a
check's `Ok` is 0 (holds), `Err(violation!(..))` 1 (violated), `Err(fail!(..))` 2 (could not be
evaluated now). The error's message is printed and becomes the property's details.

`Ctx`: `seed()` (the composition's number for this command, for its own choices), `below(n)`
(deterministic draws from it), `id()`, `name()`, `round()`, `env(k)`, `ack(id)` /
`outcome(id, "unknown")` / `acked()` (the acknowledgement log the checks read; carried across a
power cut), `exec(argv, deadline)`.

## cargo hydo

`cargo install --path sdk/rust-test` installs `cargo-hydo`. In the test crate:

```sh
cargo hydo plan  [--arch x86_64|aarch64]   # host build + static musl guest build -> .hydo/plan.json, .hydo/guest/<bin>
cargo hydo build                           # plan, then `hydo build .hydo/plan.json --out .hydo`
cargo hydo run "disk=none | run"           # plan + build, then `hydo run ...` (also explore, repro, save, report)
```

The guest binary is linked with Rust's self-contained musl and rust-lld (no C toolchain) and
host paths are remapped, so a pinned toolchain (`rust-toolchain.toml`) gives the same bytes
on every build. `--arch` defaults to aarch64 on macOS (the Mac backend) and x86_64 elsewhere;
`HYDO` names the engine command. The plan's file paths are relative to `.hydo/`, so `.hydo/`
can be copied to a build machine as a whole.

Example: `hvx/onboard/examples/redis-composed-rs/` (the Rust version of
`examples/redis-composed`, with the same command and check ids, so a schedule composes the same
commands in the same rounds in both).

Not yet: `System::from_compose` (the Docker Compose importer is another branch), fakes, log
rules and known errors (PRODUCT.md "Code first"), and re-exporting the assertion SDK
(`hydo-assert`, branch `rust-sdk`) once it is on main.
