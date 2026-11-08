# hydo-manifest: the project format

This crate is part of [hydo](https://github.com/kakoc/hydo). The hydo program runs a whole
system (its services, a workload, and checks) in a deterministic virtual machine. It injects
faults such as process kills, network faults, and power cuts, and it replays every failure bit
for bit. The crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- The types of a project's description: `hydo.yaml` (or a JSON plan), the plan's `test:` section
  and the `hydo/` folder of a repository.
- Reading and validating a project, with the messages `hydo build --check` prints.
- The JSON Schema generated from the types, and `hydo migrate`, which rewrites an old project
  file in the current format.

## License

Apache-2.0.
