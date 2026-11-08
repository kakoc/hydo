# hydo-config: every environment variable hydo reads, in one place

This crate is part of [hydo](https://github.com/kakoc/hydo). The hydo program runs a whole
system (its services, a workload, and checks) in a deterministic virtual machine. It injects
faults such as process kills, network faults, and power cuts, and it replays every failure bit
for bit. The crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- The inventory of every `HYDO_*`, `HVX_*` and `HVM_*` variable that hydo's crates use, each
  with its owner, kind, type, default, and documentation.
- `Env::process()`, which reads the environment once, when a program starts.
- Typed groups of settings (the engines, runs, image builds, tools) that the program passes to
  the other crates, so that no library reads the environment itself.

## License

Apache-2.0.
