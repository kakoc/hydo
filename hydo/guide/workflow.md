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

**An image in a private registry** gets the credential that `docker login` saved. `hydo
compose` and `hydo-fetch-image` (which fetch-images.sh runs) look for it where Docker looks:

1. The file `$DOCKER_CONFIG/config.json`, by default `~/.docker/config.json`.
2. A `credHelpers` entry for the registry: the helper program `docker-credential-<name>`, for
   example the helper of a cloud registry.
3. Else `credsStore`: the helper for every registry, for example the system keychain's.
4. Else an `auths` entry: a saved user and password, or an identity token.

hydo asks a helper only for the registry that refused, and sends the credential only to that
registry and to the token service it names. It never prints or writes a secret. When the
registry still answers HTTP 401, the message names the registry and where hydo looked. Run
`docker login <registry>` then, and run the command again.

**A registry that hydo cannot log in to.** `hydo compose --from-docker` takes such an image from
the local Docker. Pull it there first (`docker pull <image>`):

```sh
hydo compose docker-compose.yaml --out hydo/hydo.yaml --images-dir images --from-docker
```

- For each image whose registry refuses or does not answer, `docker save --platform` writes the
  file for this machine's guest. That is linux/arm64 on a Mac and linux/amd64 on Linux.
- `hydo-compose.lock.json` records the file's digest as the pin, with `from_docker: true`.
- A later `hydo compose` reads the pin and needs neither the registry nor Docker.
- `sh hydo/fetch-images.sh` saves the file again where it is missing, for example in a fresh
  checkout. When the local Docker holds another image under that name, the script stops at the
  pin. `HYDO_ACCEPT_REBUILD=1` takes the new file and records the new pin.

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

A saved regression that needs other build parameters names them on a line of its file, for
example `# params: mode=master`. `hydo save` writes the line when the exploration's image was
built with values other than the defaults of `params:` (`hydo build --set`). It gives a finding
whose filename is taken by another finding the next free name, `<name>-2.schedule`, and keeps a
finding that it saved before. `hydo build` warns when its own `--set` values differ and names
the build that makes that regression's image. `hydo repro --regressions` replays the regression
only on an image built with those values, and on another image prints `SKIP` with the build
command.

On the Mac a schedule without host tokens is `disk=none | run`. If the first run without faults
fails, the cause is the folder or a real bug. Read the `hydo: diagnosis:` lines, and the
`diagnosis` in the run directory's `result.json`: which process ended how, its log, and the
kernel's messages. Then look inside the run with `hydo inspect` (`hydo guide inspect`) before you
change the image. The
full build also checks the backend's limits (the arm64 build refuses `guest.cpu: v3`).

The exit status of `hydo explore` tells a script what happened:

| Status | Meaning |
|---|---|
| 0 | The exploration ran. Its failures, if any, are in the report. |
| 1 | The explorer stopped after it had run schedules, for example because an engine failed. |
| 2 | An option is wrong. The line before the status names it. |
| 5 | The exploration refused before it ran any schedule, and it wrote no report. |

Refusals include a guest that does not fit into this machine's memory, a missing image, and
options that do not go together. An option that the engine or the image cannot use is refused
too. Some options draw faults that no other option draws. Before its first run the explorer
draws the first 64 schedules of each such option, with only that option on. It asks the
engine's own rule about each schedule, and one the engine would refuse refuses the option. Two
examples are `--timing` with `--vcpus 2` on KVM, whose tick jitter is one vCPU's, and
`--block-faults` on hvx. An option with nothing to act
on is refused as well: `--storage-faults` without a service with `storage:` or a `faults:` with
`corrupt` or `powerloss`, and `--block-faults` on an image without a data disk. So is an option
whose first 64 schedules are the same without it, because the manifest's `faults:` or the image
leaves nothing of what it draws. Examples are `--cluster-faults` without `groups:` and `--upgrades`
without `versions:`. One such option only gets a warning, and the exploration goes on:
`--new-faults` with `--strategy process` or `swarm-pos`, which draw pauses, clock steps and
one-way losses themselves. Each refusal is
one line that names the option, the reason, and what to use instead. When several options are
refused this way, each gets its line. With `--json`, or when stdout is not a terminal, a refusal also
prints one JSON line, `{"refused": "<reason>", "status": 5}`. A refusal for memory names each
running `hydo inspect` session with its memory and its age, because each session holds a whole
guest. `hydo inspect stop --all` stops them. Inside `hydo ci run` every stop of the explorer is
that command's status 2.

