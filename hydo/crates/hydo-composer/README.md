# hydo-composer: the test composer, host side

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- The *test composer* runs a project's workload commands in rounds that a seed chooses, and the
  explorer can shrink a failing composition to the few commands that matter.
- The forms of the schedule token `W=` (a seed, the resolved list of commands, a saved
  composition) and the shrinker's candidates.
- Saved regressions (`hydo save`, `hydo repro --regressions`) and the files a plan puts into an
  image.

## License

Apache-2.0.
