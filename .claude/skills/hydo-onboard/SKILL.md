---
name: hydo-onboard
description: Analyze a software project and write its hydo.yaml manifest so hydo (the deterministic hypervisor in this repo) can test it under faults. Use when the user asks to onboard, add, or test a project, repository, service or docker-compose stack with hydo, or asks for a hydo.yaml / manifest for something.
---

# Onboard a project into hydo

The goal is ONE file, `hydo.yaml`, that `hvx/onboard/hydo-build` accepts. hydo then boots the
project in a deterministic guest, runs a workload under faults (kills, pauses, network loss,
power cuts, clock jumps, disk faults) and checks the invariants. The schema is
`hvx/onboard/MANIFEST.md`; it is authoritative, so read it first, every time. A worked example generated with this skill for an external project (miniflux + PostgreSQL)
is `example-miniflux.yaml` in this directory. Two complete
examples: `hvx/onboard/examples/redis/hydo.yaml` (one service) and
`hvx/onboard/examples/stack/hydo.yaml` (four services).

Work in this order. Write the manifest to `hvx/onboard/projects/<name>/hydo.yaml` unless the
user names another place, with any helper scripts next to it.

**If the project has a `docker-compose.yaml`, start from it:** run
`hvx/onboard/hydo-compose <docker-compose.yaml> --out hvx/onboard/projects/<name>/hydo.yaml`
(or `hydo compose ...` from a release). It writes the services (images pinned by digest,
commands, env, readiness from the healthchecks, ports, volumes on the data disk, each service on
its own loopback `addr:` with the Compose names in `/etc/hosts`), a `fetch-images.sh` that copies
the images by digest, and a lock file; it prints warnings for what it could not map. Then do
steps 1 and 4-9 below on top of it: read every warning (port collisions and bind mounts the
program writes to need a hand fix), replace the `TODO` workload and invariant, check
`guest.ram_mib` / `guest.cpu`, and keep the generated comments that say where each value came
from. MANIFEST.md, "From Docker Compose", is the reference for the mapping;
`hvx/onboard/examples/compose-voting/` is a worked example (`hydo.generated.yaml` = the raw
import, `hydo.yaml` = after the hand edits).

## 1. Inventory how the project runs

Read, in roughly this order, and note what each says about processes, ports, data and
configuration:

- `docker-compose*.yml`, `compose*.yaml`, `.devcontainer/` — the service list, images,
  ports, volumes (a volume is a candidate data directory), `depends_on`, `healthcheck`
  (a ready check), environment.
- `Dockerfile*` — the binary, how it is built, `CMD`/`ENTRYPOINT`, `EXPOSE`, `HEALTHCHECK`.
- `Procfile`, systemd units (`*.service`), init scripts, Helm charts, Ansible roles.
- `Makefile`, `package.json` scripts, `go.mod`/`Cargo.toml`/`pom.xml` — how to build a binary.
- README / docs "run" or "self-host" sections, example config files, and the config parser
  (which settings exist, which are durability or timeout settings).
- Database migrations and schema files — the tables, keys and constraints invariants can use.
- Existing integration, load or end-to-end tests — candidate workloads.

Write a short inventory for the user: each process, its binary, its port, its data
directory, what it depends on, and where you found each fact.

## 2. Decide where every binary comes from

Each binary must be one of:

- A Docker/OCI image (simplest when the project ships one or uses an official image): a
  service gets `image:` (a `docker save` tar or an OCI layout directory on the build box,
  fetched once with `skopeo copy docker://<image>@sha256:<digest> ...` and pinned by digest)
  and runs chrooted in `/oci/<service>` with the image's entrypoint/cmd/env/workdir/user.
  Point its data directory at `/data`, and run its tools from `ready:`/workload/invariants
  with `hydo-oci <service> <command>`. See MANIFEST.md, "Services from a Docker/OCI image",
  and `hvx/onboard/examples/redis-oci/`.
