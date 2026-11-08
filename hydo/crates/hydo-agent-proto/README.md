# hydo-agent-proto: the protocol between hydo and its guest agent

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- The *guest agent* (`hydo-agent`) is the first process inside every hydo guest. It only carries
  out commands; hydo, on the host, decides what to do.
- The commands, replies and events of the protocol (version 1), and its one encoding: canonical
  JSON with integers only, written without dependencies, so that the agent compiles this crate
  into itself.
- The framing of the agent's lines on the guest console, the host side of a conversation with
  its version rule, the generated JSON Schema and the format of example transcripts.

## License

Apache-2.0.
