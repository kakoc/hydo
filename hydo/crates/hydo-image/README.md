# hydo-image: the records that tie a guest image to what it needs

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- The image record `/etc/hydo/image.json` inside every image: the guest kernel by sha256 and the
  *fingerprint epoch* (fingerprints of runs of one epoch compare exactly).
- The build record next to an image file: the hydo version and commit that built it, the project
  file's sha256 and the digests of the service images.
- The guest kernels this hydo knows, how a run finds the kernel an image names, the content
  store keyed by sha256 and the index of published releases.

## License

Apache-2.0.
