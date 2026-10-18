# hydo.yaml — the project manifest

A project is onboarded by writing ONE file, `hydo.yaml`. `hvx/onboard/hydo-build` compiles it
into a guest image, and one generic guest init (`hvx/onboard/hydo-init.sh`) runs any manifest:
it starts the services, runs the workload under the explorer's faults and checks the
invariants. No per-project build script or boot script is written by hand.

This file is the reference for people and for tools that generate manifests (a Claude skill
that reads a repository and writes `hydo.yaml` is the intended next step). Complete
examples: `examples/redis/hydo.yaml` (one service, about 60 lines),
`examples/stack/hydo.yaml` (MySQL + Kafka + Redis + a gRPC front door), and
`hvx/knownbugs/sqlite/hydo.yaml` (no service: a multi-threaded program using SQLite; one
manifest, two versions via `params:`).

## What the user writes, and what is automatic

You write: which files make up the guest (store paths or host files), how each service
starts and how to tell it is ready, a workload split into numbered steps, the invariants as
shell commands, and optionally the durability settings the explorer may relax.

Automatic, for every project: the read-only ext4 root and the journaled ext4 data disk
(byte-reproducible), busybox, the deterministic CRNG seed, loopback networking, a fixed
start date for the clock, starting services in dependency order with readiness waits, the
explorer protocol (snapshot points S0/S1, one command per stop), every fault kind — kill,
pause, network loss, one-way loss, power cut, clock jump, disk faults — applied by service
name, restarting crashed services, HYDO-ASSERT records for every invariant, and the
HYDO-ORACLE verdict line the explorer reads.

## How a run goes

1. Boot. The init mounts `/proc`, `/sys`, `/dev`, `/tmp`, seeds the CRNG, brings `lo` up.
2. **S0** (snapshot point). The clock is set to `guest.clock_start`.
3. Provision: mount the data disk on `/data`, create `data.dirs`, start every service (in
   order), run `provision`, `sync` (provisioning is never part of the window under test).
4. **S1** (snapshot point). The explorer types one schedule, e.g. `run K=redis@5+3 C=12`.
5. Workload: steps `0 .. steps-1` run in ranges; between ranges the faults of the schedule
   are applied at their step numbers. `every:` invariants run in the background.
6. Without a power cut: faults are healed, dead services restarted, `workload.retry` runs,
   then every `final` invariant is checked and ONE verdict line is printed.
   With a power cut: everything freezes; only data that reached the disk survives; a fresh
   VM from S0 mounts that disk, starts the services (crash recovery; services with
   `recovery: false` are skipped) and checks the `final` invariants.

Without `hydo_phase=serve` on the kernel command line the image runs the same steps once, with
no faults ("solo").

## Schema

All keys are optional unless marked **required**. Unknown keys are errors (typos do not
silently do nothing).

