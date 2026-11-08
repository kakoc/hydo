# hydo-agents: the commands for AI coding tools

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- `hydo guide`: hydo's facts for writing a project's `hydo/` folder, built into the program.
- `hydo skill install`: the onboarding skill, copied where an AI coding tool reads skills.
- `hydo init`: a repository's first `hydo/` folder and its section in `AGENTS.md`.

## License

Apache-2.0.