- A nix store path (preferred for third-party servers): find one with
  `nix build nixpkgs#<pkg> --print-out-paths` on the build machine and pin the
  exact path under `root.nix`; the whole closure is copied, so it runs unchanged.
- A static binary built from the project's source on the build box (Go:
  `CGO_ENABLED=0 go build`; C: the pinned static musl gcc, see `build-stack.sh`), listed
  under `root.files`.
- A prebuilt file the repo already has in `~/hydo/build`.

Never fetch anything at guest run time: the guest has no network beyond loopback.

## 3. Map each process to a service

For every long-running process write one `services:` entry:

- `name`: lowercase. If it plays one of the roles the explorer already generates faults for,
  reuse that name (`mysql`, `kafka`, `redis`, `front`); other names only get faults from
  hand-written schedules (MANIFEST.md, "Limits of v1").
- `cmd`: foreground, no daemonizing (`daemonize no`, `--foreground`, no `&`). Bind to
  127.0.0.1.
- `setup`: one-time initialization (database init, migrations' first run, storage format),
  written to be idempotent because it runs before every start, including after a kill and
  in the recovery boot.
- `ready`: a cheap command that exits 0 when the service answers (a CLI ping, a `SELECT 1`,
  `curl -sf …/health`, or `grep -qa "<started line>" $HYDO_LOG`). Take it from the compose
  `healthcheck` when there is one.
- `port`: the port clients use (network faults act on it).
- Data: every directory the service must not lose on a crash goes under `/data` (list it in
  `data.dirs` and point the service's config there). Everything else is on a read-only root,
  so scratch files must go to `/tmp`.
- `depends_on` from compose; `recovery: false` for services the final check does not need
  after a power cut (a stateless API tier whose checker reads the stores directly).
- `provision`: schema creation, topics, users — everything that must exist before the
  window under test. It runs once, before the S1 snapshot.

If the project has no long-running service (a library, an embedded database, a CLI), say so:
if `MANIFEST.md` documents service-less projects, use that; otherwise stop and tell the user
the schema does not support it yet.

## 4. Choose the workload

The workload is a sequence of numbered steps; faults are placed between steps. One step
should be one client operation the system acknowledges (an order, a SET, an insert, a
request).

- Prefer a small deterministic client you write (shell with the project's own CLI, or a short
  program): step `i` does a fixed operation derived from `i`, no random data, no wall-clock
  values in payloads, a client-side deadline on every call (`timeout 2 …`).
- For each step it must record what the client saw: append `i` to `$HYDO_ACKED` on success,
  and `i ack|failed|unknown` to `$HYDO_OUTCOMES`. Use `failed` only when the system says
  for certain that nothing was applied; a timeout or a lost connection is `unknown`.
- `retry`: if the system supports idempotent retries (an idempotency key, upserts by id),
  add a retry command that re-sends every step not in `$HYDO_ACKED` with the same id.
- An existing integration test is usable only if it is deterministic and can run a step
  range (`$HYDO_FROM`..`$HYDO_TO`).
- 20–50 steps is a good size. Set `workload.timeout`.

### If the system's inputs are gRPC endpoints

Do not write a client per RPC. Generate the drivers (`MANIFEST.md`, "gRPC services"):

1. Find the `.proto` files (or a compiled descriptor set; or, if the server enables gRPC
   server reflection, its address). If the service has no `.proto` (a hand-written
   `grpc.ServiceDesc`), write one that matches it exactly: package, service and method names,
   request and response types.
2. Write a hints file from the API's documentation and validation code: `target`; a
   `deadline_ms` like the real clients'; per RPC a `weight`; ranges, choices and lengths for
   fields the server validates; `unique: true` for idempotency keys and ids the client
   chooses; `from: <Rpc>.<field>` for ids an earlier response returns (Get/Cancel/Delete of a
   created entity); `retry` with `attempts` for RPCs real clients retry, and `outcome_id` for
   the field that identifies an operation in `$HYDO_ACKED`.
3. Run `hvx/onboard/hydo-grpc-gen <proto dir> --hints hints.yaml --out <dir next to
   hydo.yaml>` and merge `<dir>/hydo.grpc.yaml` into the manifest: files, `env:`, a `test:`
   section with one `parallel_driver` per RPC (the test composer) and `workload.retry`; or copy
   `<dir>/workload/` into `hydo/workload/`. Use the commented numbered-steps `workload:` only
   when the invariants need one operation per step.
   Check the plan with `hydo-grpc gen <rpc> 5` (requests look valid?) and `hydo-grpc list`.
4. Invariants stay hand-written; use `hydo-grpc call <rpc> '<json>'` (exit 0 / 1 / 2) for
   read-backs, and `$HYDO_ACKED` / `$HYDO_OUTCOMES` for what the client was told. Example:
   `hvx/grpcgen/demo/hydo.yaml`.

## 5. Propose invariants

Read `invariant-patterns.md` in this directory for check-command templates. Derive each
invariant from something the system promises (its docs, its API contract, its schema
constraints), not from what the code happens to do. Typical ones:

- Every acknowledged write is durable and reads back with its value.
- Every acknowledged operation is applied exactly once (no duplicates after retries).
- An operation reported as definitely failed was not applied.
- Domain rules: balances never negative, totals conserved, referential integrity holds,
  no orphan rows, unique keys unique.
- Monotonic reads or versions: a value or revision a client saw never goes backwards.
- A cache never holds a value the store does not (a missing cache entry is fine).

For each: an `id`, a one-line `msg`, the `cmd` (exit 0 holds, 1 violated, 2 cannot be
evaluated now — e.g. the service is down), and whether it is `final` (checked after
recovery), periodic (`every: <s>`, for properties that could break and heal during the run,
such as stale reads), or both. Use `type: always_or_unreachable` for periodic checks that may
not get to run while a service is killed. A periodic check only runs if the workload lasts
longer than its period: miniflux's 30 creates take about 0.3 s of guest time, so its
`every: 2` check ran only in runs where a fault slowed the workload. Make the workload long
enough (more steps, or a short sleep per step), or rely on `final` checks.

Then tell the user plainly: **the invariants are the one part that needs human judgment;
please review each one**, and say which you are unsure about and why.

## 6. Durability knobs

List settings that trade durability for speed (fsync policies, commit modes, flush intervals,
replication acks). Each becomes a `knobs:` entry with the safe value as `default` and a `set`
command that changes it at run time; if a setting needs a restart, say so instead of adding
it. Use letters the explorer already knows (`M`, `R`) only for the matching stores.

## 7. Determinism hazards

Check `hazards.md` in this directory and report each hazard you found, with the fix you
applied or the question you could not answer. The ones hydo-init already handles: the CRNG
seed, loopback, the start date of the clock, service ordering, restarts, the explorer
protocol.

## 8. Validate

- Run `python3 hvx/onboard/hydo-build <hydo.yaml> --check` (works on any machine) and fix
  every error until it prints `OK`.
- If the user wants an image now: run the full build on the build box (`HYDO_SEEDRNG=…
  hvx/onboard/hydo-build <hydo.yaml>`, see MANIFEST.md "Building and running"), copy the
  disk image to the bare-metal test machine, and run one schedule without faults
  (`disk=none dseed=1 | run`) through `~/bin/hvx-run`. It must print `HYDO-ORACLE: OK`;
  run it twice and compare fingerprints. Never insmod or rmmod on the build box.
- If `MANIFEST.md` documents a `params:` build parameter (e.g. to build a buggy and a fixed
  version from one file), use it for version choices instead of two manifests.

## 9. Report

Give the user: the manifest path; the inventory; the invariants for review; the knobs; the
hazards and what you did about them; what you could NOT determine (unknown ready checks,
unclear data directories, missing binaries, ambiguous promises), each with what happens if
it is left as is; and the exact build and first-run commands.
