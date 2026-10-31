# hydo.yaml — the project manifest

A project is onboarded by writing ONE file, `hydo.yaml`. `hydo build` (or the `hydo-build` program) compiles it
into a guest image, and one generic guest init (`hydo/onboard/hydo-init.sh`) runs any manifest:
it starts the services, runs the workload under the explorer's faults and checks the
invariants. No per-project build script or boot script is written by hand.

This file is the reference for people and for tools that generate manifests: the onboarding
skill (`skills/hydo-onboard/`, an Agent Skills folder that any AI tool can follow; `hydo guide`
prints it, `hydo guide manifest` prints this file) reads a repository and writes its `hydo/`
folder. Complete
examples: `examples/redis/hydo.yaml` (one service, about 60 lines),
`examples/stack/hydo.yaml` (MySQL + Kafka + Redis + a gRPC front door),
`examples/redis-oci/hydo.yaml` (Redis from the official Docker image, no file list),
`examples/etcd/hydo.yaml` (a replicated system: a three-member etcd cluster, with link faults
between members), `examples/stack/hydo-read.yaml` (the stack's invariants written with the
ready-made store readers, see "Reading the stores"), and
`hydo/knownbugs/sqlite/hydo.yaml` (no service: a multi-threaded program using SQLite; one
manifest, two versions via `params:`).

## What the user writes, and what is automatic

You write: which files make up the guest (store paths, host files, or a Docker/OCI image per
service), how each service
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
  ram_mib: 1024                # guest RAM in MiB (default 1024): 256..3072 on x86_64, 256..4096 on arm64
  clock_start: 1767225600      # unix seconds the realtime clock starts at (default 2026-01-01)
  clock_after_cut: 60          # optional: after a power cut the recovery boot's clock continues from the cut, + 60 s
  cpu: v3                      # guest CPU level (x86_64 only): v2 (default) = x86-64-v2, no AVX;
                               # v3 = adds AVX/AVX2, FMA, BMI1/2, F16C, LZCNT, MOVBE, XSAVE

network: bridge                # loopback (default): one network for everything in the guest;
                               # bridge: each service in its own network namespace at its addr:
                               # (on arm64 with the second guest kernel; see "Service networks")

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
    addr: 127.0.0.11                      # the service's own loopback address: enables link
                                          #   faults between services (see "Link faults"); with
                                          #   network: bridge its address on the bridge, 10.89.x.y
                                          #   (see "Service networks")
    depends_on: [cache]                   # start after these
    recovery: true                        # false: not started for the power-cut recovery check

  - name: cache                # a service whose files come from a Docker/OCI image (see below)
    image: images/redis.tar    # a `docker save` tar or an OCI layout directory, relative to
                               #   hydo.yaml; or {x86_64: <path>, arm64: <path>}
    image_ref: redis:7.2.4     # which image in the file: a tag or sha256:<digest> (needed only
                               #   when the file holds more than one)
    cmd: redis-server --appendonly yes   # optional: replaces the image's CMD (string = shell
                               #   words, list = argv); the ENTRYPOINT stays
    entrypoint: [/bin/sh, -c]  # optional: replaces ENTRYPOINT (and, as in Docker, drops CMD)
    env: {TZ: UTC}             # optional: added to / overriding the image's Env
    workdir: /data             # optional: default the image's WorkingDir
    user: "999:999"            # optional: default the image's User ("" = root)
    ready: hydo-oci cache redis-cli ping | grep -q PONG

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
  - token: M                   # one capital letter, not K N C P T O Q X (built-in schedule tokens)
    name: MySQL innodb_flush_log_at_trx_commit
    default: "1"               # required. the value the image starts with (the safe one)
    set: SQL -e "SET GLOBAL innodb_flush_log_at_trx_commit=$HYDO_VALUE"   # required
    values: ["0", "1", "2"]    # documentation for generators
    safe: "1"

invariants:                    # required, at least one
  - id: orders.exactly_once    # required. [A-Za-z0-9_.-]
    msg: every acknowledged order is stored exactly once
    type: always               # always (default) | always_or_unreachable | eventually
    cmd: /bin/check-orders $HYDO_ACKED      # required. exit 0 = holds, 1 = violated,
                               #   2 = could not be evaluated now (e.g. the db is down)
    every: 2                   # also check every 2 s of guest time during the workload
    final: true                # check at the end (default: true unless every: is given)
  - id: orders.drained         # bounded liveness (see "Liveness" below)
    type: eventually
    cmd: /bin/count-pending | grep -qx 0      # exit 0 = satisfied now, anything else = not yet
    after: heal                # anchor: heal (default) | start
    within: 10                 # required: deadline in whole seconds of guest time (1..3600)
    poll: 1                    # seconds between checks (default 1); no every:/final: here

checks:                        # ready-made checkers for recorded histories (see "History
  - id: kv.linearizable        #   checks" below; hydo/HISTORIES.md). With checks:, invariants:
    type: linearizable         #   may be empty. linearizable | list-append
    history: kv                # required: the history the workload records (hydo_hist kv ...)
    model: register            # linearizable: register (default) | cas
    initial: nil               # linearizable: nil (default: keys start absent) | any
    final: /bin/read-all-keys  # optional: at the end (same boot / recovery boot), read the state
                               #   back into the history; exit != 0 = history incomplete (ERROR)
  - id: log.append
    type: list-append
    history: log
    consistency: strict-serializable   # list-append: the level the system claims (HISTORIES.md)

faults: [kill, pause, network, oneway, cut, clock, disk]   # which kinds make sense here (also
                               # freeze, protocol; see their sections);
                               # network/oneway also pull iproute2 into the image

errors: true                   # error logs as findings (see "Error logs as findings" below): scan
                               #   every service's log; or a mapping: level: error|warning,
                               #   max: 40 (kinds printed per boot), services: [names]
# and per service, under services[]:
#   errors:
#     format: auto             # auto (default: ERROR/FATAL/level=error/panic: ...) | redis | none
#     files: [/tmp/mysqld.err] # more log files the service writes (absolute paths)
#     match: 'regex'           # also an error line (POSIX ERE); ignore: 'regex' never is
#     normalize: ['req-[a-z0-9]+']   # more parts to blank out of the template
#     off: true                # not this service
```

`buggify: true` (top level, optional) says the project's programs carry BUGGIFY fault points
from an assertion SDK (Go `hydoassert.Buggify`, Rust `hydo_assert::hydo_buggify!`, JavaScript `buggify`; see
`hydo/ASSERTIONS.md`). The init then turns the schedule token `B=<seed-hex>[,<enable-pm>[,<fire-pm>]]`
(drawn by `hydo explore --buggify`, which keeps `B=` for such a manifest and drops it for others)
into `/tmp/hydo-buggify`, the file the SDKs read at a
program's first fault-point evaluation; without `B=` the file is absent and every point stays
off. The token `B` is then reserved (no knob may use it). Opt-in: `hydo-init-buggify.sh` is
spliced into the init only for a manifest that sets it, so other images build as before.

### Services from a Docker/OCI image

A service with `image:` takes its files from an image instead of `root.nix`/`root.files`.
hydo-build never pulls: the image is a file on the build machine. Make one with any of

```sh
docker save -o redis.tar redis:7.2.4                       # Docker (classic or >= 25 format)
hydo-fetch-image docker.io/library/redis sha256:<linux/amd64 manifest digest> redis:7.2.4 redis.tar
                                                           # comes with hydo, a docker-save style tar
skopeo copy docker://docker.io/library/redis@sha256:<manifest digest> \
       docker-archive:redis.tar:redis:7.2.4                # skopeo, a docker-save style tar
skopeo copy docker://docker.io/library/redis@sha256:<digest> oci:redis-oci:redis:7.2.4
                                                           # skopeo, an OCI layout directory
```

(gzip-compressed tars and OCI layouts holding a multi-platform index work too; the build picks
the `linux/<arch>` manifest. zstd layers are not supported by hydo-build: re-export
with `skopeo copy --dest-compress-format gzip`.) Pin by digest: `skopeo copy ...@sha256:...`,
and/or `image_ref: sha256:<manifest digest>` (OCI layout) or `sha256:<image id>` (docker save).
Every blob is checked against its digest and every layer against the config's `diff_ids`.
`examples/redis-oci/fetch-image.sh` is a complete example.

What the build does:

- **A root per service, not a merged root.** The layers are applied in order (whiteouts
  `.wh.<name>` and opaque directories `.wh..wh..opq` honoured) into `/oci/<service>` on the
  read-only root, and the service runs chrooted there, as in Docker. Images with different C
  libraries can coexist, and busybox, `/sbin/init` and the rest of the hydo root are untouched.
- `/proc`, `/sys`, `/dev`, `/tmp` and `/data` of the guest are bind-mounted into that root.
  `/data` is the data disk (the fault target): point the program's data directory there (it
  is the image's `WorkingDir` in many official images). `/tmp` is shared, so `$HYDO_ACKED`
  etc. are the same files inside. `/etc/hosts` in the image is replaced by
  `127.0.0.1 localhost` (Docker also writes it at run time).
- **Defaults from the image config, overridable in the manifest:** `ENTRYPOINT` + `CMD`
  (Docker semantics: `cmd:` replaces `CMD`; `entrypoint:` replaces `ENTRYPOINT` and drops
  `CMD`), `Env` (`env:` adds or overrides by name; `HOME` is the user's home from the image's
  `/etc/passwd`; `PATH` defaults to Docker's), `WorkingDir` (`workdir:`), `User` (`user:`,
  name or uid[:gid], switched with busybox `chpst`). The manifest's top-level `env:` and
  `shell:` are NOT exported into the image (they belong to the hydo side); the `HYDO_*`
  variables are.
- **Running commands inside the image:** `hydo-oci <service> <command> [args]` runs a command
  in the service's root with its env and working directory, as root. Use it in `ready:`, the
  workload, knobs and invariants (`hydo-oci cache redis-cli GET k`). A static busybox is put at
  `/.hydo/busybox` in each image root for `env -i`, `cd` and the user switch, so images without
  a shell (distroless) work.
- A killed, paused or frozen image service is the same process tree as any other service (the
  launcher `exec`s down to the program), so every fault applies unchanged.
- **Reproducible:** owners, groups and modes (including setuid) come from the layer tars and
  are written into the ext4 root with the pinned `debugfs` (x86_64) or into the cpio headers
  (arm64); timestamps are normalised like every other file (fixed mtime, zeroed
  ctime/atime/crtime). Two builds from the same image file are byte-identical; the files
  under `/oci/<service>` depend only on the image's content (`image.json` also records where it
  came from). Device nodes, FIFOs and extended
  attributes (file capabilities) in layers are skipped; a path containing `"`, `\` or a newline
  is refused.
- **Architecture:** the image config's `architecture` must match the build (`amd64` for the
  default x86_64 build, `arm64` for `--arch arm64`); anything else is refused with an error.
- Inside the image: `/etc/hydo/oci/<service>/run` (the launcher), `exec` (used by `hydo-oci`),
  `image.json` (digests, layers, the image config and the effective command/env/workdir/user).

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

- Every evaluation becomes a `HYDO-ASSERT` record (`hydo/ASSERTIONS.md`), once per id and
  outcome, so `hydo_assert_report` and the explorer's property table include them.
- `every:` invariants run while the workload runs. A violation is remembered and fails the
  run at the end.
- `final` invariants run once at the end (same boot, or the recovery boot after a power cut),
  in manifest order. The verdict is `HYDO-ORACLE: VIOLATION [<id>] <details>` for the first
  violated one, `HYDO-ORACLE: ERROR invariant <id> could not be evaluated` if one returned 2,
  else `HYDO-ORACLE: OK invariants=<n> held (<first id>: <details>)`.
- Use `always_or_unreachable` for a check that may legitimately never get to run (a periodic
  query while its service is killed).

### Liveness (`type: eventually`)

A safety invariant says "never X"; a liveness invariant says "X happens". hydo checks the
bounded form: **after an anchor, the check succeeds within `within` seconds of guest time**.
Guest time is deterministic under hvx, so the verdict and the measured time are the same on
every run of a schedule. Anchors:

