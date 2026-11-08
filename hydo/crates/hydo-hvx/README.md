# hydo-hvx: the hvx run engine (x86_64 Linux)

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- The engine that drives `/dev/hvx`, the device of hydo's own VT-x kernel module, through its
  ioctls: boot an image to its two snapshot points, run schedules from a snapshot, power cuts
  with a recovery boot.
- It runs on a thread of the `hydo` process when that process can open the device, or as the
  program `hydo-hvx` otherwise.
- Most of the crate is compiled only on Linux.

## License

Apache-2.0.
