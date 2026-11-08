# hydo-explore: the explorer

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- Schedules generated from seeds by several strategies, and exhaustive sweeps over one kind of
  fault.
- A pool of engine workers that runs them, runs every failure again to confirm that it replays
  bit for bit, and shrinks it to a minimal failing schedule.
- The exploration's results (`events.jsonl`, `exploration.json`) and its report (`report.md`,
  `hydo report`).

## License

Apache-2.0.
