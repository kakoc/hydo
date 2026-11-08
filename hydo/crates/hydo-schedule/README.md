# hydo-schedule: the schedule language

This crate is part of [hydo](https://github.com/kakoc/hydo). The hydo program runs a whole
system (its services, a workload, and checks) in a deterministic virtual machine. It injects
faults such as process kills, network faults, and power cuts, and it replays every failure bit
for bit. The crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- A *schedule* is one line that describes one run: the host part (for example the disk's fault
  mode and seed) before `|`, and the faults inside the guest after `run`, for example `disk=cut dseed=1 | run C=12 R=everysec`.
- One Rust type for every token of the language.
- The parser and the printer. A schedule printed from its types is the original text, byte for
  byte, and this is the only code in hydo that reads schedule text.

## License

Apache-2.0.
