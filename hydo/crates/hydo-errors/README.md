# hydo-errors: error logs as findings

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- Error kinds and their signatures, read from the error-log records of a run's consoles: an
  error line of a service with its times, numbers and ids blanked out.
- The list of known, accepted kinds, `hydo/known-errors.yaml`.
- `hydo errors list`, `hydo errors accept` and `hydo errors check`.

## License

Apache-2.0.
