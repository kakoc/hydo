# hydo-kvm: the KVM run engine (x86_64 Linux without a kernel module)

This crate is part of [hydo](https://github.com/kakoc/hydo). The hydo program runs a whole
system (its services, a workload, and checks) in a deterministic virtual machine. It injects
faults such as process kills, network faults, and power cuts, and it replays every failure bit
for bit. The crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- Runs hydo's x86_64 images deterministically on stock Linux KVM (`/dev/kvm`), so hydo runs
  where its own kernel module cannot be loaded, for example in cloud virtual machines and on AMD
  machines.
- A clock that counts virtual-machine exits, the engine's own interrupt controller and the Mac
  engine's devices and protocol; one virtual CPU.
- It runs inside the `hydo` process by default, or as the program `hydo-kvm`.

## License

Apache-2.0.
