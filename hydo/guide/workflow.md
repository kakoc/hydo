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
hydo onboard
```

`hydo onboard` looks at the repository and says what it found: a Compose file and its services,
the Dockerfiles, the main language, and an existing `hydo/` folder. Then it starts `hydo/`: with
a Compose file it runs `hydo compose` (below), with Kubernetes manifests or a Helm chart `hydo
k8s` (below), and with neither `hydo init`. A repository with several Compose files, for example
one per database, gets the first of `docker-compose.yaml`, `docker-compose.yml`, `compose.yaml`
and `compose.yml`, and a list of the others; `hydo onboard --compose docker-compose-postgres.yml`
imports the one you name. It never changes an
existing `hydo/` folder. It installs the onboarding skill for the AI tools it finds (`--agent`
names them, `--no-skill` installs none) and prints the next step. `hydo onboard --help` lists its
options.

The two commands it runs also work alone:

```sh
hydo init
```

`hydo init` creates `hydo/hydo.yaml` (a skeleton with `TODO` markers), `hydo/workload/`, `hydo/checks/` and
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

**From Kubernetes manifests** (a folder of manifests, a Helm chart, or a Kustomize folder):

```sh
hydo k8s k8s/ --out hydo/hydo.yaml --images-dir images
hydo k8s charts/app --values ci-values.yaml --set auth.password=test --out hydo/hydo.yaml --images-dir images
sh hydo/fetch-images.sh
```

`hydo k8s` renders a chart with `helm template` and a Kustomize folder with `kubectl kustomize`;
it never contacts a cluster. Each container of each pod becomes a service. A StatefulSet of three
replicas becomes `<name>-0`, `<name>-1` and `<name>-2`, and a group `<name>` under `groups:`, so
`K=<name>.majority@5+2` kills two of them. Init containers become one-shot jobs that run before
the pod's services start, and Jobs run once in `provision:`. Each Service's name resolves to the
pod it selects. The readiness probes become `ready:` checks, and the claims and `emptyDir`
volumes become directories on the data disk.

The ConfigMaps a pod mounts are written to `hydo/k8s-import/configmap/`; commit them. The Secrets
go to `hydo/k8s-import/secrets/`, which holds a `.gitignore` of `*`, and their environment values
reach the services through `env_file:`, never through `hydo.yaml`. Run the import again in a fresh
checkout to write them back.

`hydo onboard` runs `hydo k8s` when it finds no Compose file but a chart, a Kustomize folder or
manifests in `chart/`, `charts/*`, `helm/`, `deploy/`, `k8s/`, `kubernetes/`, `manifests/` or a
similar folder. Read every warning. A Deployment's replicas become one pod, a Service port that
differs from its target port is reached on the target port, and the containers of a pod do not
share `localhost`. An Ingress, a NetworkPolicy, an operator's custom resources and a service
mesh's sidecars are not imported. The reference, with the mapping of every field, is
`docs/design/k8s-import.md` in hydo's repository.

## Where each program comes from

The guest has no network beyond its own services, so nothing is fetched at run time. A program
is one of:

- **a Docker/OCI image file**: `image:` on the service, a `docker save` tar or an OCI layout under
  `hydo/images/`, pinned by digest. The service runs chrooted in its own image root; commands and
  checks run its tools with `hydo-oci <service> <command>`. Schema: "Services from a Docker/OCI
  image".
- **a static binary** built from the project's source (Go: `CGO_ENABLED=0 go build`; Rust: a musl
  target, as below; C: a static musl compiler), listed under `root.files` with `src:` relative to
  `hydo.yaml`.
- **a Nix store path** under `root.nix`, only on a machine with Nix (the whole closure is copied).

**A static Rust binary on a Mac.** The guest is arm64 Linux on a Mac (x86_64 Linux on Linux), so
the target is `aarch64-unknown-linux-musl` (or `x86_64-unknown-linux-musl`). Installing a target
is a network install: `rustup target add aarch64-unknown-linux-musl`. A project with no C code
links with Rust's own linker and needs no C toolchain. Add this `.cargo/config.toml` to the
project:

```toml
[target.aarch64-unknown-linux-musl]
linker = "rust-lld"
rustflags = ["-C", "linker-flavor=ld.lld", "-C", "link-self-contained=yes", "-C", "target-feature=+crt-static"]

[target.x86_64-unknown-linux-musl]
linker = "rust-lld"
rustflags = ["-C", "linker-flavor=ld.lld", "-C", "link-self-contained=yes", "-C", "target-feature=+crt-static"]
```

Then `cargo build --release --target aarch64-unknown-linux-musl` writes a static binary to
`target/aarch64-unknown-linux-musl/release/`. `file` on it prints `statically linked`. The
recipe works only without C code. `cargo tree -i cc --target aarch64-unknown-linux-musl` lists
the crates whose build scripts compile C, such as `ring`, `openssl-sys` or `zstd-sys`. Without C
code it prints `did not match any packages` or `nothing to print`. A build script of the
project itself that calls `cc::Build` also compiles C.

A project with C code needs a C compiler for the target. Use one of these:

- `zig cc -target aarch64-linux-musl` as the linker and the `CC` of the target (with
  `cargo-zigbuild`);
- a musl cross toolchain, such as `aarch64-linux-musl-gcc`;
- a build in a Linux container: `docker run --rm -v "$PWD":/src -w /src rust:alpine cargo build
  --release` on a Mac gives an arm64 musl binary.

## Services, workload, checks

- The least a first folder needs, for a project with its own binaries and no Compose file, on one
  page: `hydo guide minimum-path`.
- Services: `hydo guide services`.
- Commands in `hydo/workload/`: `hydo guide composer` (kinds, environment, exit codes, the
  `test:` section). gRPC systems: generated drivers, `hydo guide outside-services`. Tests written
  in a programming language: `hydo guide sdks`.
- Checks in `hydo/checks/`: `hydo guide checks` and `hydo guide invariant-patterns`. Business
  rules from the project's own sources, validated on runs without faults: `hydo guide rules`.
  Error logs as findings: `hydo guide error-logs`.
- Which checks and faults found bugs in other projects of the same type, with the evidence:
  `hydo guide strategies`.
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
hydo report                                   # each problem in words, with the command that shows it
hydo report --details                         # the technical report: classes, minimal schedules
hydo report --html                            # the same as one web page to share (hydo guide inspect)
hydo save <explore dir>                       # the minimal schedules -> hydo/regressions/
```

On the Mac a schedule without host tokens is `disk=none | run`. If the first run without faults
fails, the cause is the folder or a real bug. Read the `hydo: diagnosis:` lines, and the
`diagnosis` in the run directory's `result.json`: which process ended how, its log, and the
kernel's messages. Then look inside the run with `hydo inspect` (`hydo guide inspect`) before you
change the image. The
full build also checks the backend's limits (the arm64 build refuses `guest.cpu: v3`).

## Which versions have a finding

`hydo versions` says in which versions of the system a finding is present. It replays the
finding's minimal schedule on each version and gives each version one outcome:

- `reproduced`: the same check failed with the same finding key;
- `passes`: the checks held;
- `fails-otherwise`: another check or a run error failed the run;
- `cannot-run`: the version did not build, or the schedule names something the version lacks.

Give the finding as a run directory, an exploration directory (each of its findings), or a
schedule. Give the versions oldest first, in one of three forms:

```sh
hydo versions hydo-out/explore --param version=0.16.28,0.16.29,0.17.9   # hydo build --set version=<v>
hydo versions hydo-out/runs/run-1 --git v1.4.0..v1.6.0 --build 'sh build.sh'  # a checkout of each commit
hydo versions "disk=none | run K=db@5+1" --images old=a-disk.img,new=b-disk.img
```

The search replays the newest version and the oldest first. Then it bisects for the first
version that reproduces and the last that passes. It assumes one change point and says when the
replays contradict it. `--all` replays every version. One replay per version is enough, because
hydo replays exactly: a verdict is exact for one engine and image. A flaky test's verdict is
exact too, but another engine, vCPU count or turn length can give another verdict.

- `--build CMD` runs before each build, with `HYDO_VERSION` set to the version. A version whose
  command fails is `cannot-run`, and the search steps over it.
- A run directory's finding is its verdict's key. When the checks held, it is the first new
  error kind. `--key f1:error-log:<10 hex>` names another; `ci-run.json` lists the keys.
- An image cache keeps each version's image by its inputs, so a second search builds nothing new.
- The answer is `versions.json` next to the replays, and `hydo report <dir>` prints it with the
  report of the run or exploration.

## Performance tests

A run under hydo is deterministic, so the work that a workload does is the same number in every
run of one image. A performance test compares that number between two builds of the system. A
change of 1 % then shows in every comparison. It needs no repeats, and the host adds no noise.

Add `perf:` to the manifest:

```yaml
perf:
  steps: true        # each workload step is its own workload call and its own measure
  threshold: 0.5     # hydo perf compare: a smaller change in percent reads as no change
```

Each run's `result.json` then has `perf`, with these measures:

- `workload` and `step <i>`: the work of the whole workload and of each step, counted by the
  engine. On hvx the unit is `branches`, the guest's retired conditional branches. The CPU counts
  them exactly. hvx also gives `guest_ns`, the guest clock's time.
- On KVM and the Mac engine the unit is `exits`, the VM exits. An exit count does not include CPU
  work between two exits, so these engines do not show a slower request handler.
- The workload's own numbers: each line `<name> <value> [<unit>]` that it writes to the file
  `$HYDO_PERF`.

Build the image of each commit, run the same schedule on each, and compare the two runs:

```sh
hydo run base-disk.img "disk=none | run" --out runs/base
hydo run head-disk.img "disk=none | run" --out runs/head
hydo perf compare runs/base runs/head
```

On hvx, with a planted slowdown of 1 % of the server's work per request
(`hydo/onboard/examples/perf-kv`), the comparison printed this (steps 2 to 9 left out):

```text
hydo perf: run B against run A on hvx, counted in the guest's retired branches, counted exactly by the CPU; a change under 0.5% reads as no change
  the whole workload did 0.88% more work (slower): 228,035,550 -> 230,036,509 branches
  step 0 did 0.88% more work (slower): 22,857,114 -> 23,058,986 branches
  step 1 did 0.82% more work (slower): 22,819,331 -> 23,006,695 branches
  no change in 0 of 11 measures (within 0.5%)
  (11 measures of guest time are not shown: guest time does not follow the work; --all shows them)
```

Two runs of one image give the same measures, so their comparison prints `no change: all 11
measures are exactly the same (0.00%)`.

The exit status is 1 when a measure of the engine grew by more than the threshold, so a CI job
fails on it. The comparison leaves out hvx's guest time; `--all` shows it, and every measure
that did not change. `hydo perf show <run>` prints one run's measures. Measure only what hydo
counts exactly:

- Use the engine's count of a step, not its guest time. On hvx the guest clock includes the time
  the guest waits for a timer or the disk. A 5 % slowdown of the server's work moved the guest
  time by 0.25 %.
- Do not report latencies per request in guest time. The guest clock moves in steps of about
  1 ms on hvx, so the percentiles of the example's requests are the same in every build.
- Keep the work of a step on the CPU. A slower disk or network does not change the count.
- Compare runs of one engine and one schedule. On KVM, also compare runs of one host CPU model.