`hydo run` exits with status 0 whenever its run finished, whatever the verdict. It exits with
status 5 when it refuses before any VM, because the guest does not fit into this machine's
memory. With `--json` the refusal is also the same JSON line as the explorer's, `{"refused":
"<reason>", "status": 5}`.

### A schedule that ran once runs once

hydo repeats a run exactly, so a schedule that an exploration already ran gives the same result
again. The shrinker often tries a candidate that an earlier round tried, and a project with few
fault kinds draws one schedule for many seeds. `hydo explore` runs such a schedule once. A later
first run or shrink candidate with the same text takes the earlier run's verdict, fingerprint and
consoles, without a VM. Two kinds of runs always get their own VM: the re-runs that check that a
failure repeats, and the two confirmations of each minimal schedule.

- `exploration.json` counts these runs in its summary as `repeats`, with `repeats_saved_s`, the
  seconds that their earlier runs took. `report.md` shows both.
- The disk's seed `dseed=` decides only the faults of `disk=ioerr`, `disk=lying`, and the block
  faults (`torn=`, `mis=`, `lse=`, `hang=`). Where it decides nothing, the explorer writes
  `dseed=0`, so that two such schedules have one text. A schedule that you write keeps its own
  `dseed=` in `hydo run`.
- `--rerun-repeats` runs every repeated schedule in a VM again.

### Which failures are shrunk

`hydo explore` shrinks the first failure of each kind to a minimal schedule: the fewest and
smallest faults that still give the same failure. It shrinks a check's violation, and since
hydo 0.5.0 also these errors of your system, by their words with every number left out:

- a service that does not start again: `ERROR <service> restart failed` (also with its reason in
  parentheses) and `ERROR restart after the faults`;
- a workload or a check that does not end: `TIMEOUT workload`, `TIMEOUT check`, `TIMEOUT
  recovery`, and on the Mac engine `ERROR workload: exit budget exhausted` and `ERROR check: exit
  budget exhausted`.

So a service that never starts again after a crash gets its minimal schedule, as a lost write
does. A shrink candidate keeps the failure when it ends with the same error. The errors of hydo
and of its engine, for example a lost worker or a schedule that the engine refuses, are not
shrunk.

hydo can damage a service's files (`G=`) or its disk's blocks (`torn=`, `mis=`, `lse=`, `hang=`,
`G=...:bad` and `G=...:rot`). A service that does not start again after such damage is usually
the system's own check that found it, as its documentation promises. hydo recognizes it by the
failure's class, `corrupted-files` or `damaged-disk`, or by the restart's reason `its files were
corrupted`. Such an error is not shrunk, because its shrink only shows the damage again. Nor is
a new error line of the service's log (`hydo guide error-logs`) that first appeared in such a
run. `--shrink-detected-damage` shrinks both, for a system that must start on damaged files.

### Shrinks that continue from snapshots

A shrink candidate keeps the faults before the one it changes. So it runs again what an earlier
run of the shrink ran, up to a step range where the two differ. With `--shrink-from-snapshots`,
the re-runs of a failure and the shrink candidates take a snapshot where each of their step ranges
starts. A later candidate on the same worker continues from the deepest of them that it reaches,
instead of running that part again. It reaches a snapshot when it has the snapshot's host part,
`dseed=` and timing tokens included, and sends the same commands up to it. Every other candidate
runs from S1, as before. The verdicts, fingerprints, consoles and minimal schedules are the same
with the option and without it.

- `shrink-snapshots.json` in the exploration's folder counts the runs that continued, the
  snapshots taken and their seconds, and the share of those runs' guest time that was not run
  again.
- `--check-continued` runs every run that took or used a snapshot again from S1 and compares the
  two. A difference is printed and listed in the file, and the run from S1 counts.
- `--shrink-from-s1` runs every candidate from S1. `HYDO_SHRINK_SNAPSHOTS=1` or `0` sets the
  default; the options decide over it.
- It is on by default with the KVM engine, where a snapshot takes a few milliseconds and the
  Redis example's shrinks took 1 % to 7 % less time. It is off on the Mac engine, where a
  snapshot of a 1 GiB guest takes about 0.1 s and the shrinks of the Redis examples ran slower.
  It is off on hvx.

### Several explorations of one image

An onboarding explores in rounds, each with other options: a blind round, a round with
`--kill-now`, a round aimed at one event. Each round has its own report, and a problem that two
rounds found shows in both. Name every round's folder to see one list:

```sh
hydo report hydo-out/blind hydo-out/kill-now hydo-out/aimed
hydo report --html hydo-out/blind hydo-out/kill-now hydo-out/aimed   # the first folder's report-merged.html
```

The list names each round with its seeds and the options that tell it from the others. It shows
each problem once, with every round and seed that showed it: two failures are one problem when
their class and signature agree, the key each exploration shrinks by. A problem is shown as the
first round that found it shows it, with that round's minimal schedule and command, and `hydo
inspect diagnose` of that round's folder with its own problem number. The rounds must have
explored one image: two images are refused, because a problem of one is not a problem of the
other. `hydo explore --merge` is for the shards of one exploration, which share one command
line; this list is for rounds with different options.

## How much was explored, and whether to run more

`hydo explore` writes `stats.json` beside `exploration.json`. `hydo stats` prints it in words,
`hydo report` prints its short form, and `hydo report --html` draws its curve. The file is a
record of the host, so it changes no run, no fingerprint, and no verdict. It has four parts:

- The curve of new behaviour. After each first run in seed order, it counts the distinct
  fingerprints, verdicts, property outcomes (`<id>: held`, `<id>: failed`), error kinds, and
  error paths that the runs showed so far. A measure that the image does not record is left out.
- The cost. It gives the engine's seconds of the first runs, the re-runs and the shrinks, and the
  share of the workers' busy time that went to re-runs and shrinks. It also gives the engine's
  clock (VM exits on the Mac and KVM engines), the host CPU, and the largest resident memory.
  Each worker's time is split into busy, starting its engine, waiting for a quiet window, and
  idle.
- The faults drawn. Each fault kind has its runs, its targets, and its workload steps. A
  reaction (`A=`) counts as the fault that its action makes: `kill-<svc>` as a crash, `cut` as a power
  cut, and `powerloss-<svc>` as a power loss of one service. The kinds that `faults:` allows and
  no schedule drew are listed, each with the option that draws it.
- An estimate. It says how many first runs fit in a time budget with N workers, or on N machines
  with `--shard I/N`.

```sh
hydo stats                                    # hydo-out/explore
hydo stats <explore dir> --budget 2h --workers 4 --shards 8
hydo stats <explore dir> --json               # the document, with the asked estimate
```

`hydo stats` reads one exploration. It refuses two folders: run it on each, or make one
exploration of the shards of one with `hydo explore --merge`.

On the Mac engine the Redis quickstart's `hydo explore --seeds 1-60 --strategy swarm --workers 4`
took 62 s, and `hydo stats` began with this:

```text
New behaviour
  Flat over the last 15 runs: no new verdict or property outcome since run 13 of
    60.
  60 distinct run fingerprints (almost every run gives a new one; not judged)
  4 distinct verdicts, the last new one at run 13
  2 distinct property outcomes, the last new one at run 2
