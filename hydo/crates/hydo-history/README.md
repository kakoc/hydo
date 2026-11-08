# hydo-history: the history checkers

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- An *operation history* is the list of operations a workload's clients performed, each with its
  invocation and its outcome, recorded by the assertion SDKs as `HYDO-HIST` lines.
- A linearizability checker per key for registers and compare-and-set objects, and a checker of
  list-append transactions for a chosen consistency level.
- The combined verdict with a witness (the operations that show a violation), and `hydo check-history`.

## License

Apache-2.0.
