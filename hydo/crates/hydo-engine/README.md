# hydo-engine: one interface over hydo's run engines

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- The traits every caller uses (`Engine`, `Worker`) and the ones the engines implement
  (`ControlledEngine`, `ControlledWorker`).
- Starting an engine inside the process or as a program, worker pools sized from the machine,
  snapshots kept in memory, and the fingerprints of a run.
- The lock-step link: an engine stops at numbered points so that the host can send the guest its
  commands at exact moments.

## License

Apache-2.0.