```

Read the curve this way:

- The fingerprints grow with almost every run, because two different schedules almost always
  give two different runs. The judgement leaves them out.
- The judgement looks at the last quarter of the runs, at least 10 of them. It is `growing` when
  a verdict, a property outcome, an error kind or an error path first showed there. It is `flat`
  otherwise, and it names the run where the curve last grew.
- A growing curve says that the last runs still showed new behaviour, so more seeds of the same
  options can show more. Raise `--seeds`.
- A flat curve says that more seeds of the same strategy and options mostly repeat what these
  runs showed. It does not say that the system has no more bugs. Other fault kinds, other
  strategies, a longer workload, or other checks can still find more. Try the kinds that the
  faults drawn list as never drawn before you add seeds.

The estimate counts `shards × ⌊(budget − start) × workers ÷ job × (1 − share)⌋`, rounded
down. Here `job` is a first run's seconds on one worker, and `start` is a worker's start. `share`
is the part of the busy time that re-runs and shrinks took. The estimate assumes that each shard
runs on a machine like this one. `seeds_from: history` of `hydo ci run` uses the same rule. `hydo explore --merge` makes
the statistics of the whole range from the parts' files, with the curve that one exploration of
the range gives.

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

## Does a new version behave the same

`hydo compare-versions` runs the same schedules on two versions of the system and says where the
two runs differ. hydo repeats a run exactly, so a difference always comes from the versions.
Use it before an upgrade, a dependency bump, or a refactor. Give the two versions as for `hydo
versions`, A first, and the schedules in one of three forms:

```sh
hydo compare-versions "disk=none | run" "disk=cut | run C=12" --param fsync=always,everysec
hydo compare-versions --regressions --git v1.4.0,v1.5.0 --build 'sh build.sh'
hydo compare-versions --seeds 1-20 --images old=a-arm64.cpio.gz,new=b-arm64.cpio.gz
```

`--regressions` takes the saved regressions beside version A's image. `--seeds` draws schedules
as `hydo explore` does, from version A's image, and takes the explorer's drawing options after
it. A drawn schedule whose runs differ is shrunk to a smallest schedule that still differs in
the same place.

It compares three levels. `--levels` picks some of them:

- `verdicts`: the verdict and each finding.
- `observations`: what the client saw. These are the lines of `$HYDO_OUTCOMES`, the
  acknowledged and failed lists, and the `HYDO-HIST` records without their guest times. The
  lists compare as sets of lines, sorted by operation id.
- `state`: what the system stored. These are the output of each service's `compare:` command
  (`hydo guide manifest`, "State for comparisons") and what each check printed.

A field that may differ, such as a generated id or a time, needs a rule. Then the rest must be
equal:

- `--ignore 'REGEX'` blanks each match; `--ignore 'REGEX=>TEXT'` writes TEXT instead.
- `--ignore-json PATH` blanks a JSON field, such as `items.*.created_at`.
- `hydo/compare.yaml` holds rules that every comparison of the project uses:
  `ignore: [{regex: ..., as: ...}, {json: ...}]`.

For each schedule the command prints the first difference: the workload steps, the record, what
A had, and what B had. It prints the commands that run the schedule again on each side. The
answer is `compare.json` in `--out` (default `hydo-out/compare`). `hydo report DIR` prints it,
and `hydo report --html DIR` writes a page. The exit status is 0 when the versions agree, 1 when
a schedule differs, and 2 when a side could not run.

## Which settings break a promise

Many findings come from one setting of the system's configuration: an fsync policy, a write
cache, a sync interval, a journal mode. `hydo explore --configs` explores each value of such a
setting with the same seeds and says which values lose acknowledged data, or break another check,
under which faults.

Make the setting a build parameter, and list its values under `configs:`:

```yaml
params:
  appendfsync: always              # the default: it comes first