| `after:` | the deadline counts from |
|---|---|
| `heal` (default) | the end of fault injection: the workload's last step has run. Everything the init then does to heal -- lift network faults, end pauses, restart killed services, the workload's `retry` -- counts against the deadline. After a power cut: the start of the recovery boot (the services' crash recovery counts). Solo: after the last step. |
| `start` | the start of the workload (after a power cut: the start of the recovery boot) |

From the anchor the init polls `cmd` in the background every `poll` seconds (each call bounded
by `within + 1` s). Exit 0 = satisfied; any other status = not yet. At the end (`finish`,
before the `final` invariants) it waits for the result and emits ONE `HYDO-ASSERT` record of
type `eventually` with `details` `{"after","elapsed_s","out","within_s"}`: `ok:true` if the
check succeeded by the deadline, `ok:false` if the deadline passed first. A missed deadline is
a violation: the verdict is `HYDO-ORACLE: VIOLATION [<id>] not satisfied <t>s after heal,
deadline <d>s (last check rc=1: <its last line>)`. Liveness results come before the `final`
invariants, so when both fail the verdict names the liveness one; the OK line's
`invariants=<n>` counts the `final` invariants only.

Deadlines are in seconds, not workload steps: after the heal no steps run, and a step's
duration depends on the faults. A fault that keeps a service unavailable after the workload
ends (for example a timed pause `P=<svc>@<at>~<off>:<dur>` longer than the remaining
workload -- the init waits for it before restarting anything) makes a `heal` deadline miss
deterministically.

Only images whose manifest has an `eventually` invariant get the liveness code: `hydo-build`
then inserts `hydo/onboard/hydo-liveness.sh` into `/sbin/init` before its `main` section, with
renamed copies of the four init functions it wraps (`step_range`, `start_periodic`,
`start_all`, `finish`). Every other image's `/sbin/init` is `hydo-init.sh` byte for byte.
`hydo-build --print-init` shows the generated init. Example: `examples/redis/hydo-live.yaml`.

The program under test can still use the assertion SDK (`hydo/stack/hydoassert`) for
properties that need to see inside it; both kinds of record mix on the console.

### History checks (`checks:`)

An invariant is a shell command the project writes itself. For the properties every data
system must keep, a check is stronger and needs no checking code: the workload **records** each
operation (invocation before the request, completion after the reply, `ok` / `fail` / `info` for
an unknown outcome) and the host checks the whole history after the run with a ready-made
checker: `linearizable` (a key-value register or compare-and-set object, per key, Porcupine /
Knossos style, with optional store-assigned versions) or `list-append` (Elle style: lost
acknowledged appends, duplicates, incompatible orders, reads going back in time, G0 / G1c /
G-single / G2 cycles at the chosen `consistency:`). The record format, the models and the
verdict rules are in `hydo/HISTORIES.md`.

- The workload, `retry`, `final:` and invariant commands get the recorder in their prelude
  (an executable sources `/etc/hydo/prelude.sh` to get it):
  `hydo_hist <history> <process> <invoke|ok|fail|info> <f> <args>...` (and `hydo_hist_enc` for
  free-form values). Give every concurrent client its own process number. Programs use the
  recorder of their SDK, all writing the same lines: Go `hydo/stack/hydohist`, Rust
  `hydo_assert::history`, JavaScript `hydo-assert/history` (`sdk/js/history.js`) and Python
  `hydo_assert.history` (Milestone 96).
- `final:` runs after the workload, `retry` and healing (no cut), or in the recovery boot after a
  power cut, before the invariants: read every key or list back and record the reads, so that a
  write lost in the crash shows up as a read that contradicts an acknowledged write. Its exit
  status is reported (`HYDO-CHECK-FINAL`); non-zero turns a passing check into an ERROR.
- The guest prints one `HYDO-CHECK <spec>` line per check when the workload starts. The checks run
  on the host: the explorer (`hydo explore`, and its shrinker), `hydo run` and
  `hydo check-history` combine the guest's verdict with the checks' (the guest's own invariant
  violations come first) and write the explanation, e.g. the minimal set of operations that cannot
  be ordered, next to the console (`<tag>.checks.txt`).
- Only manifests with `checks:` get the recorder and `hydo-init-history.sh` (spliced into
  `/sbin/init` like the liveness code); every other image builds byte-identically.
- Examples: `examples/redis/hydo-hist.yaml` (SET/GET as registers and RPUSH/LRANGE as
  list-append: `R=everysec` + a power cut loses an acknowledged write, both checks see it) and
  `examples/etcd/hydo-hist.yaml` (puts and gets with etcd's mod_revision, read back through every
  member: etcd issue 14685 shows as a version no order explains).
- On arm64 (the Mac backend) the same: the Mac release's `hydo explore`, `hydo run` and
  `hydo repro --regressions` run the host checkers (the hydo-history crate). Examples: `hydo/onboard/examples-mac/redis/hydo-hist.yaml` and
  `examples/python-kv/hydo-mac-hist.yaml` (the Python recorder).

### Fault tokens (what the explorer types at S1)

| token | effect |
|---|---|
| `K=<svc>@<at>+<down>` | SIGKILL the service before step `at`; restart it (setup + cmd + ready) before step `at+down` |
| `P=<svc>@<at>+<n>` | SIGSTOP before step `at`, SIGCONT before step `at+n` |
| `P=<svc>@<at>~<off>:<dur>` | pause after `off` ms of guest time for `dur` ms, while steps run; `off` may have a fraction down to microseconds (`60589.25`) |
| `N=<svc>@<from>-<to>:<netem>` | `tc netem` on traffic to the service's `port` for steps `[from,to)` |
| `O=<svc>@<from>-<to>:req\|resp:<netem>` | one direction only |
| `C=<at>` | power cut before step `at` |
| `T=<at>:<delta>` | step the realtime clock by `delta` seconds |
| `N=<a>-<b>@...`, `N=<a>-*@...` | netem on the link between two services with `addr:` (or between `a` and every other one); see "Link faults" |
| `O=<a>-<b>@<from>-<to>:req\|resp:<netem>` | one direction of a link: `req` = packets from `a` to `b`, `resp` = from `b` to `a` |
| `F=<svc>><peer>@<step>[#<n>][:<match>]:<dur>` | freeze `<svc>` at the n-th packet it **sends** to `<peer>` (`<` = **receives** from) at/after step `<step>`, for `<dur>` ms; only with `freeze` in `faults:` (see "Freeze at a call") |
| `H=<target>@<from>-<to>:<rule>[,<rule>...]` | a protocol fault on a fake or proxy for steps `[from,to)`: an error, slow, missing or reset answer, or a duplicated / reordered / late / lost webhook; see "Fake outside services" |
| `<X>=<value>` | a knob from `knobs:` |
| host side: `disk=ioerr:<p>` / `lying:<p>` | disk I/O errors / a disk that drops acknowledged writes |

Lifting a network fault (`N=`/`O=`, at `to` or at the end of the workload) does not drop
the packets still waiting in netem's delay queue: the init first changes the netem to no
delay/loss/duplication/corruption (queued packets keep their send times, new packets follow
them in order), waits until the queue is empty (at most the largest delay in force plus
500 ms of guest time), then removes the qdisc. The console shows
`network fault lifted: queued=<n> drained_ms=<ms> left=<n>`. Before 2026-10-23 the qdisc was
deleted at once and every queued packet was lost, so TCP retransmitted after a timeout.

A token naming a service the manifest does not have is ignored. `hydo explore` draws its
faults from the manifest (the hydo-explore crate's `space.rs`): kills and pauses from all
services, network and one-way faults from services with a `port:`, knob values from
`knobs:`, fault placement from `workload.steps`, fault kinds from `faults:`. It reads
`/etc/hydo/hydo.yaml` out of an `explore-ob-*` image automatically; pass `--manifest` for
any other image. A knob token that collides with a built-in token (K P N O C T Q X F) is
rejected. `hydo explore --freeze-call` draws `F=` for a manifest whose `faults:` lists `freeze`;
`hydo explore --grpc-faults` draws gRPC-level `H=` tokens for a manifest with a gRPC proxy (see "gRPC faults").

### Freeze at a call (stop a service at an exact point of its traffic)

Some bugs need a service stopped at one precise moment of a conversation -- between two
messages, not at a step boundary. List `freeze` in `faults:` and the explorer (and a hand
schedule) can use `F=<svc><dir><peer>@<step>[#<n>][:<match>]:<dur>`:

- `<dir>` is `>` (packets `<svc>` sends to `<peer>`) or `<` (packets it receives from `<peer>`);
- counting starts when workload step `<step>` begins; `<n>` (default 1) selects the n-th packet
  that carries payload in that direction (and, with `<match>`, contains those bytes; `%xx`
  escapes a byte). `#0` fires nothing and dumps a compact trace of the pair's traffic
  (`hydo-fc-trace:` lines) so you can read off the `<n>` of an event;
- at that packet the whole process tree of `<svc>` is SIGSTOPped -- held before it runs past the
  packet, which is then let through -- and SIGCONTed after `<dur>` ms of guest time.

It is deterministic under hydo, so the freeze lands on the same instruction every run. A packet
is held in a netfilter queue (`nfnetlink_queue` + the static helper `hydo_fc`, pinned
`iptables-legacy`); nothing of this is staged unless `freeze` is in `faults:`, so every other
image builds byte-identically. `freeze_match:` at the top level lists payload strings the
explorer may draw as `<match>` (so a known event has a small `<n>`): either a plain list, or a
mapping `replies: [...]` (strings that occur in packets the frozen service receives) and
`requests: [...]` (in packets it sends); a drawn string then gets its own direction. Which
`(svc, peer)` pairs the explorer freezes: a peer with a `port:` that `<svc>` reaches through its
`depends_on`; without such pairs, every ordered pair with a listening peer; and when no service
calls another (a single server), the peer `workload`: the workload's own client connections from
`127.0.0.1` to `<svc>`'s port (`F=redis<workload@3#2:500`; `examples/redis/hydo-freeze.yaml`).
`--arch arm64` (the Mac backend) builds the image for the second guest kernel, which has iptables
and NFQUEUE, and stages the aarch64 `iptables-legacy` and `hydo_fc` (see "On the Mac: the second
guest kernel" below).

A random draw rarely lands on the one packet a narrow-window bug needs. `hydo explore --sweep freeze`
tries every packet instead: for each packet stream (a pair, a direction, and either every packet
or one `freeze_match:` string) and each swept step `s` it first runs the schedule once with `#0`
and reads the guest's "freeze never fired: <N> of 0 matching events after step <s>" line, so N(s)
is the stream's packet count from step `s` to the end of fault injection. Step `s` then holds
N(s) - N(s+1) packets (the last step N(s)), and the sweep runs one schedule
`F=<svc><dir><peer>@<s>#<n>[:<match>]:<dur>` per packet n. Options: `--fc-pairs a>b,...`,
`--fc-dirs '<>'`, `--fc-steps 8-9`, `--fc-match all,manifest` (default: every packet plus each
`freeze_match:` string in its own direction), `--fc-durs 150000`, `--sweep-every k` (every k-th
packet), `--sweep-with "<tokens>"` (one more fixed fault on every point and trace run, e.g.
`K=kafka@9+1`; not a power cut, whose trace run never reaches the count), `--sweep-dry` (trace,
write `points.txt`, stop). Every failure is re-run twice. Trace counts are in `fc-traces.jsonl`,
point labels in `sweep-points.jsonl`.

Sweeping every packet is expensive (M76: 1,125 runs, 2 h 26 min for two links over two steps).
`--sweep freeze --fc-adaptive` spends the runs where failures are, in two stages:

- **Stage 1**, a cheap sweep over every allowed pair and every `--fc-steps` step: every packet of
  the manifest's `freeze_match:` streams, or, for a manifest without `freeze_match:`, every k-th
  packet (`--fc-stride k`, default 8) of the every-packet streams (`--fc-stage1 manifest|stride`,
  default `auto` = whichever the manifest allows; `manifest,stride` runs both).
- **Stage 2**, around every stage-1 failure: every packet in a window of +-W packets
  (`--fc-window W`, default 16) on the every-packet streams of the failing point's step -- its own
  pair in both directions and, with `--fc-links svc` (default; `pair` = only its own pair), both
  directions of every other freeze pair of the frozen service. Each window then widens until W
  packets beyond its outermost failure pass on both sides, so a failing band is mapped whole.
  The centre on the point's own stream is exact: the helper's `hydo-fc: FROZE ... (resp308, ...)`
  line gives the frozen packet's number among all packets of that direction. The opposite
  direction's centre is the last packet before it in the step's `#0` trace, another link's the
  packet nearest in guest time (extrapolated beyond the trace's 600 visible lines); the window
  absorbs the error. `--fc-centres fail,props,state` also centres windows on passing points whose
  sometimes / reachable / eventually outcomes (`props`) or verdict line (`state`) differ from the
  stream's trace run.

Every point is an ordinary schedule and every failure is re-run twice, as in the full sweep;
`fc-adaptive.json` lists the centres, windows and rounds. Without `--fc-adaptive` the sweep is
unchanged.

### Clock after a power cut (`guest.clock_after_cut`)

Every serve boot sets the realtime clock to `guest.clock_start`, the recovery boot after a power
cut included. A check that compares a time written in the first boot with one written in the
recovery boot (for example a broker append time against the time an engine processed an event
only after the restart) then sees time run backwards and can report a false violation.
`guest.clock_after_cut: <seconds>` makes the recovery boot continue the first boot's clock
instead: at the cut the first boot adds `clock=<epoch>` to its `hydo-explore-acked:` line, which
the harness passes on in the recovery boot's `oracle` command, and the recovery boot starts its
clock at that epoch plus `<seconds>` (the time the machine was off), as a real
clock keeps running across a power loss. Opt-in: `hydo-init-clock.sh` is spliced into the
init only for a manifest that sets it.

### Link faults (a network fault between two named services)

`N=` and `O=` on a service act on its `port:`, i.e. on traffic from every client to that service.
A replicated system also needs faults on the link between two members, leaving the others
connected (a partition). For that, give each member an `addr:`: its own loopback address
(`127.x.y.z`, not `127.0.0.1`, unique). Then a fault token may name a **link**:

- `N=a-b@<from>-<to>:<netem>`: netem on every packet between `a`'s and `b`'s addresses, both ways;
- `N=a-*@...`: the same between `a` and every other service with an `addr:` (isolates `a`);
- `O=a-b@...:req:<netem>` / `:resp:` : only the packets from `a` to `b` / from `b` to `a`.

hydo-init matches on the IP source and destination (tc u32 on `lo`), so the packets must carry
the members' addresses at both ends: each member listens on its `addr:` and its **outgoing**
connections must leave from it too. On loopback the kernel picks `127.0.0.1` as the source unless
the program binds its dialer or the routing says otherwise; `examples/etcd/hydo.yaml` shows a
recipe that needs no change to the program: run each member as its own user and add a policy
routing rule per uid (`ip rule add uidrange …`, a table whose `local 127.0.0.0/8` route has
`src <addr>`). Clients that connect from `127.0.0.1` are not touched by a link fault.

With two or more services that have `addr:`, `hydo explore` adds every pair
`a-b`, and with three or more every `a-*`, to the targets its network and one-way faults pick
from. The extension lives in `hydo-init-links.sh`; hydo-build copies it into the image and adds one
line to `/sbin/init` that sources it, only for manifests with an `addr:`, so every other
manifest builds a byte-identical image. `--arch arm64` builds such a manifest for the second guest
kernel (policy routing for the per-user source routes; see "On the Mac: the second guest kernel").

On one shared loopback network a program's outgoing connections leave from `127.0.0.1` unless it
is made to do otherwise, so most programs need the uid recipe above before a link fault touches
them. `network: bridge` (next section) removes that condition: every service's packets carry its
own address because the service has nothing else.

### Service networks (`network: bridge`)

```yaml
network: bridge          # default: loopback (one network shared by everything in the guest)
services:
  - name: api
    addr: 10.89.0.12     # required for every service: its address on the bridge
    port: 80
  - name: web
    addr: 10.89.0.13
    port: 80             # the same port as api: each service has its own network
```

Like Docker's bridge network, every service runs in its own network namespace:

- `lo` with `127.0.0.1` and `::1`, private to the service: `localhost` inside a service is that
  service, as inside a container. Two services may listen on the same port.
- `eth0` with the service's `addr:` (an address in `10.89.0.0/16` other than `10.89.0.1`, unique)
  and the MAC address `02:42:<addr in hex>`; the other end of its veth pair, `vh<i>`, is a port of
  the bridge `hydo0` in the guest's own namespace. IPv6 is off on the bridge and the veths (no
  router solicitations or duplicate-address checks on the wire); `lo` keeps `::1`.
- The bridge has `10.89.0.1/16`: the address of the guest's own namespace, where the workload,
  provisioning, the invariants and the fake outside world run. Their connections to a service
  come from `10.89.0.1`. There is no route anywhere else, so a connection to an outside address
  fails at once.
- `/etc/hosts`, when the manifest gives none, maps every service's name (and every fake's and
  proxy's) to its address, as Docker resolves service names.

The network is set up when `/sbin/init` starts, before the first snapshot, so every boot has it,
including the recovery boot after a power cut. What runs where:

| | where |
|---|---|
| a service's `setup:`, `cmd:` and `ready:` | in its namespace (a `ready:` that asks `localhost` asks the service itself, like a Compose healthcheck) |
| `hydo-oci <svc> ...` | in that service's namespace, like `docker exec` |
| `workload:`, `provision:`, invariants, knobs, `hydo-world` | in the guest's own namespace, on `10.89.0.1` |

Faults by name are unchanged: kills, pauses and the process-tree reach act on processes (one PID
namespace), a power cut freezes every service, and a killed service restarts in the same
namespace with the same address. Network faults are netem qdiscs on the veth where the packets
leave, so they act on the services' real traffic:

| token | packets | where |
|---|---|---|
| `N=<svc>@...`, `O=<svc>@...:req:...` | to `<svc>`'s `port:`, from every other service and the workload | its bridge port `vh<i>`, towards the service |
| `O=<svc>@...:resp:...` | from `<svc>`'s `port:` | `eth0` inside its namespace |
| `N=a-b@...` | between `a` and `b`, both ways (u32 on source and destination address) | `a -> b` on b's `vh`, `b -> a` on a's `vh` |
| `O=a-b@...:req|resp:...` | `a -> b` / `b -> a` only | as above |
| `N=a-*@...` | between `a` and every other service | the `vh` of every service involved |

A heal drains every netem queue of the fault before removing it (as `net_heal`, Milestone 71), and
the console line says how many packets were queued. `F=` (freeze at a call) holds the pair's
packets in the frozen service's namespace (`OUTPUT` for what it sends, `INPUT` for what it
receives; the workload peer is `10.89.0.1`); `fakes:` and `proxies:` default to `addr: 10.89.0.1`,
another address in `10.89.0.0/16` is added to the bridge, and a proxy's `to:` and a webhook's URL
use bridge addresses.

`hydo explore` draws link faults only where there is traffic: the pairs in which one service lists
the other in `depends_on` (a Compose import keeps Compose's `depends_on`), and `a-*` for a service
with two or more such peers. Services without any `depends_on` between them (peers that find each
other, such as a cluster) get every pair, as on loopback.

hydo-build stages iproute2's `ip` and the guest kernel's `llc`, `stp`, `bridge` and `veth` modules
and splices `hydo-init-netns.sh` into `/sbin/init`, only for `network: bridge`; every other
manifest builds byte-identically. On arm64 the default Mac guest
kernel (`hv/hvm/explore/build-guest.sh`) has no network namespaces, so such a manifest is built
for the second guest kernel (next section); the four modules are built into it. One bridge
models every Compose network: services on different Compose networks can still reach each other.
Examples: `examples/redis-bridge/` (two Redis servers on the same port, a replica link, a fault
proxy, freeze) and `examples/compose-voting/`.

### On the Mac: the second guest kernel (`Image-explore-netns`)

The Mac backend has two arm64 guest kernels. `Image-explore` (`hv/hvm/explore/build-guest.sh`) is
the default and is unchanged, so every recorded Mac fingerprint stays valid. `Image-explore-netns`
(`hv/hvm/explore/build-guest-netns.sh`, Milestone 99) is the same Linux 6.6.35 source, patches and
configuration plus `hv/hvm/explore/hvm-netns.config`: network namespaces, veth, the bridge, IPv6,
policy routing, and iptables (legacy, filter table) with NFQUEUE, all built in. `hydo-build --arch
arm64` builds a manifest for the second kernel when it has `network: bridge`, an `addr:` or
`freeze` in `faults:`; every other manifest builds exactly as before and boots the default kernel.
Such an image says so in its own root: `/etc/hydo/kernel` holds `Image-explore-netns <sha256>`, and
the build prints it. The Mac `hydo run`/`repro`/`explore` and the
report's reproduction lines read that record and boot exactly that kernel (it must sit next to
`Image-explore`, and a file with another sha256 is refused), so a reproduction uses the same
kernel as the run it reproduces; the report's summary shows `guest_kernel`. What the build stages
for these manifests only, from `$HYDO_ARM64_STORE` (a directory holding `nix/store/...`; the Mac
release sets its `guest/tc` tree; unset, the host's `/nix/store`): iproute2's `ip` (the same aarch64
store path as `tc`) for `network: bridge`, and for `freeze` the aarch64 `iptables-legacy` with the
three extensions the freeze's rules load and `hydo_fc` built for aarch64 (`$HYDO_FCBIN_ARM64`,
default `<build dir>/hydo_fc-arm64`; the Mac release ships `guest/hydo_fc`). Example
`hydo/onboard/examples-mac/redis-bridge`. On a Mac, `hydo compose` now writes `network: bridge`
with an `addr:` per service, as on Linux. A big image root is a big initramfs on the Mac (the root
lives in guest RAM): Docker's voting app needs `guest.ram_mib: 3072` there.

## Fake outside services and protocol faults (`fakes:`, `proxies:`, `H=`)

Business software mostly fails at its edges: a payment provider answers 500 or 429, answers too
late, takes the money and never answers, sends a webhook twice or out of order. `fakes:` declares
the outside services the system talks to; hydo serves them **inside the guest** (one static
program, `/bin/hydo-world`, source `hydo/onboard/world/`), so they are as deterministic as
everything else. Every request a fake (or proxy) receives is **recorded**, so invariants can ask
what the outside world saw: "the customer was charged at most once per order". `proxies:` puts
the same faults and records in front of a real service of the manifest.

```yaml
fakes:
  - name: payments             # [a-z][a-z0-9_]*, not a service name; H= tokens and checks name it
    port: 9001                 # required; listens on addr:port
    addr: 127.0.0.1            # default 127.0.0.1 (any 127.x.y.z)
    protocol: http             # http (default: HTTP/1.1 and cleartext HTTP/2) | grpc (see below)
    idempotency: header:Idempotency-Key   # where a request's idempotency key is (extractor)
    ref: json:order            # the business reference a request is about (extractor); per route too
    routes:                    # required for a fake; the first match wins
      - name: charge           # [a-z][a-z0-9_]*; the target <fake>.<route> of H= and of checks
        method: POST           # default: any
        path: /v1/charges      # exact, a prefix /v1/*, or with segments /v1/orders/{id}
        status: 200            # the answer (default 200), with headers: and a templated body:
        headers: {Content-Type: application/json}
        body: '{"id":"ch_{seq}","order":"{ref}","amount":{json:amount}}'
        idempotent: true       # a repeat with the same key gets the first answer, no new effect
        effect: true           # does a request here act in the world (default: not GET/HEAD/OPTIONS)
        webhook:               # after an effect, call the system back (asynchronously)
          url: http://127.0.0.1:8080/webhooks/payments
          body: '{"type":"charge.succeeded","id":"ch_{seq}","order":"{ref}"}'
          delay_ms: 20         # after the effect
          retries: 5           # a failed (non-2xx) delivery is retried after 0.5, 1, 2, ... s
proxies:
  - name: redisp               # a fault proxy in front of a real service
    port: 6380                 # the system talks to this port instead of the service's
    service: redis             # forward to that service's addr:port (or to: 127.0.0.1:6379)
    protocol: tcp              # tcp (any protocol; one connection = one request) | http | grpc
    # http/grpc proxies may have ref:, idempotency: and routes: (name/method/path/ref/effect only),
    # which name the requests in the records and as H= targets
```

**Extractors** (`idempotency:`, `ref:`): `header:<Name>`, `json:<field>` (dotted path into a JSON
body), `query:<name>`, `form:<name>`, `path:<name or 1-based segment>`, `pb:<field number>` (a
top-level field of the first gRPC message: a string or a varint), `body` (a hash of the body).

**Templates** (answer bodies, header values, webhook url/body/headers): `{seq}` (the record number
of the request; replays use the original's), `{ref}`, `{idem}`, `{fake}`, `{route}`; in answers
also `{json:a.b}`, `{header:X}`, `{query:x}`, `{path:x}`, `{form:x}`, `{pb:n}` from the request.
Single braces: `{{name}}` is a build parameter (`params:`). A webhook may use only the first five
(it can be re-sent after a power cut, without its request).

**gRPC** (`protocol: grpc`): cleartext HTTP/2 with prior knowledge. A route's `path` is the method
(`/pkg.Service/Method`), its answer `message:` (hex protobuf bytes, default empty) and `code:`
(gRPC status, default 0); an error rule answers trailers-only with `grpc-status`. Status codes in
`H=` rules may be numbers or names (`status=UNAVAILABLE`).

### The record (what the outside world saw)

`/tmp/hydo/world.log` in the guest, one line per event, in arrival order, fields `k=v` (values
%-escaped):

```
seq=7 t=<unix ms> kind=req ep=payments route=charge from=127.0.0.1 method=POST path=/v1/charges
      ref=o2 idem=charge-o2 body=<hash> fx=1 out=ok+delay3000 rule=1
seq=8 ... kind=req ... fx=0 out=replay+delay3000 orig=7 rule=1      (an idempotent repeat)
seq=11 ... kind=hook ep=payments route=charge ref=o2 src=7 n=1 dup=0 out=200   (webhook delivery)
seq=13 ... kind=event ep=- route=cancelled ref=o2 fx=1               (hydo-world event)
```

`fx=1` marks a request that took effect in the world (an effectful route, answered or not, not an
idempotent replay; for a proxy: forwarded to the service). `out` is how it was answered: `ok`,
`replay`, `fwd` (proxy), `-` (the world never acted: an error rule without `effect=yes`), plus
`+delay<ms>`, `+drop`, `+reset`, `+status<code>`. A webhook delivery `out` is the HTTP status the
system answered, `err`, `lost` or `gave-up`. Just before the verdict the console gets a compact
copy (`hydo-world: <seq> payments.charge ref=o2 idem=charge-o2 fx=1 ok+delay3000`, at most 150
lines).

The workload (or the system) can add its own events to the same ordered record:
`hydo-world event <name> <ref>` (for example `hydo-world event cancelled o2` once a cancellation
is acknowledged), which "no side effect after <event>" checks compare against.

### Side-effect checks

`hydo-world check <kind> <target> [options]` reads the record; use it as an invariant's `cmd:`
(exit 0 holds, 1 violated, 2 cannot decide; its `HYDO-ORACLE:` line becomes the details, so
`HYDO-ASSERT` records, the verdict, the explorer and the shrinker work unchanged):

| kind | holds when |
|---|---|
| `at-most-once <ep>.<route> [--by ref\|idem]` | no ref (or idempotency key) took effect twice |
| `effect-for-acked <ep>.<route> [--ref 'o{id}'] [--acked FILE] [--unless event:<name>\|<ep>.<route>]` | every acknowledged id (`$HYDO_ACKED`; `--ref` turns an id into the ref the effect carries) has an effect, except refs with an `--unless` record (an order whose cancellation was requested owes no receipt). As a `type: eventually` invariant: "every acknowledged request produced its side effect within N s" |
| `exactly-once <ep>.<route> [--ref ...]` | both of the above |
| `none-after <ep>.<route> --after event:<name>\|<ep>.<route>` | no ref took effect after that ref's first `<after>` record (no payment after its cancellation) |
| `count <ep>.<route>` | always; prints the number of effects |

`examples/shop/hydo.yaml` uses each of them.

### Protocol faults (`H=`)

```
H=<target>@<from>-<to>:<rule>[,<rule>...]
  target   <fake|proxy> (every route) or <fake|proxy>.<route>
  window   on while workload steps [from, to) run (switched at step boundaries, like N=), off when
           fault injection ends (and frozen with everything else at a power cut)
  request rules (what the answer to a matching request becomes):
    status=<code>       an error answer: an HTTP status, or for grpc a gRPC code (number or name)
    retry-after=<s>     with status=: a Retry-After header (429, 503)
    delay=<ms>          the answer comes <ms> of guest time late (combines with status= / drop / reset)
    drop                no answer: the request is held until the client gives up or the rule ends
    reset               the connection is reset instead of answered
  webhook rules (on the webhooks of effects made while the rule is on):
    dup                 delivered twice
    reorder             delivered after the next webhook of the same route (pairs swap)
    late=<ms>           delivered <ms> later
    lost                never delivered
  modifiers:
    p=<1..100>          share of the matching requests (or webhooks) the rule acts on (default 100)
    effect=yes|no       does the world still perform the side effect? Default: no for status=
                        (rejected before acting), yes for delay/drop/reset (the request arrived and
                        acted; only the answer is slow or lost). status=500,effect=yes = "charged,
                        then failed to say so"
```

On a `tcp` proxy a request is a connection: `delay` holds every chunk the service sends back,
`drop` lets the client's bytes through and never returns the answer, `reset` resets the connection
when the answer arrives (`effect=no`: nothing reaches the service); `status=` has no meaning there.

Determinism: which matching request a rule acts on is a pure function of the token text and the
request's index among those the rule has seen (`mix64(fnv64(token) + k) % 100 < p`); the order of
requests is the guest's, which hvx makes deterministic. Several `H=` tokens may be given; the first
firing request rule (in token order) decides a request, and every webhook rule that fires applies.
A target the manifest does not have is ignored (as for every fault token). `H` is reserved: no knob
may use it.

**Power cut.** A cut stops the system under test, not the outside world. At the cut the world is
frozen (a request that arrives later is never recorded) and its effects, events, idempotency memory
and undelivered webhooks are appended to the acknowledged ids as `w:` tokens (at most 3000 bytes;
`hvx_explore` carries that line into the recovery boot's `oracle` command, which has room for it
since this feature). The recovery boot's world starts from them: the checks see both boots, a repeat
of an old idempotency key is still a replay, and undelivered webhooks are re-sent. If the carried
record does not fit, it is replaced by `w:trunc` and every check answers 2 (cannot decide), never a
false verdict.

**Explorer.** `hydo explore --protocol-faults` draws `H=` (one token on 60% of the schedules, a
second on 25%) on its own random stream, from the targets the manifest gives: every fake
route (with the webhook rules when it has a webhook), every proxy and proxy route. Without the flag
every schedule string is byte-identical.
`--sweep protocol` runs every rule kind (status, status with effect=yes, delay=3000, drop, reset,
and on webhook routes dup, reorder, late=5000, lost) on every target, one workload step at a time. The shrinker drops
`H=` tokens, narrows their window, and removes `p=`, `retry-after=` and `effect=`, then halves
delays. A manifest with a `faults:` list draws `H=` only if it lists `protocol`.

**Build.** `hydo/onboard/world/build.sh` builds `build/hydo-world` (pinned Go, standard library
only); `hydo-build` stages it (`HYDO_WORLDBIN` overrides), writes `/etc/hydo/world.json`
(`--print-world` shows it) and splices `hydo/onboard/hydo-init-fakes.sh` into the init. A manifest
without `fakes:`/`proxies:` builds a byte-identical image. `--arch arm64` (the Mac backend) stages
the arm64 build of the same program: `HYDO_WORLD_GOARCH=arm64 hydo/onboard/world/build.sh
build/hydo-world-arm64` (`HYDO_WORLDBIN_ARM64` overrides; the Mac release ships it as
`guest/hydo-world`), and the Mac engine `hvmx` carries the world across a power cut in the same
3584-byte recovery command as `hvx_explore`; `hydo explore` on the Mac takes `--protocol-faults`
and `--grpc-faults` (Milestone 96).

**Extending it (the code layout).** `world/rules.go`: the `H=` grammar (`parseRule`, one `Rule`
per token, `fires`, `applied`); a new rule is a field there plus its action. `world/serve.go`: one
handler per endpoint protocol -- HTTP and gRPC share `handler()` (decide under the world lock,
record, then act: delay / drop / reset / status / answer), `serveTCP` is the byte-stream proxy;
a new protocol is a new serve function that calls `lockGate`, `requestRule`, `record` in that order.
`world/world.go`: records, webhooks, the control socket (`on`/`off`/`heal`/`event`/`dump`/`summary`)
and the power-cut carry. `world/check.go`: the checks. `world/config.go`: `world.json`, extractors,
templates. The explorer's draws are `add_protocol_faults` in the hydo-explore crate (rule kinds
per target come from its `space.rs`).
## The test composer (`test:` and the `hydo/` folder)

Instead of a fixed `workload.run` split into numbered steps, a project can write its test as
**small commands and properties** and let hydo compose the commands into seeded sequences and
concurrent mixes. The composition is part of the schedule (token `W=`), so a schedule replays
the same commands in the same order and overlap, bit for bit, and the explorer varies and
shrinks it like a fault. Implementation: the hydo-composer crate (build side, explorer side),
`hydo-composer.sh` (the guest side; staged only for such a plan). Example:
`examples/redis-composed/` (the repository layout below, with SETs, transfers, AOF rewrites).

### The plan format (what SDKs generate; versioned)

The engine-facing plan is the manifest itself, in YAML or JSON (`hydo build plan.json` works
the same). It is language-neutral: commands and checks are executables given as an **argv
list** (exec'd, no shell); a string is accepted as a shell command for hand-written plans. An
SDK (`sdk/rust`, the `hydo-test` crate) writes this file on the host from the team's own
program; nothing of the SDK's setup code runs in the guest.

```yaml
format: 1                      # the plan format; this hydo reads 1 (anything else is refused)
hydo_version: "0.1"            # the hydo the plan was written for: "0.1" (same major.minor; a
                               #   different patch only warns), "0.1.0", or ">=0.1". A mismatch
                               #   is an error unless HYDO_VERSION_CHECK=warn. hydo/VERSION (or the
                               #   release's VERSION) is the engine's version.
workload:
  steps: 20                    # rounds of the composition (default 20); faults are placed at
                               #   round numbers exactly as at step numbers. No run:/process:
  timeout: 300                 # as before: bound of one step-range call (the composer's wait)
test:
  parallel: 4                  # at most this many commands at once (W= draws p in 1..parallel)
  per_round: 2                 # parallel drivers started per round (W= draws q in 1..per_round)
  round_ms: 200                # a round every round_ms of guest time (W= draws round_ms/2, x1, x2)
  command_timeout: 60          # seconds per command (a command's own timeout: overrides it)
  drain_timeout: 60            # after the last round, running commands get this long, then are killed
  max_commands: 0              # 0 = no limit (W='s n caps the number of commands too)
  budget_ms: 0                 # 0 = none; else no command starts later than this after round 0
  check_every: 2               # default period of `always` checks (seconds of guest time)
  check_within: 30             # default deadline of `eventually` checks
  commands:
    - {name: place_order, kind: parallel_driver, run: [/bin/shoptest, driver, place_order]}
    - {name: audit, kind: serial_driver, run: [/bin/shoptest, driver, audit], timeout: 120}
    - {name: seed_catalog, kind: first, run: [/bin/shoptest, first, seed_catalog]}
  checks:
    - {name: never_charged_twice, kind: final, run: [/bin/shoptest, check, never_charged_twice]}
    - {name: orders_settle, kind: eventually, within: 20, run: [/bin/shoptest, check, orders_settle]}
  regressions:
    - {name: double-charge, schedule: "disk=none dseed=1 | run K=pay@4+1 W=0000002a,p4,q2,r200", expect: ok}
```

A command's id is `<kind>_<name>` (`parallel_driver_place_order`); ids are unique and are how
saved compositions name commands ("Stable compositions" below). A command may carry `content:
<hex>` (8 to 64 lowercase hex digits), a hash of its code that an SDK computes, so a saved
regression warns when the command changed. A check's name is its property id. Everything the composer adds to the manifest (the workload, a `first` step in
provisioning, invariants for checks and for `eventually`/`finally` commands) is a rewrite the
generic init already understands, so `hydo-init.sh` is unchanged; a manifest without `test:`
and without a `hydo/` folder builds byte-identically to before.

### The `hydo/` folder (the repository layout)

```
hydo/
  hydo.yaml          services, data, knobs, allowed faults, guest settings (and test: options)
  workload/          commands: <kind>_<name> executables; helper_* files and dot files are ignored
  checks/            properties: final_<name>, always_<name>, eventually_<name>
  regressions/       <name>.schedule: saved minimal schedules, replayed by `hydo repro --regressions`
.hydo/               build cache, git-ignored (`hydo build` writes here for a manifest in hydo/)
```

`hydo-build` reads the three directories when they exist next to the manifest and adds their
entries to `test:` (a name in both is an error; an unknown prefix or a non-executable file is
an error, so typos do not silently do nothing). Each file becomes `run: [/hydo/<dir>/<file>]`;
the whole directory, helpers included, is copied to `/hydo/<dir>/` on the read-only root. A
text file may carry a header in its first 4 KiB: `# hydo: timeout=30` (commands), `# hydo:
every=1` / `# hydo: within=10 after=start poll=1` (checks), `# hydo-msg: <description>`.

### Command kinds (when they run)

| kind | runs | faults | alongside |
|---|---|---|---|
| `first` | every `first` command once, in name order, during provisioning (before S1: in the snapshot every schedule starts from; `HYDO-SETUP-COMPLETE` comes after provisioning, so after the first commands) | none | nothing |
| `parallel_driver` | repeatedly, in rounds, several at once (also copies of one command) | yes, also mid-command | other parallel drivers, anytime |
| `serial_driver` | repeatedly, never two at once; waits until no other driver runs | yes | anytime only |
| `singleton_driver` | once per run, in a run that has no other drivers: when a plan has one, 1 run in 4 (or every run, without other drivers) is such a run | yes | anytime only |
| `anytime` | in rounds, any time | yes | anything |
| `eventually` | after the faults stop (after the heal, the host's "go", and in the recovery boot after a power cut), once each, in name order | none | nothing |
| `finally` | at the end, after the `eventually` commands, only when every command started in this boot ran to completion (no timeout, no drain kill, no power cut), so never in a recovery boot | none | nothing |

**Exit status.** A driver or anytime command: 0 = ok, **100 = the command saw a violation**
(the run fails: the `compose.commands` invariant and an `always_or_unreachable` property
`compose.<id>.no_violation`), anything else = the operation failed (recorded, not fatal:
under faults, operations fail). A `first` command that fails fails provisioning (`HYDO-ORACLE:
ERROR provision failed`). An `eventually` or `finally` command is an invariant: 0 holds,
anything else is a violation. A check: 0 holds, 1 violated, 2 could not be evaluated now (as
any invariant); a check that times out counts as 2.

**Check kinds.** `final`: once at the end (an `always` invariant with `final: true`).
`always`: also every `every` seconds while the workload runs (`check_every` by default).
`eventually`: bounded liveness, polled after `after` (`heal` default, or `start`) until it
holds or `within` seconds pass (MANIFEST "Liveness").
`linearizable` / `list_append` (alias `list-append`): a ready-made **history check**
(`hydo/HISTORIES.md`, "History checks" above): the commands record their operations with
`hydo_hist` (source `/etc/hydo/prelude.sh`; process = `$HYDO_CMD_ID`, unique per entry), and the
host checks the history after the run. Fields: `history` (required), `model`/`initial`
(linearizable), `consistency` (list_append), `msg`; `run` is optional and is the final read-back
(run at the end of the same boot or of the recovery boot). In the `hydo/` folder the file
`checks/linearizable_<name>` or `checks/list_append_<name>` is the read-back, with
`# hydo: history=kv model=register` (or `consistency=...`) in its header. They become top-level
`checks:` entries, not invariants. Example: `examples/redis-composed-hist/`.

**Environment of a command.** Everything an invariant sees (`HYDO_ACKED`, `HYDO_OUTCOMES`,
the manifest's `env:`), plus `HYDO_CMD` (its id), `HYDO_CMD_ID` (its entry number, unique in
a run: a good key for the operation), `HYDO_ROUND`, and `HYDO_SEED` (a number from the
composition's random stream, for the command's own choices: which account, how much). Its
output goes to `/tmp/hydo/compose/log.<entry>`; `HYDO-ASSERT` lines it prints are forwarded to
the console (the SDKs write to the console directly).

### How a composition runs

The plan of a run is a pure function of `W=` and the command list (the hydo-composer crate's
plan; the same code in awk in the guest, checked by that crate's `tests/awk.rs`): for each of `steps`
rounds, either a serial driver (1 round in 4 when there are parallel drivers), or 1..q
parallel drivers drawn with repetition, plus an anytime command in 1 round of 3; or, in a
singleton run, the singleton in round 0. Each entry gets its own `HYDO_SEED`. Round r starts
`r x round_ms` of guest time after round 0 (later when a fault point held it); a command keeps
running across later rounds and fault points until it ends, so faults land mid-command.
Rules while running: at most `p` commands at once; parallel drivers wait while a serial or
singleton driver runs; serial and singleton drivers wait until no other driver runs. At a
fault point before round t the composer stops starting rounds, the init applies the fault,
then round t starts. After the last round the composer waits for the running commands
(`drain_timeout`), kills what is left, and the run continues as before (heal, restart,
`eventually` commands, checks, verdict). At a power cut everything freezes as before.

**`W=` (guest token, the explorer draws it; hand schedules may use it):**
`W=<seed, hex>[,p<concurrency>][,q<per round>][,r<round ms>][,n<max commands>][,t<budget ms>][,x<entries>][,o<commands>]`,
where `x` lists plan entries not to run and `o` commands never to run, by id joined with `+`
(`oparallel_driver_set+serial_driver_rewrite`; the shrinker writes ids) or, in schedules from
before M98, by index in id order as `3.5-9.12`. Without `W=` the guest runs seed 0 with the
manifest's defaults. This seed form is what the explorer draws; the shrinker's output and saved
regressions use the resolved form (below, "Stable compositions"). `hydo explore`
puts a `W=` on every schedule of such a plan, from its own random stream, so every other
field equals the schedule without it (`--no-compose`: no `W=`); manifests without a test plan
get exactly the old schedules. The shrinker tries, in order:
one command never run (`o`), half the commands (`n`), no concurrency (`p1`, then `p-1`), the
first half of the entries dropped (`x`), single entries (when 8 or fewer are left); the report
prints the minimal composition round by round.

**Console lines** (one write each; the explorer and reports read them):

```
HYDO-COMPOSE plan W=0000002a,p4,q2,r100 seed=42 p=4 q=2 r=100 n=0 rounds=20 entries=34
HYDO-CMD start i=3 r=2 kind=parallel_driver cmd=parallel_driver_transfer
HYDO-CMD end i=3 cmd=parallel_driver_transfer rc=0 ms=30 status=ok      (ok|failed|timeout|violation|killed)
HYDO-CMD skip i=30 r=17 cmd=... budget_ms=2000
HYDO-COMPOSE done started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0
```

plus `HYDO-ASSERT-DECL`/`HYDO-ASSERT` records: `compose.<id>.succeeded` (sometimes: the
command exited 0 at least once; NOT EXERCISED says the test never got that operation through)
and `compose.<id>.no_violation` (always_or_unreachable). The invariant `compose.commands`
(first in the verdict order) fails on an exit 100 and otherwise reports the counts.

### Regressions

`hydo/regressions/<name>.schedule` (or `test.regressions`): comment lines, `# expect: ok`
(default: the bug stays fixed), `# expect: violation` (a known open bug still fails) or
`# expect: fp=<16 hex>` (bit for bit), and one schedule line. They are staged into the image
(`/hydo/regressions/`, and `/etc/hydo/compose.json`), so `hydo repro --regressions [IMG]`
replays exactly what the image was built with, from fresh boots, and exits 1 if one is not as
expected. `hydo save <explore dir>` writes an exploration's minimal schedules there (with
`expect: violation`; change it to `ok` once fixed).

### Stable compositions (command ids)

A seed-form `W=` composes by drawing positions in the plan's sorted command list, so adding,
removing or renaming a command changes what it composes. Saved regressions therefore do not use
it. Every command has a stable **id**, `<kind>_<name>`: the file name in `hydo/workload/`, or
`kind` and `name` in the plan (an SDK's attribute name). Ids are unique; `hydo-build` refuses a
duplicate, and `hydo-test` refuses two drivers with the same name.

**The resolved form** lists the composition itself, with commands by id:

```
W=:p2,r200[,t<budget ms>],c<id>+<id>...,<entry>.<round>.<k>.<seed>,...
   e.g. W=:p2,r200,cparallel_driver_transfer,7.4.0.721476632,8.4.0.40276333
```

`p` and `r` are concurrency and round length, `c` is the table of the ids used, and each entry
gives its entry number (`HYDO_CMD_ID`), its round, its command (an index into `c`) and its
`HYDO_SEED`. The guest looks the ids up in the image's list, so the order of the list and the
commands added to it do not matter, and nothing depends on how a seed maps to positions (a later
hydo may draw differently and old regressions still replay). It runs exactly these entries,
in this order, with the same timing rules as the seed form.

**The saved form** `W=@<key>` names a resolved composition by a hash of its text (12 hex digits).
A regression file carries the resolved form on a `# compose: <resolved form>` line, and `hydo
build` stages it as `/etc/hydo/compose.saved/<key>`; it is used when the resolved form would make
the schedule's guest part longer than 500 bytes (the engine's limit is 511).

**Where each form appears.** The explorer draws the seed form. After shrinking, it turns the
minimal composition into the resolved form and runs that twice; when it fails the same way, the
resolved schedule is the reported minimal schedule (`report.json` `shrunk[].stable`,
`stable_schedule`, `contents`). Otherwise (the resolved run is a different run: the guest starts
round 0 at another time, so a timing-sensitive failure may not recur) the report keeps the seed
form and says why (`stable_note`), and `hydo save` writes that seed form with its command list
(`# commands:`, the old index form below): it replays the failing run exactly while the list is
unchanged, and by id once the list changes. `hydo save` writes:

```
# found by `hydo explore` (seed 6): violation total_conserved
# composition: r4: parallel_driver_transfer, parallel_driver_transfer
# command-sha: parallel_driver_transfer 549e0b6d2561dc5b
# expect: violation      (change to ok when the bug is fixed)
disk=none dseed=1d0b14e4db018fed | run W=:p2,r200,cparallel_driver_transfer,7.4.0.721476632,8.4.0.40276333
```

(a long composition: `W=@<key>` and a `# compose:` line). In `test.regressions` the same fields
are `compose:` and `contents: {<id>: <hash>}`.

**What the replay checks** (`hydo build` warns, `hydo repro --regressions` acts):

- a command the composition uses is not in the plan: that regression FAILS without running, with
  a message naming the command and the plan's commands now; a `W=` naming a missing command in
  any other run fails the knob step in the guest (`HYDO-COMPOSE error: ...`, verdict
  `HYDO-ORACLE: ERROR knob W=... failed`);
- an entry's round is not below `workload.steps`: the same failure;
- a used command's content changed since the save: a warning. The content hash is the first 16
  hex digits of SHA-256 of the file (`hydo/` folder), the plan's `content:` (an SDK's hash of the
  function, `hydo-test`), or else of the command's `run` and `timeout`. Helpers the command
  sources or calls are not covered.

**Old regressions.** A regression in the seed form with `# commands: <ids>` (what `hydo save`
wrote before M98: the list at save time; `commands:` in `test.regressions`) replays against that
list: when the plan's list differs, `hydo build` resolves the composition with the saved list
and stages the resolved form, so it runs the commands it ran when saved. A seed form without a
saved list still composes from the current list, with a build warning. The plan format stays
`format: 1`: old plans read the same, and a new plan's only new key (`content:`) is refused by
an older hydo as an unknown field.

## Reading the stores (`hydo-read`, `hydo-check`)

Cross-service invariants need "what is in this table / on this topic / under this key". Instead
of writing that reading code in shell for every manifest, a check can call two ready-made
commands (source: `hydo/crates/hydo-read`; one static Rust binary, `hydo-check` is a symlink to it):

- `hydo-read <store> <args>` prints what a store holds as JSON Lines, one record per line,
  keys sorted, lines sorted bytewise (Kafka, Redis lists and sorted sets keep the store's
  order). Exit 0 = read, 2 = could not read (the store is down, the table or topic does not
  exist, a timeout; also a wrong command line). Every read has a deadline (`--timeout <s>`,
  default 30).
- `hydo-check <check> <sources>` compares readings and prints ONE explanation line (the last
  line of its output, which the init puts into the invariant's `HYDO-ASSERT` details and the
  `HYDO-ORACLE` verdict). Exit 0 = holds, 1 = violated, 2 = could not be evaluated: exactly the
  invariant contract above, so a check is one command.

| store | command | records |
|---|---|---|
| MySQL | `mysql <dsn> "<query>"` | one per row, `{"<column>": "<text>" \| null}` |
| PostgreSQL | `postgres <dsn> "<query>"` | the same (booleans `"true"`/`"false"`, times RFC 3339 UTC) |
| Kafka | `kafka <bootstrap> <topic>` | every record of every partition from the log start to the high watermark: `partition, offset, key, value, timestamp_ms`; never creates the topic; a missing topic is "could not read" |
| Redis | `redis <addr> get <key>...` / `scan <pattern>` / `hgetall\|smembers\|lrange\|zrange <key>` / `cmd <word>...` | `{key, value}` (null = missing) / `{key, type, value}` of every matching key of any type / one per field or member |
| etcd | `etcd <endpoint> get <key> [--prefix] [--serializable]` / `status` | `{key, value, create_revision, mod_revision, version}` / `{endpoint, revision, raft_index, raft_term}` (the v3 JSON gateway on the client port) |
| hydo | `hydo acked\|failed\|told-failed\|unknown` | `{id}` from `$HYDO_ACKED`, `$HYDO_FAILED`, failed minus acked, last outcome `unknown` |
| files | `file <path>` / `jsonl <path>` / `range <from> <to>` | `{line}` / saved `hydo-read` output / `{n}` |

DSNs: MySQL `user:pw@tcp(host:port)/db` (the Go driver's form) or `mysql://user@host:port/db`;
PostgreSQL `postgres://user@host:port/db?sslmode=disable`; Redis `host:port` or
`redis://:pw@host:port/db`; `--base64` prints Kafka and etcd keys and values in base64.

Checks (A is `--left` or `--in`, B is `--right`; items are compared as strings):

| check | holds when | typical use |
|---|---|---|
| `subset` | every item of A is in B | every acknowledged order is in the database |
| `equal` | A and B hold the same items | two replicas agree |
| `exactly-once` | every item of A occurs exactly once in B (B as a multiset) | every acknowledged order is on the topic once |
| `disjoint` | no item of A is in B | nothing the client was told failed was applied |
| `no-duplicates` | no item occurs twice in `--in A` | a topic without duplicates |
| `count` | the number of records of `--in A` is `--eq N` / `--min N` / `--max N` | no rows with a wrong amount |
| `never-wrong-value` | every key `--cache C` holds has the value `--truth T` has for it; a key missing from C is fine, a key T does not have is wrong (`--allow-orphans`: fine) | a cache versus the source of truth |

A **source** is one argument: an optional `name=` (used in the explanation), the `hydo-read`
words, and a projection: `--field F` (the item; default the only field, else `key` for
Kafka/Redis/etcd, `line`, `id`, `n`, else a field named `key`), `--item 'T'` (the item is the
template T with `{F}` replaced by field F: `--item 'k:{id}=v{id}'` against `--item '{key}={value}'`
compares keys and values at once), `--key F --value F` (for
`never-wrong-value`; default `key`/`value`), `--strip-prefix P` and `--add-prefix P` (to make
`order:5` and `5`, or `c5` and `5`, compare equal). Quote the source for the shell and the
query inside it with the other kind of quotes:

```yaml
invariants:
  - id: order.in_kafka_once
    msg: every ACKed order is in the Kafka topic exactly once
    cmd: >-
      hydo-check exactly-once --unreadable violation --left 'acked=hydo acked'
      --right 'kafka=kafka 127.0.0.1:9092 orders'
  - id: redis.cache_never_wrong
    type: always_or_unreachable
    cmd: >-
      hydo-check never-wrong-value
      --cache 'redis=redis 127.0.0.1:6379 scan "order:*" --strip-prefix order:'
      --truth 'mysql=mysql hydo:@tcp(127.0.0.1:3306)/ "SELECT id, amount FROM stack.orders" --key id --value amount'
```

The explanation names the first offending item in a fixed order (numbers numerically, then
strings bytewise) and the count, e.g. `disjoint violated: 1 item of told_failed is also in kafka: 11`
or `exactly-once violated: acked c4 is in categories 0 times (of 20 in acked); missing 1: c4`.
Options: `--unreadable violation` turns "could not read a source" into a violation (exit 1) --
for a final check after recovery, where an unreadable store is itself lost data; `--oracle` ends
with a `HYDO-ORACLE: OK|VIOLATION|ERROR <explanation>` line (for a program or script that is not
run by the init); `--id <id> [--type <t>] [--msg <m>]` also prints a `HYDO-ASSERT-DECL` and a
`HYDO-ASSERT` record (`details`: `check`, `first`, `explanation`), on stdout or `--console
/dev/ttyS0`; `--show N` lists up to N offending items (default 5).

**In the test composer.** A check of a composed plan is the same command: a `test.checks` entry
`run: [hydo-check, count, --eq, "5", --in, 'accounts=redis 127.0.0.1:6379 scan "acct:*"']` (argv,
exec'd) or a string, or an executable `hydo/checks/<kind>_<name>` that ends with
`exec hydo-check ...`; exit status and last line mean the same there.
Example: `examples/redis-composed-read/hydo/`.

**Staging.** `hydo-build` copies the binary to `/bin/hydo-read` and adds the `/bin/hydo-check`
symlink only when some command of the manifest calls `hydo-read` or `hydo-check` (descriptions
and `msg:` do not count; `test:` entries count, and so does any file in the plan's `hydo/`
folder that contains the name, a script or a compiled program); every other manifest builds a
byte-identical image. Build the binary
first with `hydo/crates/hydo-read/build.sh` (Rust 1.90.0, static musl, byte-reproducible on a given
build host; it writes `build/hydo-read` for x86_64 and `build/hydo-read-arm64` for `--arch arm64`,
or point `HYDO_READ_BIN` / `HYDO_READ_BIN_ARM64` at them; the Mac release ships the arm64 one as
`guest/hydo-read`); example `hydo/onboard/examples-mac/redis/hydo-read.yaml`. From a program under test (an SDK, not
an invariant), run the same binary with `--id`/`--console` so the record reaches the console.
Examples: `examples/stack/hydo-read.yaml`, `examples/miniflux/hydo-read.yaml`; tests against real
stores: `hydo/crates/hydo-read/native-test.sh`.

### gRPC faults (a fault proxy in front of a gRPC service)

A proxy with `protocol: grpc` (`proxies:`, above) sits between a gRPC client and a gRPC service. It is a streaming HTTP/2
proxy (cleartext HTTP/2 with prior knowledge, what gRPC clients speak without TLS): every call is
forwarded to the service as it arrives, message by message in both directions, so unary,
client-streaming, server-streaming and bidirectional calls all pass, and a fault can act after the
service has answered or in the middle of a stream. The client must dial the proxy's `port:`
(usually an environment variable or flag of the client); the service keeps its own port.

```yaml
proxies:
  - name: orders
    protocol: grpc
    port: 50052                 # the client dials this
    to: 127.0.0.1:50051         # or service: <name> (a service with a port:)
    ref: pb:1                   # the business reference: field 1 of the request message
    routes:                     # optional: name methods, so a fault can target <proxy>.<route>
      - {name: place, path: /hydo.OrderService/Place}
```

Faults are `H=` tokens (`H=<proxy>[.<route>]@<from>-<to>:<rule>[,<rule>...]`, on while workload
steps `[from,to)` run, off when fault injection ends), with these rules on a gRPC proxy:

| rule | what the client sees | did the server act? |
|---|---|---|
| `drop` | no answer; it waits until its deadline (or the fault ends, then its stream is reset) | yes: forwarded, the answer is discarded |
| `status=<code>` | that gRPC status, at once | no (not forwarded) |
| `status=<code>,effect=yes` | that status, after the server has finished | yes |
| `delay=<ms>` | the server's answer, ms of guest time late (past its deadline when ms exceeds it) | yes |
| `reset` (`reset=stream`) | its HTTP/2 stream reset (`INTERNAL`, RST_STREAM) when the server answers | yes |
| `reset=conn` | its TCP connection reset when the server answers: every call on it fails (`UNAVAILABLE`), the client reconnects | yes |
| `cut=<n>` | n response messages, then its stream reset (`dir=req`: n request messages reach the server, then both halves are cut) | partly |
| `hold=<n>` | n messages, then nothing more in that direction until the fault ends (then the stream continues) or the client gives up | partly |

`<code>` is a name (`UNAVAILABLE`, `DEADLINE_EXCEEDED`, `RESOURCE_EXHAUSTED`, `INTERNAL`,
`ABORTED`, or any other gRPC code name) or a number. `effect=no` on drop, reset or delay means the
call is not forwarded at all. `delay=` combines with any other rule (the delay comes first).
Which calls a rule acts on:

- `<proxy>.<route>`, or `rpc=<name>` on the proxy (`Method`, `Service/Method` or `/pkg.Service/Method`): one method;
- `call=<a>` or `call=<a>-<b>`: the a-th (to b-th) call of a method through this proxy, counted from 1
  since the guest booted (every call counts, faulted or not; the recovery boot after a power cut counts again from 1);
- `p=<1..100>` with an optional `seed=<n>`: that share of the matching calls; which ones is a hash of
  the token text and the call's index under the rule, so `seed=` picks another set.

Every decision is a pure function of the token and the order in which calls arrive, which hvx
makes deterministic: a schedule gives the same faults on every run. `rpc=`, `call=`, `cut=`,
`hold=`, `dir=` and `reset=conn` act on gRPC proxies only (hydo-world refuses them elsewhere);
on a gRPC fake (`fakes:` with `protocol: grpc`) select calls by route, with `status=`, `delay=`,
`drop` and `reset`.

**Records.** Each call gives two records in `/tmp/hydo/world.log`: at arrival `kind=req` (as for
every proxy, with `call=<k>`; `fx=1` = forwarded), and at its end

```
kind=call ep=orders route=place path=/hydo.OrderService/Place ref=7 call=8 req=<seq of the req record>
          status=<what the client got> server=<1|0|?> upstream=<the server's status> msgs=<in>/<out> out=fwd+drop
```

`status` is the gRPC status the client received (a number), `rst` (stream reset), `conn`
(connection reset) or `none` (the client gave up before any answer). `server=1`: the server
answered, so it acted; `0`: the call never reached it; `?`: forwarded, no answer from the server.
"The server acted, the client saw a failure" is `status` other than 0 with `server=1`; a history
or side-effect check can join these records with the client's own outcomes by `ref`.
`hydo-world check calls <proxy>[.<route>]` prints the four counts (answered OK; failed after the
server acted; failed before reaching it; failed, server's part unknown) as a HYDO-ORACLE OK line
(it always holds, so it can be an invariant that only reports). The console summary before the
verdict shows each `call` record.

**Explorer.** `hydo explore --grpc-faults` draws up to two such tokens per schedule on the
manifest's gRPC proxies, from its own random stream: without the
flag every schedule string is byte-identical. The shrinker drops a
token, narrows its window, and removes `call=`, `rpc=`, `p=`/`seed=`, `dir=req`, turns
`reset=conn` into `reset` and halves `cut=`/`hold=`. Example: `examples/stack-grpc/`.

**Code.** gRPC proxies are served by `world/grpc.go` (`serveGRPCProxy`, `grpcHandler`), not by
`handler()`; it parses the gRPC-only rule keys (`parseRuleExt`, called from `parseRule`), picks the
rule with the extra filters (`grpcRule`), and implements `check calls`. gRPC fakes still use
`handler()`.

Limits: cleartext HTTP/2 only (no TLS); the client must be pointed at the proxy; a `ref:`/
`idempotency:` taken from the message (`pb:`) reads the call's first message before the decision
(a protobuf field holding its default value, such as an id of 0, is absent, so the ref is empty);
messages up to 64 MiB; compressed messages are passed through and counted, not decoded.

## Error logs as findings (`errors:`, `hydo/known-errors.yaml`)

An invariant checks what you thought of. A service that writes an error line to its log while the
invariants still pass is often the first sign of a bug nobody wrote a check for. With `errors:` in
the manifest, hydo reports every KIND of error line a run produced, compares the kinds with a
reviewed list of expected ones, and reports the new ones as findings of their own.

**In the guest** (`hydo-init-errlog.sh`, spliced into the init only for a manifest with `errors:`;
every other image builds byte for byte as before). Each covered service has its stdout/stderr log
(the init already captures it; a restart's rotation into `<svc>.log.old` is followed), the output
of its `setup:` command, and the files in `errors.files` (MySQL's `log-error=` file, a Java
service's own log file). At the end of every boot -- just before the verdict line, or at a power
cut just before the frozen state is handed over (the services are stopped first) -- the init picks
the error lines, reduces each to a **template** and prints one record per (service, template):

```
HYDO-ERRLOG {"svc":"mysql","src":"/tmp/mysqld.err","n":2,"at":"12,end","tpl":"<time> # [ERROR] [MY-012592] [InnoDB] Operating system error number # in a file operation.","ex":"2026-01-01T00:00:12.345678Z 0 [ERROR] ..."}
HYDO-ERRLOG-SUM {"boot":"first","steps":30,"lines":3,"kinds":2,"shown":2}
```

- Which lines: `format: auto` (the default) takes an upper-case level word (`ERROR`, `FATAL`,
  `CRITICAL`, `PANIC`, `SEVERE`, glog's `ERRO`/`FATA`; MySQL `[ERROR]`, PostgreSQL `ERROR:`,
  log4j/logback), `Error:`/`[Error]`, logfmt and JSON levels (`level=error`, `"level":"error"`),
  bracketed `[error]`, Go's `panic:` and `fatal error:`, and exception lines (`x.y.FooException:
  ...`, Python's `ValueError: ...`) -- except an exception inside the stack trace of an error line
  just counted. `level: warning` adds warning lines. `format: redis` takes Redis's `#` lines (its
  most severe level). `format: none` takes only `match:`. `match:` adds lines, `ignore:` removes
  them (POSIX extended regular expressions, as busybox awk reads them). Lines starting with
  `HYDO-` are never errors.
- The template: user `normalize:` patterns first, then timestamps and dates (`<time>`, `<date>`),
  uuids (`<uuid>`), `0x...` and long hex runs (`<hex>`), long mixed ids (`<id>`), and every other
  number becomes `#` -- except an error code after an upper-case prefix (`MY-012592`,
  `ORA-00942`), a number after `errno`/`code`/`error`/`status`, and a port below the ephemeral
  range after an IPv4 address, `localhost` or a service name (`127.0.0.1:3306` becomes
  `#.#.#.#:3306`; the client side `127.0.0.1:54321` becomes `#.#.#.#:#`). Lines that differ only
  in times, counters, ids and client ports give one template, and so one kind in every run.
- `at`: when the lines were written. `s` = before the workload (boot, provisioning; the same in
  every schedule), a number = the workload range that starts at that step (ranges are split at
  every fault boundary), `end` = after the last step (healing, retry, final invariants), `r` = the
  recovery boot after a power cut.
- Record text has no backslash, double quote or control character (written `/`, `'`, space) and
  no `hydo-`/`HYDO-` (written `hydo_`/`HYDO_`), so a log line can never look like a harness
  marker. At most `errors.max` (default 40) kinds per boot are printed; the summary says how many
  there were.

**On the host** (the hydo-errors crate, used by `hydo explore`, `hydo run`, `hydo repro`
and `hydo errors`). A kind's **signature** is the first 12 hex digits of
`sha256("<service>\n<template>")`. The known list is `hydo/known-errors.yaml` (or
`./known-errors.yaml`; `hydo explore --known-errors FILE`, `hydo run --known-errors FILE`). It is
NOT part of the image: accepting a kind changes no image and no fingerprint, and a run's findings
are a pure function of its consoles, its schedule and the list.

```yaml
format: 1
unknown: report            # report (default): a new kind is a finding of its own
                           # fail: it also turns an OK run into "VIOLATION error-log [svc] <template> (<sig>)"
known:
  - sig: 0c1f2e3d4a5b      # one kind (as `hydo errors list` prints it); or
    match: 'Connection to node # .* could not be established'   # a regular expression on the template
    service: kafka         # optional: only this service's kinds
    with: [C]              # optional: only in runs whose schedule has one of these faults
    during: [K=kafka]      # optional: only for lines written while one of these faults was on
    reason: "Kafka's clients retry while the broker restarts"   # required: why it is expected
    by: user              # who decided (hydo errors accept writes it, and date:)
```

Fault patterns: `K` (any kill), `K=mysql`, `P=redis`, `N=kafka`, `O=`, `X=`, `F=`, `H=payments`,
`C` (a power cut: `C=` or the flush cut `cf=`), `T` (a clock jump), `disk` / `disk=ioerr` (a disk
fault), and knob values such as `R=everysec`. A line is "during" a fault when its range lies
between the step the fault starts and the step it is lifted, inclusive -- so the range right after
a restart or a resume, where the service recovers, is part of it. A power cut or a clock jump
lasts to the end of the run, including the recovery boot; a disk fault covers the workload. A kind
is known when one entry covers every one of its occurrences in that run.

**The explorer** (`hydo explore`) adds, only for images with `errors:`, `errors_new` (signatures)
and `errors_known` to each run in `runs.jsonl`, `consoles/<tag>.errors.json`, an `errors.json` and
a section "Error logs" in `report.md`: each new kind with the number of runs, the first seed and
schedule that produced it, an example line, and -- unless `--no-shrink` -- the smallest schedule
in which the kind is still new (the shrinker that minimises violations, with "the kind is still
new" as its test; at most `--errors-shrink N` kinds, default 8), run twice (both fingerprints, and
whether the kind appeared in both). `--errors fail` (or `unknown: fail`) also makes each such run
a failure, grouped, reproduced and shrunk like a violation. A cached run (`HYDO_EXPLORE_CACHE`) is
compared again with the current list.

**Triage** (both release CLIs):

```
hydo errors list [DIR]                       # the kinds of an exploration (default hydo-out/explore) or a run dir
hydo errors accept 0c1f2e3d4a5b --reason "the startup banner" [--by NAME] [--with C] [--during K=kafka]
                   [--from DIR] [--file hydo/known-errors.yaml]
hydo errors check                            # validate the list
```

`accept` appends one entry per kind (with its template and an example line, for the reader) and
refuses to write a list that does not load. `hydo run` prints the run's new and known kinds and
writes `one.errors.json`.

Limits: the guest scans the logs at the end of each boot, so a kind's lines are counted, not
timed within a range; a log written somewhere not listed in `files:` is not seen; a service that
rotates its own log files by renaming loses the renamed part; multi-line messages count by their
first matching line; the templates are heuristics (a project-specific id that does not look like
a number, hex or a long mixed token needs a `normalize:` pattern).

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
# with a release: hydo build path/to/hydo.yaml, hydo run, hydo explore (the quickstarts).
# In the repository: the hydo-build program (cargo build --release -p hydo-build; it takes the
# command line of the Python tool it replaced); on the build machine tool paths default
# to build-stack.sh's pins
HYDO_SEEDRNG=~/hydo/build/hvx_seedrng hydo-build path/to/hydo.yaml
hydo-build path/to/hydo.yaml --set version=3.51.3   # a build parameter
hydo-build path/to/hydo.yaml --check         # validate only
hydo-build path/to/hydo.yaml --print-shell   # show the compiled manifest
# -> build/explore-ob-<name>-disk.img; copy it to ~/hydo/build on the bare-metal machine, then:
~/bin/hvx-run sudo ./hvx_explore one ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz \
    ~/hydo/build/explore-ob-<name>-disk.img <outdir> "disk=none dseed=1 | run C=12"
hydo-explore --image explore-ob-<name>-disk.img --seeds 1-50 --out <dir>   # cargo build --release -p hydo-explore
```

Guest RAM: the explorer reads `guest.ram_mib` from the manifest inside the image (or from
`--manifest`) and starts the engine with `--ram-mib N`; `hydo explore --ram-mib N` overrides it.
The size is not part of a schedule, so a hand run of a non-default image needs the same flag:
`~/bin/hvx-run sudo ./hvx_explore --ram-mib 2048 one ...` (the report's repro line includes it).
Out-of-range values are rejected by `hydo-build`, the explorer and both engines.

Guest CPU: `guest.cpu: v3` makes the explorer start `hvx_explore --cpu v3` (hvx flag
`HVX_VM_X86_64_V3`): the guest sees AVX/AVX2, FMA, BMI1/2, F16C, LZCNT, MOVBE and XSAVE (x87|SSE|AVX
only, no AVX-512), which prebuilt x86-64-v3 binaries and images built for Haswell/Broadwell need.
`hydo explore --cpu v3|v2` overrides it. Like the RAM size it is not in the schedule, so a hand run
needs `--cpu v3` too (the report's repro line includes it). Without the key nothing changes.

Inside the image: `/etc/hydo/hydo.yaml` (the manifest as written), `/etc/hydo/manifest.sh`
(compiled), `/etc/hydo/prelude.sh` (env + shell helpers), `/sbin/init` (`hydo-init.sh`).

## Limits of v1

- The kernel command line is the explorer's. Guest RAM is per image (`guest.ram_mib`), at most
  3072 MiB on x86_64 (hvx keeps RAM below the 4th-GiB MMIO hole) and 4096 MiB on arm64.
- Images are files on the build machine (no registry pulls); one root per image service, not
  a merged root; no xattrs/capabilities, devices or FIFOs from layers; no zstd layers.
- The guest console is 64 KiB: a workload that prints every acknowledged id should print
  short ids, or the done marker can be lost.
- One data disk, ext4. The workload's step count is the unit of fault placement.

## From Docker Compose

A business stack usually ships a `docker-compose.yaml`. `hydo compose` (or the `hydo-compose`
program, the same options) turns it into a starting `hydo.yaml`, so onboarding begins from
what the team already runs:

```sh
hydo compose docker-compose.yml [--out hydo.yaml] [--images-dir images]
        [--env-file F ...] [--profile P ...] [--use-env] [--no-resolve] [--lock FILE]
        [--network bridge|loopback] [--no-addr]
sh fetch-images.sh            # written next to hydo.yaml: copies every image by digest with hydo-fetch-image (network, once)
# write the workload and the invariants (the generated file marks them TODO), then hydo build
```

The importer needs network only to resolve image tags (it asks the registry directly; anonymous
pulls only, no skopeo). Everything it resolves is written to
`hydo-compose.lock.json`, so a second run needs no network and writes the same file; an image
that `fetch-images.sh` already copied is read from the file instead. It reads the Compose file
as YAML 1.1, as PyYAML's `safe_load` does (anchors, aliases and `<<` merge keys included); the
output of `docker compose config --format json` works as well.
A bare `environment:` entry (`- POSTGRES_USER`) takes its value from `.env`, as in Compose. The generated manifest is checked with hydo-build's own
validator before it is written. Example: `examples/compose-voting/` (Docker's example voting app;
`hydo.generated.yaml` is the raw import, `hydo.yaml` the version with a workload and invariants).

| Compose | hydo.yaml |
|---|---|
| `services.<name>` | `services[]`, in Compose order. The name becomes `[a-z][a-z0-9_]*` (`my-api.v2` -> `my_api_v2`, used by fault tokens); the Compose name still resolves. |
| `image:` | `image: {x86_64: images/<name>-<tag>-amd64.docker.tar, arm64: images/<name>-<tag>-arm64-oci}` + `image_ref: <name>:<tag>`. The tag is resolved to the index digest and the linux/amd64 and linux/arm64 manifest digests (recorded as comments and in the lock file); `fetch-images.sh` copies by those digests. An image given by digest (`name@sha256:...`) is checked against it. |
| `command:`, `entrypoint:` | `cmd:`, `entrypoint:` as lists. A string is split into words as Compose does (no shell); `$$` is `$`. `entrypoint:` drops the image's CMD, as in Docker. |
| `environment:`, `env_file:` | `env:`. env files first (KEY=VALUE, `export`, quotes, comments; `required: false`), then `environment:`. A variable without a value (taken from the shell by Compose) is left out with a warning. |
| `${VAR}` interpolation | done with `.env` next to the Compose file (or `--env-file`): `${VAR:-d}`, `${VAR-d}`, `${VAR:?e}`, `${VAR:+r}`. The shell environment is NOT used (the manifest must not depend on who imports it); a variable found only there is reported. `--use-env` follows Compose. |
| `working_dir:`, `user:` | `workdir:`, `user:` |
| `ports:`, `expose:` | `port:` = the first CONTAINER port (`"8080:80"` -> 80); else the image's `EXPOSE`. Network faults (`N=`/`O=`) act on that port. |
| (each service) | `network: bridge` and its own network namespace with `addr:` 10.89.0.11, .12, ... (see "Service networks"), and `/etc/hosts` (`root.text`, mounted into every image root) maps the Compose service names, `container_name`, `hostname`, network `aliases`, `links` aliases and `extra_hosts` to them (`host-gateway` -> 10.89.0.1). `--network loopback`: the Milestone 84 layout, one shared loopback network with `addr:` 127.0.0.11, .12, ... (link faults then act on nothing, see "Link faults"). `--no-addr`: one shared loopback network, every name -> 127.0.0.1 (the arm64 build, whose kernel has no network namespaces; `hydo compose` on a Mac passes it). |
| `depends_on:` (list or map) | `depends_on:`. hydo starts a service only after its dependencies passed their `ready:`, so `service_healthy` and `service_started` both hold. |
| `depends_on.<job>.condition: service_completed_successfully` | the job service idles (`/.hydo/busybox sleep`) and its command runs once per boot in the `setup:` of every service that waits for it (also before a restart and in crash recovery). |
| `healthcheck:` (else the image's `HEALTHCHECK`) | `ready: timeout <t> hydo-oci <svc> <test>`: `CMD` as argv, `CMD-SHELL`/a string through `/bin/sh -c`, as the service's `user:` (busybox `chpst`), polled every second; `ready_timeout` = `start_period + interval x retries + 60` s (120..600). Without one: `nc -z -w 2 <addr> <port>`, or `true` (with a TODO) when there is no port. |
| `volumes:` named / anonymous, the image's `VOLUME`s | a directory `/data/vol/<name>` on the data disk (the fault target; listed in `data.dirs`), mounted on the target with `copy_up` (on first use it gets the image's content, owner and mode, as a Docker named volume). Several services naming one volume share it. `external: true` starts empty (warning). A volume on `/data` is the data disk's root already. |
| `volumes:` bind mount of an existing file or directory | copied into the read-only root (`root.files`, `/etc/hydo/compose/bind/<n>-<name>`) and mounted on the target; read-only in effect (a warning for a directory not marked `:ro`: if the program writes there, make it a named volume). A source that does not exist becomes an empty data-disk directory (Docker creates it). Host paths `/etc/localtime`, `/etc/timezone`, `/var/run/docker.sock`, `/proc`, `/sys`, `/dev`, ... are skipped with a warning. |
| `tmpfs:`, `type: tmpfs` | a tmpfs mount (mode 1777, `size=` kept) |
| `secrets:` with `file:` | the file, copied into the root, mounted on `/run/secrets/<name>` (or `target:`) |
| (every image service) | `mounts:` also gives it a writable `/run` (a tmpfs with a copy of the image's `/run`) and a 64 MiB `/dev/shm`: a container has a writable layer and its own `/dev/shm`, the hydo image root is read-only. |
| `restart:` | `restart: "no"` -> `recovery: false` (not started again after a power cut, as Docker does not restart it after a reboot); every other policy -> started in crash recovery. hydo restarts every service after a kill fault whatever the policy. |
| `profiles:` | services in a profile are left out unless `--profile <p>` (as `docker compose up`) |
| `mem_limit:`, `deploy.resources.limits.memory` | counted in the `guest.ram_mib` guess |
| (images) | `guest.ram_mib`: 256 MiB + a per-image guess (PostgreSQL 384, MySQL/MongoDB 640, Redis 96, JVM services 1024, others 256), at least 1024, at most 3072. `guest.cpu: v3` when an image's linux/amd64 manifest has variant v3/v4 or the image is known to need AVX2 (restatedev/restate); otherwise a commented line to uncomment if a service stops with "invalid opcode". `data.size_mib` from the volumes. |
| (workload, invariants) | a TODO skeleton that fails loudly (`exit 1` / `exit 2`), so nobody mistakes it for a passing test. The hydo-onboard skill (`hydo guide`, `skills/hydo-onboard`) writes the real ones. |
| `faults:` | all kinds except `freeze` |

Reported as warnings, with what to do (the file's header repeats them): `build:` (with an image:
the image is used; without one the import stops: build it, `docker save` it, set `image:`),
non-default `networks:` (one bridge in the guest: isolation between Compose networks is not
modelled), `network_mode`, `privileged`, `cap_add`/`cap_drop`, `devices`, `security_opt`,
`pid`/`ipc`/`uts`/`userns_mode`, `sysctls`, `ulimits`, `dns*`, `init`, `volumes_from`, `extends`,
`configs`, `scale`, `deploy.replicas` > 1 and other `deploy:` (Swarm) keys, `shm_size`, GPU
reservations, `platform:` other than linux/amd64 or linux/arm64, an `include:`. Ignored silently:
`labels`, `logging`, `stdin_open`, `tty`, `stop_signal`, `stop_grace_period`, CPU shares and quotas,
`x-*` extension fields. With `--network loopback` or `--no-addr`, a **port collision** -- two
services listening on the same container port, which Compose keeps apart with one network namespace
per container -- is reported with each service's `addr:`: on one shared network only the first one
that binds `0.0.0.0:<port>` starts, so make each listen on its own address or move one
(`examples/compose-voting/hydo-loopback.yaml` moves `result` to 81). With `network: bridge` (the
default) there is no collision: each service has its own namespace.

### `services[].mounts` (image services)

The importer writes these; they can be written by hand too. Each entry is made by the image
service's launcher before every start and every `hydo-oci` call (once per boot, under a lock):

```yaml
    mounts:
      - {src: /etc/hosts, dst: /etc/hosts}                              # a file or dir of the guest
      - {src: /data/vol/pgdata, dst: /var/lib/postgresql/data, copy_up: true}   # a volume
      - {tmpfs: true, dst: /run, copy_up: true}                          # a writable copy of /run
      - {tmpfs: true, dst: /dev/shm, size: 64m}                          # an empty tmpfs, mode 1777
```

`src:` is a path of the guest (the hydo root, or under `/data`); `copy_up: true` on a `src:` fills
an EMPTY directory from the image's `dst` (content, owner, mode) before mounting it; on a tmpfs it
copies the image's directory in. A `dst` the image lacks is created in the image root at build time
(below `/proc`, `/sys`, `/dev`, `/tmp`, `/data` at run time); a `dst` that is a symlink in the image
is resolved inside the image root, never through to the guest's own directory. Paths use
`[A-Za-z0-9._@+=,:/-]` only. An image service without `mounts:` gets exactly the launcher it had
before.

### What a Compose stack does not get

- One bridge for every Compose network (no isolation between networks), one hostname (`hostname`
  inside a service is the guest's) and one PID namespace (services see each other's processes).
  Each service has its own network namespace only with `network: bridge`, the importer's default
  (on a Mac too, with the second guest kernel); with `--network loopback` or `--no-addr` link
  faults between services act on nothing.
- The image roots are read-only except for the `mounts:`: a program that writes elsewhere fails
  with "Read-only file system"; add a `mounts:` entry (a volume or a tmpfs) for that path.
- Healthchecks run through `hydo-oci`, as root unless the service has a `user:`; the image's own
  user for a healthcheck (Docker runs it as the container's user) is not looked up.
- No `build:`, no registry pulls at build time, no Swarm (`deploy:`), no GPUs or host devices.

## gRPC services: generated drivers (`hydo-grpc-gen`)

A system whose inputs are gRPC endpoints needs no hand-written workload client.
`hydo-grpc-gen` reads the service definitions and writes a driver for every RPC: one static
client binary, `hydo-grpc`, with one subcommand per job, plus the project's schema and a plan.
The client sends valid random requests from a seeded stream, records every call's outcome, and
retries the way a careful client does. Source: `hydo/grpcgen/` (Go, the pinned toolchain).

```sh
hydo/onboard/hydo-grpc-gen <proto dir> [--hints hints.yaml] --out <dir>   # every .proto under the dir
hydo/onboard/hydo-grpc-gen --descriptor-set set.pb [--hints ...] --out <dir>   # protoc -o / buf build -o
hydo/onboard/hydo-grpc-gen --reflect 127.0.0.1:7070 [--hints ...] --out <dir> # a running server's reflection
#   -I <dir>      extra .proto import path (repeatable)    --target host:port   the address in the guest
#   --arch amd64,arm64   also build hydo-grpc.arm64        --no-build           only the schema and plan
#   --steps N     workload.steps in the starter section     --rel <dir>          base of its relative paths
```

It writes, under `<dir>`:

| file | what |
|---|---|
| `grpc/desc.pb` | the schema: a `FileDescriptorSet`, dependencies first, no source info, deterministic bytes |
| `grpc/plan.json` | every RPC with its hints, checked against the schema (unknown fields, wrong types and bad paths are errors) |
| `hydo-grpc` | the static guest client for linux/amd64 (`-trimpath`, no build id: byte-identical from the same sources and Go); with `--arch amd64,arm64` also `hydo-grpc.arm64` for the Mac backend (example `hydo/grpcgen/demo/hydo-mac.yaml`) |
| `workload/parallel_driver_<rpc>` | one test-composer command per RPC with a weight (and per scenario), in the `hydo/workload/` layout, with a `# hydo: timeout=` header |
| `hydo.grpc.yaml` | a starter section to merge into `hydo.yaml`: files, `env:`, a `test:` section with the commands, `workload.retry`, and the numbered-steps `workload:` as a commented alternative |

`hydo/onboard/hydo-grpc-gen` builds the generator for the host and the client with `$HYDO_GO`, else
the pinned `go-1.26.5` of `build-stack.sh` if present, else `go` on PATH. Put `grpc/` at
`/etc/hydo/grpc` and the client at `/bin/hydo-grpc` (the starter section does).

### What the client does

| command | use |
|---|---|
| `hydo-grpc steps $HYDO_FROM $HYDO_TO` | `workload.run`: each step runs `steps.calls` operations on each of `steps.concurrency` workers, the RPC (or scenario) drawn by weight |
| `hydo-grpc retry` | `workload.retry`: re-sends, with the identical request, every operation whose last outcome was unknown and whose RPC has `retry` |
| `hydo-grpc drive <rpc\|scenario> [N]` | one composer command: `drive.calls` (default 2, or N) operations; exit 0 if all ended ok, 1 if one ended failed or unknown (the composer records a failed operation; it never exits 100) |
| `hydo-grpc call <rpc> ['<json>'\|-]` | one call; prints the response as JSON; exit 0 ok, 1 definite failure, 2 unknown -- the invariant convention, for read-backs |
| `hydo-grpc gen <rpc> [N]` / `list` | print generated requests / the plan's RPCs |

Unary, server-streaming, client-streaming (`stream_len` messages) and bidirectional RPCs are
supported. Every attempt has a deadline (`deadline_ms`). An operation whose outcome is unknown is
retried at once with the same request up to `retry.attempts` times; a definite failure never is.

**Outcomes.** The gRPC status decides: OK is ok; INVALID_ARGUMENT, NOT_FOUND, ALREADY_EXISTS,
PERMISSION_DENIED, UNAUTHENTICATED, FAILED_PRECONDITION, OUT_OF_RANGE, ABORTED, UNIMPLEMENTED are
definite failures (the server rejected the request before acting); DEADLINE_EXCEEDED,
UNAVAILABLE, CANCELLED, UNKNOWN, INTERNAL, DATA_LOSS and RESOURCE_EXHAUSTED are unknown (the
request may have taken effect: a deadline passed while the server worked on, a connection
reset, a crash half-way). A non-status error is unknown. `classify:` overrides a code per RPC
for an API that documents a stronger promise.

**Records.** Every attempt is two lines in the history recorder's format (`HYDO-HIST`,
`hydo/HISTORIES.md`), history `grpc` (`record.history:`), in `/tmp/hydo/grpc/calls`
(`$HYDO_GRPC_CALLS`) with the bodies, and on the guest console without them (`record.console:
brief`, the default; `full` or `off`), so a few hundred calls fit in the 64 KiB console:

```
HYDO-HIST grpc <process> invoke <time> <rpc> <op> <request JSON>
HYDO-HIST grpc <process> ok|fail|info <time> <rpc> <op> <CODE> <response JSON or status message>
HYDO-HIST grpc 0 invoke 13472685048 orders.v1.Orders/Create 11 {"request_id":"r11","customer":"bob",...}
HYDO-HIST grpc 0 info 14474532744 orders.v1.Orders/Create 11 DEADLINE_EXCEEDED context%20deadline%20exceeded
```

`<process>` is the worker (steps mode) or the OS pid (a composer command), with one operation in
flight; the operation `<f>` is the RPC; `<op>` the operation id, the same for its retries (each
retry is a new invocation with the same request); `<time>` guest nanoseconds since boot
(CLOCK_BOOTTIME, deterministic under hydo); bodies are compact protobuf JSON (proto field names);
every token is percent-encoded (`%20`, `%40` for `@`; `%` alone is empty). `hydo/history`'s parser
reads them; no ready-made checker has a model for arbitrary RPCs, so a `checks:` entry that names
history `grpc` needs RPCs that are reads and writes of a register (a future converter), and
otherwise the records are for reading a failing run. An RPC with `outcome_id:` also writes the
value of that request field to `$HYDO_ACKED` (ok) and `<id> ack|failed|unknown` to
`$HYDO_OUTCOMES`, the files invariants read. The first ok / definite failure / unknown outcome of
each RPC in a run is also a `HYDO-ASSERT` record of type `reachable` (`grpc.<rpc>.ok`, `.failed`,
`.unknown`).

**Determinism.** A request is a pure function of the seed and of where it runs -- under the
test composer the entry's `HYDO_SEED` (so the schedule token `W=`, which the explorer draws and
shrinks, decides the requests as well as the mix); in steps mode the step and the worker
(stream `splitmix64(seed, step, worker)`) -- plus values taken from earlier responses and a
run-wide counter for unique values; never host time. The seed is `--seed`, else
`$HYDO_GRPC_SEED` (the starter section sets it in `env:`), else the file `/tmp/hydo/grpc/seed`,
else `hydo_grpc_seed=` on the kernel command line, else the hints' `seed`.

### The hints file

Everything is optional; unknown keys are errors.

```yaml
target: 127.0.0.1:7070      # the server (default 127.0.0.1:50051); per RPC: target:
deadline_ms: 1000           # per-attempt deadline (default 2000); per RPC: deadline_ms:
seed: 1
services: [orders.v1.Orders]                 # only these services (default: every one)
steps: {calls: 2, concurrency: 1}            # per step: operations per worker, workers in parallel
drive: {calls: 4}
record: {console: brief, history: grpc}      # console: off | brief | full
defaults:                                    # where a field has no hint
  string_len: [1, 10]   bytes_len: [0, 16]   repeated_len: [0, 3]   map_len: [0, 2]
  depth: 4              message_prob: 0.85   optional_prob: 0.7     oneof_none: 0
rpcs:                       # key: Method, Service/Method or the full name
  Create:
    weight: 3               # relative frequency (default 1; 0 = never drawn)
    retry: {attempts: 4, backoff_ms: 100, at_end: true, end_tries: 8}
    outcome_id: request_id  # a single scalar request field
    classify: {ABORTED: info}                # code -> ok | fail | info
    oneofs: {payment: [card]}                # allowed members of a oneof
    stream_len: [1, 3]                       # client-streaming: messages per call
    fields:                 # request field paths; a repeated field's hint applies to each element;
                            # maps: <field>.key / <field>.value
      request_id: {unique: true, prefix: r}  # r1, r2, ... (an integer: range lo, lo+1, ...)
      customer: {choice: [alice, bob]}       # also enum value names, numbers, true/false
      amount_cents: {range: [1, 50000]}      # numbers, inclusive
      items: {len: [1, 3]}                   # strings/bytes: length; repeated/map: entries
      items.sku: {format: digits, len: [3, 3], prefix: sku-}   # alnum lower digits hex uuid
      note: {prob: 0.5}                      # presence of an optional / message field
      page_token: {omit: true}
      region: {value: eu-1}                  # a constant
  Cancel:
    fields:
      id: {from: Create.id}  # a value an earlier call returned: <rpc>.<path> (response),
                             # <rpc>.request.<path>; or {rpc:, path:, side:, pick: random|latest,
                             # fallback: skip|generate}. skip: the call waits until one exists.
scenarios:                   # optional fixed sequences, drawn like an RPC
  - {name: create_cancel, weight: 1, calls: [Create, Get, Cancel]}   # `from` takes this worker's latest
```

Without hints every value is still valid for its type: integers mostly small and non-negative,
with the edges (0, 1, the maximum) and a few negatives; finite floats; lowercase alphanumeric
strings; declared enum values without `*_UNSPECIFIED`; one member of each oneof; distinct map
keys; proto2 `required` fields always set; nesting bounded by `depth`; Timestamp in the 30 days
after 2026-01-01, Duration up to an hour, empty Any/Struct. What a server rejects for business
reasons (an amount of 0, an unknown id) needs a hint: those calls are recorded as definite
failures, which is valid input but rarely the interesting part.

Values for `from` are kept in pools under `/tmp/hydo/grpc/pool/` (request fields after an ok or
unknown outcome, response fields after an ok), shared by the processes of one run.

### With the test composer, or numbered steps

The starter section uses the test composer (MANIFEST "The test composer"): one
`parallel_driver` per RPC with a weight (`run: [/bin/hydo-grpc, drive, <rpc>]`, a `timeout:`
that bounds `drive.calls` operations whose every attempt runs into its deadline) and per
scenario, the same commands as files in `<out>/workload/` for a `hydo/` folder, and
`workload.retry: /bin/hydo-grpc retry` (the generic init runs it after the faults heal; the
composer leaves `workload.retry` alone). The composer draws commands uniformly, so `weight:` only
matters in steps mode; give an RPC weight 0 to leave out its command. Without the composer, the
commented `workload:` in the starter section runs `hydo-grpc steps` per step range; a plan
whose invariants need one operation per step (the stack's ids 0..29) uses that mode.

### Example and evidence

`hydo/grpcgen/demo/`: an order service (`orders.v1.Orders`: Create, Get, Cancel, List) on Redis
with one planted bug, driven only by generated drivers under the test composer: `proto/`, `hints.yaml`, `hydo.yaml`
(`params: mode: buggy|fixed`), `build.sh`. The stack's front door has a `.proto` too
(`hydo/stack/proto/hydo/order.proto`, `hydo/stack/grpc-hints.yaml`), and
`examples/stack/hydo-grpcgen.yaml` is the stack manifest with the generated driver in place of
`hydostack place/retry` (numbered steps). Results: `docs/HISTORY.md`, Milestone 91.

Limits: a server that needs TLS, auth metadata or custom headers is not supported (plain-text
HTTP/2 only); `from` carries scalar and enum values, not whole messages; values a request needs
from another system (a payment token) must come from hints.
