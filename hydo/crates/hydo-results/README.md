# hydo-results: the results format

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- The types of `result.json` (one run), `exploration.json` (one exploration) and `events.jsonl`
  (an exploration's records, written while it runs).
- One shape for every finding: a failed invariant, a failed assertion, a history check, an
  error-log record.
- A lenient reader (it skips fields and records it does not know) and the generated JSON Schema.

## License

Apache-2.0.
