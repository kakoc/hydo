# hydo-engine-proto: the calls between hydo and its run engines

This crate is part of [hydo](https://github.com/kakoc/hydo). The hydo program runs a whole
system (its services, a workload, and checks) in a deterministic virtual machine. It injects
faults such as process kills, network faults, and power cuts, and it replays every failure bit
for bit. The crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- A *run engine* is the part of hydo that runs a guest virtual machine: hvx on x86_64 Linux, hvm
  on an Apple Silicon Mac, and the KVM engine.
- The requests, host commands, engine messages, and results as Rust types. Inside one process
  they travel as values.
- Their one text form, the line protocol that hydo uses with an engine running as its own
  program.

## License

Apache-2.0.
