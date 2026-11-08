# hydo-test: a hydo test as one Rust program

A team writes its test in Rust. A `system()` function describes the system: services, files,
data disk, knobs, allowed faults, composer options, and saved regressions. The test's operations
and properties are ordinary functions with an attribute. hydo is the deterministic
hypervisor in [this repository](https://github.com/kakoc/hydo).

```toml
[dependencies]
hydo-test = { path = "../hydo/sdk/rust-test" }   # a checkout of the repository (or a git dependency)
```

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
  hydo's plan: the manifest format of `hydo/onboard/MANIFEST.md` as JSON (format 1, with
  `hydo_version`), with one `test.commands` or `test.checks` entry per attributed function,
  run as `/bin/<bin> driver <name>` or `/bin/<bin> check <name>`. Functions are listed in a
  fixed order (role, kind, name), never in link order, so the plan is a pure function of the
  program.
- **Guest:** hydo's composer runs those subcommands. `system()` is never called there.

Kinds: `#[driver(first | parallel | serial | singleton | anytime | eventually | finally)]` with
optional `timeout = <s>`, `name = "..."`, `msg = "..."`; `#[check(final | always | eventually)]`
with optional `every = <s>` (always), `within = <s>`, `poll = <s>`, `after = "start"`
(eventually), `msg = "..."`. MANIFEST.md "The test composer" says when each kind runs.

Results map to the composer's exit statuses:

| Function | Result | Exit status | Meaning |
|---|---|---|---|
| driver | `Ok` | 0 | done |
| driver | `Err(fail!(..))` | 1 | the operation failed; recorded, normal under faults |
| driver | `Err(violation!(..))` | 100 | the run fails |
| check | `Ok` | 0 | holds |
| check | `Err(violation!(..))` | 1 | violated |
| check | `Err(fail!(..))` | 2 | could not be evaluated now |
 The error's message is printed and becomes the property's details.

`Ctx`: `seed()` (the composition's number for this command, for its own choices), `below(n)`
(deterministic draws from it), `id()`, `name()`, `round()`, `env(k)`, `ack(id)` /
`outcome(id, "unknown")` / `acked()` (the acknowledgement log the checks read; carried across a
power cut), `exec(argv, deadline)`.

History checks (`hydo/HISTORIES.md`): drivers record their operations with
`hydo_test::assert::history::History`, with `ctx.id()` as the process, unique per command. The
plan declares a ready-made checker that the host runs after the run, for example:

- `System::history_check(HistoryCheck::linearizable("db.kv", "kv").model("cas").readback(&argv))`
- `HistoryCheck::list_append("db.log", "log").consistency("serializable")`

Each becomes a `test.checks` entry of kind `linearizable` or `list_append`. The optional
read-back argv runs at the end of the same boot, or of the recovery boot.

## cargo hydo

`cargo install --path sdk/rust-test/cargo-hydo` (from a checkout) installs the `cargo hydo`
subcommand; it is its own crate, so a test crate depends only on
`hydo-test`. In the test crate:

```sh
cargo hydo plan  [--arch x86_64|aarch64]   # host build + static musl guest build -> .hydo/plan.json, .hydo/guest/<bin>
cargo hydo build                           # plan, then `hydo build .hydo/plan.json --out .hydo`
cargo hydo run "disk=none | run"           # plan + build, then `hydo run ...` (also explore, repro, save, report)
```

The guest binary is linked with Rust's self-contained musl and rust-lld (no C toolchain) and
host paths are remapped, so a pinned toolchain (`rust-toolchain.toml`) gives the same bytes
on every build. `--arch` defaults to aarch64 on macOS (the Mac backend) and x86_64 elsewhere;
`HYDO` names the engine command; the plan step sees `HYDO_ARCH` (x86_64 or aarch64), so
`system()` can pick per-architecture files. The plan's file paths are relative to `.hydo/`, so `.hydo/`
can be copied to a build machine as a whole.

Example: `hydo/onboard/examples/redis-composed-rs/` (the Rust version of
`examples/redis-composed`, with the same command and check ids, so a schedule composes the same
commands in the same rounds in both).

**Command ids and saved regressions.** A driver's id is `<kind>_<name>` (`parallel_driver_transfer`
for `#[driver(parallel)] fn transfer`). Saved compositions name commands by this id
(`hydo/onboard/MANIFEST.md`, "Stable compositions"). Renaming a driver that a saved regression
uses makes that regression fail with a message that names it. Adding, removing, or reordering
other drivers does not change it. `<bin> plan` refuses two drivers (or two checks) with the same name,
since the guest runs them as `<bin> driver <name>`. The attribute macro writes a `content` hash
of each driver into the plan, so a regression warns when the driver changed since it was saved.
The hash is FNV-1a 64 over the driver's attribute and tokens. Formatting and comments do not
count, code it calls is not covered, and another compiler version can print tokens differently.
`cargo hydo save` writes regressions to `hydo/regressions/` of the crate; `cargo hydo plan`
mirrors that folder into `.hydo/regressions/`, where `hydo build` reads it next to the plan.
`System::regression(name, schedule, expect)` still takes a schedule string; use it for schedules
in the resolved form (`W=:...`), and prefer files for anything `hydo save` wrote.

The assertion SDK (`sdk/rust`, crate `hydo-assert`) is re-exported as `hydo_test::assert`, so
a driver or check can also state properties as assertions (they go straight to the console).

Not yet: `System::from_compose` (the Docker Compose importer is another branch), fakes, log
rules and known errors (PRODUCT.md "Code first").
