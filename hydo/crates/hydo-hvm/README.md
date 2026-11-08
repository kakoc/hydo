# hydo-hvm: the run engine for Apple Silicon Macs

This crate is part of [hydo](https://github.com/kakoc/hydo). The hydo program runs a whole
system (its services, a workload, and checks) in a deterministic virtual machine. It injects
faults such as process kills, network faults, and power cuts, and it replays every failure bit
for bit. The crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- A deterministic arm64 virtual machine on Hypervisor.framework: a clock that counts
  virtual-machine exits, an interrupt controller, a serial console, and virtio disk and network
  devices with seeded faults.
- The *spin observer*, which makes busy-wait loops without exits deterministic.
- It runs as the program `hydo-hvm`, which needs the hypervisor entitlement, or inside the
  calling process.

## License

Apache-2.0.