```yaml
name: myapp                    # required. lowercase letters, digits, '-'
description: one line          # free text, anywhere below too

guest:
  ram_mib: 1024                # v1: must be 1024 (the explorer runs every guest at 1 GiB)
  clock_start: 1767225600      # unix seconds the realtime clock starts at (default 2026-01-01)

root:                          # what goes on the read-only root filesystem
  size_mib: 1792               # default: 1.3 x the content, rounded up to 64 MiB
  nix: [/nix/store/...-mysql-8.4.10]   # store paths; each FULL closure is copied to the
                                       # same path, so binaries run unchanged
  files:                       # host file or directory -> guest path
    - {src: build/myapp, dst: /bin/myapp, mode: "0755"}   # src relative to hydo.yaml, or ~ / absolute
  text:                        # small files written inline
    /etc/myapp.conf: |
      port 8080
  dirs: [/etc/myapp]           # empty directories to create
# image: myorg/app:1.4         # RESERVED for OCI/Docker images (a later version); an error today

data:                          # the read-write disk, mounted on /data; the fault target
  size_mib: 512                # default 512
  mount: rw                    # ext4 mount options
  dirs: [db, logs]             # created under /data on every mount

env:                           # exported to every command (services, workload, invariants)
  MYAPP_HOME: /opt/myapp

shell: |                       # shell helpers, sourced before every command
  SQL() { mysql -h127.0.0.1 -uroot -N -B "$@"; }

params:                        # build parameters: {{name}} in any string value below is replaced
  version: "3.51.2"            #   by the default here or by `hydo-build --set version=3.51.3`.
                               #   One manifest then builds several versions of a project (a
                               #   buggy and a fixed release). Values are strings; a {{name}}
                               #   that is not declared, or a --set of an undeclared name, is an
                               #   error. The values are recorded in the image as /etc/hydo/params.

services:                      # may be empty ([] or left out): a project that is only a
  - name: db                   #   workload program (e.g. a library under test). Started in this
                               #   order, moved only as far as depends_on requires.
    # name: required, [a-z][a-z0-9_]* -- it is also the name fault tokens use (K=db@...)
    cmd: /bin/mydb --data /data/db        # required. runs in the foreground; its output
                                          #   goes to $HYDO_LOG (/tmp/hydo/<name>.log)
    setup: '[ -d /data/db/x ] || /bin/mydb --init'   # run before EVERY start (incl. restarts
                                          #   after a kill and crash recovery); make it idempotent
    ready: /bin/mydb-cli ping             # required. exit 0 = ready; polled once per second
    ready_timeout: 120                    # seconds (default 120)
    port: 5432                            # the port network faults (N=, O=) act on
    depends_on: [cache]                   # start after these
    recovery: true                        # false: not started for the power-cut recovery check

provision: /bin/myapp-admin create-schema   # once, after all services are ready, before S1

workload:                      # required
  steps: 30                    # required, 1..1000. Faults are placed at step numbers.
  run: /bin/loadgen $HYDO_FROM $HYDO_TO     # required. runs steps [HYDO_FROM, HYDO_TO);
                               #   called several times per run (once per fault-free range)
  retry: /bin/loadgen --retry-unacked       # after faults heal (no power cut), optional
  timeout: 300                 # seconds per run/retry call; then it is killed (default 300).
                               #   A client without its own deadline hangs on a paused service.
  process: loadgen             # pattern (pgrep -f) telling whether a request is in flight
                               #   (used by mid-request pauses); default: run's first word

knobs:                         # durability settings the explorer may relax
  - token: M                   # one capital letter, not K N C P T O (those are faults)
    name: MySQL innodb_flush_log_at_trx_commit
    default: "1"               # required. the value the image starts with (the safe one)
    set: SQL -e "SET GLOBAL innodb_flush_log_at_trx_commit=$HYDO_VALUE"   # required
    values: ["0", "1", "2"]    # documentation for generators
    safe: "1"

invariants:                    # required, at least one
  - id: orders.exactly_once    # required. [A-Za-z0-9_.-]
    msg: every acknowledged order is stored exactly once
    type: always               # always (default) | always_or_unreachable
    cmd: /bin/check-orders $HYDO_ACKED      # required. exit 0 = holds, 1 = violated,
                               #   2 = could not be evaluated now (e.g. the db is down)
    every: 2                   # also check every 2 s of guest time during the workload
    final: true                # check at the end (default: true unless every: is given)

faults: [kill, pause, network, oneway, cut, clock, disk]   # which kinds make sense here;
                               # network/oneway also pull iproute2 into the image
```

### Variables every command sees

| variable | meaning |
|---|---|
| `HYDO_FROM`, `HYDO_TO` | the workload step range (`workload.run` only) |
| `HYDO_STEPS` | `workload.steps` |
| `HYDO_ACKED` | file: one id per line for every operation the client saw acknowledged. The workload appends; invariants read. Carried across a power cut. |
| `HYDO_OUTCOMES` | file: `<id> ack|failed|unknown` per attempt (the last line per id wins) |
| `HYDO_FAILED` | file: ids whose last outcome was `failed` (computed before the check) |
| `HYDO_LOG` | the service's log file (`setup`/`cmd`/`ready` only) |
| `HYDO_SVC` | the service's name |
| `HYDO_VALUE` | the knob value (`knobs[].set` only) |

### Invariants

An invariant is a shell command. Exit status 0 means the property holds, 1 means it is
violated, 2 means it could not be evaluated right now. Its last output line (or its own
`HYDO-ORACLE:` line, if it prints one) becomes the details.

