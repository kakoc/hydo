# hydo-build: the image builder

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- `hydo build`: a project file compiled into a guest image, an ext4 disk and an initramfs on
  x86_64, an initramfs and a data disk on arm64.
- Services from Docker or OCI images, the guest agent and hydo's guest programs staged into the
  image.
- Reproducible output: the same project file gives the same image bytes on any machine.

## License

Apache-2.0.
