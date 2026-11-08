# hydo-build: the image builder

This crate is part of [hydo](https://github.com/kakoc/hydo). The hydo program runs a whole
system (its services, a workload, and checks) in a deterministic virtual machine. It injects
faults such as process kills, network faults, and power cuts, and it replays every failure bit
for bit. The crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- `hydo build`: a project file compiled into a guest image. On x86_64 the image is an ext4 disk
  with an initramfs; on arm64 it is an initramfs with a data disk.
- Services from Docker or OCI images, the guest agent, and hydo's guest programs, staged into the
  image.
- Reproducible output: the same project file gives the same image bytes on any machine.

## License

Apache-2.0.
