# hydo-control: the host controller of a run

This crate is part of [hydo](https://github.com/kakoc/hydo). The hydo program runs a whole
system (its services, a workload, and checks) in a deterministic virtual machine. It injects
faults such as process kills, network faults, and power cuts, and it replays every failure bit
for bit. The crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- The controller decides everything a run does: it starts the services, runs the workload,
  applies the schedule's faults and runs the checks, through the commands of the guest agent.
- The `Controller` trait and `ScheduleController`, which runs a schedule.
- The control transcript of every run, and its replay (`hydo repro --transcript`).

## License

Apache-2.0.
