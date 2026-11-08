# hydo-compose: the Docker Compose importer

This crate is part of [hydo](https://github.com/kakoc/hydo). hydo runs a whole system (its
services, a workload and checks) in a deterministic virtual machine, injects faults such as
process kills, network faults and power cuts, and replays every failure bit for bit. The
crate is published on crates.io only because the `hydo` command is built from it.

**This crate has no stable API.** Its items can change in any hydo release, so do not depend
on it from your own code. To use hydo, install the command with `cargo install hydo` (see
<https://crates.io/crates/hydo>) and read its built-in guide with `hydo guide`.

## What it contains

- `hydo compose`, which turns a `docker-compose.yaml` into a starting `hydo.yaml`: services,
  health checks as readiness checks, volumes, restart policies and addresses.
- Service images pinned by digest, read from the registry directly.
- The program `hydo-fetch-image`, which downloads a pinned image.

## License

Apache-2.0.
