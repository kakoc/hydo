# Onboarding with this hydo: the commands and the files

This page is part of the guide built into the `hydo` program, so it always matches the hydo you
run. The onboarding skill (`hydo guide skill`) says how to work; this page says what this hydo
offers for each step.

## The folder

```
hydo/
  hydo.yaml        the system: files, services, data disk, guest settings, knobs, allowed faults
  workload/        commands, one executable per operation: parallel_driver_<name>, first_<name>, ...
  checks/          properties: final_<name>, always_<name>, eventually_<name>
  regressions/     saved minimal schedules (<name>.schedule), replayed by `hydo repro --regressions`
.hydo/             build cache (git-ignored)
```

hydo composes the commands into seeded sequences and concurrent mixes (the test composer), places
faults between and inside them, and runs the checks. The older "numbered steps" workload
(`workload.run`) is still supported for the cases in `hydo guide numbered-steps`.

## The format

- `hydo guide schema` prints the project format's JSON Schema; it is authoritative, so read the
  sections you use before you write them. `hydo guide manifest` explains every key.
- Write `format: 1` in every `hydo.yaml`. Within format 1 hydo only adds keys; an unknown key is
  an error, so a typo fails the check. `hydo migrate` rewrites an older file in the current
  spelling; a newer format is refused with the format it needs.
- `hydo build hydo/hydo.yaml --check` validates the schema and the guest RAM limit. For every
  command and check file it validates the name, the executable bit, and the header. Run it after every change.
- Without the `hydo` command, inside hydo's own source repository, the same facts are in
  `hydo/guide/`, `hydo/onboard/MANIFEST.md` and `hydo/schema/project-v1.json`, and
  `cargo build --release -p hydo-build && target/release/hydo-build <manifest> --check` replaces
  `hydo build <manifest> --check`.

## Start the folder

```sh
hydo init
```

It creates `hydo/hydo.yaml` (a skeleton with `TODO` markers), `hydo/workload/`, `hydo/checks/` and
`hydo/regressions/`, adds `.hydo/`, `hydo/images/` and `hydo-out/` to `.gitignore`, and adds a short hydo
section to `AGENTS.md` between marker comments. It never overwrites a file, so it is safe to run
again. The skeleton's two placeholder files (`workload/parallel_driver_todo`, `checks/final_todo`)
make every run fail on purpose until they are replaced.

**From a Docker Compose file** (it replaces the skeleton's `hydo.yaml`):

```sh
hydo compose docker-compose.yaml --out hydo/hydo.yaml --images-dir images
sh hydo/fetch-images.sh        # copies every image by digest into hydo/images/ (network, once)
```

The importer writes the services: images pinned by digest, commands, environment, readiness from
the healthchecks, ports, volumes on the data disk, and restart policies. It sets
`network: bridge`, so each service has its own network namespace at its own address. It prints a
warning for everything it could not map. Read each one; bind mounts that the program writes to
need a fix by hand. It also
writes a numbered-steps `workload.run` and a `TODO` invariant: delete both (keep `workload.steps`
and `workload.timeout`), and write the test as commands in `hydo/workload/` and `hydo/checks/`
(or as a `test:` section in the file). Keep the generated comments that
say where each value came from. The reference is `hydo guide manifest`, section `From Docker
Compose`.

**A service built locally** (`build:`) is never pulled. When a Docker command answers (`docker`, else
`podman`; `--docker <cli>` names another), `hydo compose` builds the image for this machine's
guest: linux/arm64 on a Mac, linux/amd64 on Linux. It passes Compose's `context`, `dockerfile`,
`args`, `target` and `labels`, and saves the image where `hydo.yaml` names it. It reads the
image's healthcheck, ports and volumes from the file, and records the file's digest as the pin in
`hydo-compose.lock.json`.

- Without Docker, `hydo compose` names what to install. Later, `sh hydo/fetch-images.sh` builds
  the image and records the pin. Run `hydo compose` again then, so that `hydo.yaml` takes the
  image's settings.
- fetch-images.sh builds every such image whose file is missing, for example in a fresh checkout.
  When the new build has another digest than the pin, it stops and keeps the build as
  `<file>.rebuilt`. `HYDO_ACCEPT_REBUILD=1 sh hydo/fetch-images.sh` takes it and records the new
  pin.
- Expect that stop on another machine: a Docker build rarely gives the same bytes twice. Docker
  28.4 writes a memory address into the history of an `EXPOSE` line, so each build differs.
- `hydo compose --no-build` leaves every build to fetch-images.sh.

## Where each program comes from

The guest has no network beyond its own services, so nothing is fetched at run time. A program
is one of:

- **a Docker/OCI image file**: `image:` on the service, a `docker save` tar or an OCI layout under
  `hydo/images/`, pinned by digest. The service runs chrooted in its own image root; commands and
  checks run its tools with `hydo-oci <service> <command>`. Schema: "Services from a Docker/OCI
  image".
- **a static binary** built from the project's source (Go: `CGO_ENABLED=0 go build`; Rust: a musl
  target; C: a static musl compiler), listed under `root.files` with `src:` relative to
  `hydo.yaml`.
- **a Nix store path** under `root.nix`, only on a machine with Nix (the whole closure is copied).

## Services, workload, checks

- Services: `hydo guide services`.
- Commands in `hydo/workload/`: `hydo guide composer` (kinds, environment, exit codes, the
  `test:` section). gRPC systems: generated drivers, `hydo guide outside-services`. Tests written
  in a programming language: `hydo guide sdks`.
- Checks in `hydo/checks/`: `hydo guide checks` and `hydo guide invariant-patterns`. Error logs as
  findings: `hydo guide error-logs`.
- Outside services (fakes and proxies): `hydo guide outside-services`. Fault kinds, knobs and
  BUGGIFY: `hydo guide faults`. The guest environment and its limits: `hydo guide hazards`. What
  this machine's backend supports: `hydo guide capabilities` and `hydo doctor`.
- Complete examples: `hydo guide examples/example-redis-composed/README.md` (a `hydo/` folder with
  commands and checks) and `hydo guide examples/example-miniflux.yaml` (two services, numbered
  steps).

## Build and run

```sh
hydo build hydo/hydo.yaml --check             # until it prints OK
hydo build hydo/hydo.yaml                     # the guest image (x86_64 on Linux, arm64 on a Mac)
hydo run "disk=none dseed=1 | run"            # one run without faults: must end HYDO-ORACLE: OK
hydo run "disk=none dseed=1 | run"            # again: the fingerprint (fp=) must be identical
hydo explore --seeds 1-20 --strategy process  # schedules with faults; failures re-run and shrunk
hydo report                                   # the findings, each with a minimal schedule
hydo save <explore dir>                       # the minimal schedules -> hydo/regressions/
```

On the Mac a schedule without host tokens is `disk=none | run`. If the first run without faults
fails, the cause is the folder or a real bug. Read the `hydo: diagnosis:` lines, and the
`diagnosis` in the run directory's `result.json`: which process ended how, its log, and the
kernel's messages. Then look inside the run with `hydo inspect` (`hydo guide inspect`) before you
change the image. The
full build also checks the backend's limits (the arm64 build refuses `guest.cpu: v3`).
