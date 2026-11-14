# hydo-contracts: function contracts in comments, checked at every call

This crate is part of [hydo](https://github.com/kakoc/hydo). The hydo program runs a whole
system (its services, a workload, and checks) in a deterministic virtual machine. It injects
faults such as process kills, network faults, and power cuts, and it replays every failure bit
for bit. The crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- The comment language: `// hydo: requires ...`, `// hydo: ensures ...` with `old()` and
  `result`, and `// hydo: input <name> in <lo>..<hi>`, right before a function.
- The compiler of the plan: with a program's debug information (DWARF), each contracted
  function's entry address and the reads an engine makes at its entry and at its return.
- The plan an engine follows (`<image>.contracts.json`) and the records it writes, one per call.
- The judgement of a run's records: each broken clause with the first broken call's values.

## License

Apache-2.0.