configs:
  appendfsync:
    values: [always, everysec, "no"]
    service: redis                 # whose configuration it is (a name or a list)
    doc: "https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/"
    tradeoff:                      # what the project's documentation says each value weakens
      everysec: "you may lose 1 second of data if there is a disaster"
      "no": "the operating system decides when the data reaches the disk"
root:
  text:
    /etc/redis.conf: |
      appendfsync {{appendfsync}}
```

Then explore every combination:

```sh
hydo explore --configs hydo/hydo.yaml --seeds 1-20
```

- Each combination of values is one image (`hydo build --set appendfsync=everysec`). The images
  come from the image cache of `hydo versions` by their inputs, so a second run builds nothing.
- Each image is explored with the same seeds and the same explorer options. Each combination's
  exploration is a folder of its own in `hydo-out/configs/`, with its own report.
- With up to 12 combinations every combination is explored. With more, hydo explores a set that
  covers every pair of values of two settings. `--configs-mode one` explores the defaults, then
  one setting changed at a time.
- The table shows each combination's values and each class of failure that it found. Each class
  has its minimal schedule and the command that replays it.
- A failure found only with some values names those values. It is labeled the setting's
  documented trade-off, not a bug, in one case. Every combination that found it has a value with
  a `tradeoff:` note, and a combination without such a value was explored. Look at the class: a
  durability note explains lost acknowledged writes, not a crash.
- A failure found with the safe values too is not the setting's trade-off.
- The answer is `configs.json` (results type `configs`). `hydo report hydo-out/configs` prints the
  table, and `hydo report --html hydo-out/configs` writes it as a page.

Choose the faults the setting must survive in `faults:`. A disk that drops acknowledged writes
(`disk`) loses writes with every setting. Leave it out when you ask which setting is durable.
The guide `hydo guide strategies` has the evidence, in the entry on settings that trade
durability.

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