- Every evaluation becomes a `HYDO-ASSERT` record (`hvx/ASSERTIONS.md`), once per id and
  outcome, so `hydo_assert_report` and the explorer's property table include them.
- `every:` invariants run while the workload runs. A violation is remembered and fails the
  run at the end.
- `final` invariants run once at the end (same boot, or the recovery boot after a power cut),
  in manifest order. The verdict is `HYDO-ORACLE: VIOLATION [<id>] <details>` for the first
  violated one, `HYDO-ORACLE: ERROR invariant <id> could not be evaluated` if one returned 2,
  else `HYDO-ORACLE: OK invariants=<n> held (<first id>: <details>)`.
- Use `always_or_unreachable` for a check that may legitimately never get to run (a periodic
  query while its service is killed).

The program under test can still use the assertion SDK (`hvx/stack/hydoassert`) for
properties that need to see inside it; both kinds of record mix on the console.

### Fault tokens (what the explorer types at S1)

| token | effect |
|---|---|
| `K=<svc>@<at>+<down>` | SIGKILL the service before step `at`; restart it (setup + cmd + ready) before step `at+down` |
| `P=<svc>@<at>+<n>` | SIGSTOP before step `at`, SIGCONT before step `at+n` |
| `P=<svc>@<at>~<off>:<dur>` | pause after `off` ms of guest time for `dur` ms, while steps run |
| `N=<svc>@<from>-<to>:<netem>` | `tc netem` on traffic to the service's `port` for steps `[from,to)` |
| `O=<svc>@<from>-<to>:req\|resp:<netem>` | one direction only |
| `C=<at>` | power cut before step `at` |
| `T=<at>:<delta>` | step the realtime clock by `delta` seconds |
| `<X>=<value>` | a knob from `knobs:` |
| host side: `disk=ioerr:<p>` / `lying:<p>` | disk I/O errors / a disk that drops acknowledged writes |

A token naming a service the manifest does not have is ignored, so the explorer's fixed
service names (mysql, kafka, redis, front) work against any project.

## YAML subset

The build machine has no YAML library, so `hydo-build` parses a subset: block mappings and
sequences, `- key: value` mapping items, `|` / `|-` literal and `>` / `>-` folded blocks,
flow `[a, b]` and `{k: v}`, single- and double-quoted strings, integers, `true`/`false`,
comments. Not supported: anchors and aliases, tags, multiple documents, multi-line plain
scalars. A `.json` file with the same structure is accepted as well.
Quote values YAML would otherwise turn into numbers or booleans when you mean a string
(`"1"`, `"no"`).

## Building and running

```sh
# on the build machine; tool paths default to build-stack.sh's pins
HYDO_SEEDRNG=~/hydo/build/hvx_seedrng hvx/onboard/hydo-build path/to/hydo.yaml
hvx/onboard/hydo-build path/to/hydo.yaml --set version=3.51.3   # a build parameter
hvx/onboard/hydo-build path/to/hydo.yaml --check         # validate only
hvx/onboard/hydo-build path/to/hydo.yaml --print-shell   # show the compiled manifest
# -> build/explore-ob-<name>-disk.img; copy it to ~/hydo/build on the bare-metal machine, then:
~/bin/hvx-run sudo ./hvx_explore one ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz \
    ~/hydo/build/explore-ob-<name>-disk.img <outdir> "disk=none dseed=1 | run C=12"
python3 hvx/explore/explore.py --image explore-ob-<name>-disk.img --seeds 1-50 --out <dir>
```

Inside the image: `/etc/hydo/hydo.yaml` (the manifest as written), `/etc/hydo/manifest.sh`
(compiled), `/etc/hydo/prelude.sh` (env + shell helpers), `/sbin/init` (`hydo-init.sh`).

## Limits of v1

- No OCI/Docker import yet (`image:` is reserved); files come from store paths or the host.
- Guest RAM is fixed at 1024 MiB and the kernel command line is the explorer's.
- The explorer's schedule generator (`explore.py`) knows the service names mysql, kafka,
  redis, front and the knobs M and R; other names get no kill/pause/network faults from it
  (hand-written schedules work for any name).
- One data disk, ext4. The workload's step count is the unit of fault placement.
