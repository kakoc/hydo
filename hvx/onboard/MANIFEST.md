# hydo.yaml — the project manifest

A project is onboarded by writing ONE file, `hydo.yaml`. `hvx/onboard/hydo-build` compiles it
into a guest image, and one generic guest init (`hvx/onboard/hydo-init.sh`) runs any manifest:
it starts the services, runs the workload under the explorer's faults and checks the
invariants. No per-project build script or boot script is written by hand.

This file is the reference for people and for tools that generate manifests (a Claude skill
that reads a repository and writes `hydo.yaml` is the intended next step). Complete
examples: `examples/redis/hydo.yaml` (one service, about 60 lines),
`examples/stack/hydo.yaml` (MySQL + Kafka + Redis + a gRPC front door),
`examples/redis-oci/hydo.yaml` (Redis from the official Docker image, no file list),
`examples/etcd/hydo.yaml` (a replicated system: a three-member etcd cluster, with link faults
between members), and
`hvx/knownbugs/sqlite/hydo.yaml` (no service: a multi-threaded program using SQLite; one
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
  cpu: v3                      # guest CPU level (x86_64 only): v2 (default) = x86-64-v2, no AVX;
                               # v3 = adds AVX/AVX2, FMA, BMI1/2, F16C, LZCNT, MOVBE, XSAVE

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
                                          #   faults between services (see "Link faults")
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

faults: [kill, pause, network, oneway, cut, clock, disk]   # which kinds make sense here;
                               # network/oneway also pull iproute2 into the image
```

### Services from a Docker/OCI image

A service with `image:` takes its files from an image instead of `root.nix`/`root.files`.
hydo-build never pulls: the image is a file on the build machine. Make one with any of

```sh
docker save -o redis.tar redis:7.2.4                       # Docker (classic or >= 25 format)
skopeo copy docker://docker.io/library/redis@sha256:<manifest digest> \
       docker-archive:redis.tar:redis:7.2.4                # skopeo, a docker-save style tar
skopeo copy docker://docker.io/library/redis@sha256:<digest> oci:redis-oci:redis:7.2.4
                                                           # skopeo, an OCI layout directory
```

(gzip-compressed tars and OCI layouts holding a multi-platform index work too; the build picks
the `linux/<arch>` manifest. zstd layers are not supported by the build box's Python: re-export
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
then inserts `hvx/onboard/hydo-liveness.sh` into `/sbin/init` before its `main` section, with
renamed copies of the four init functions it wraps (`step_range`, `start_periodic`,
`start_all`, `finish`). Every other image's `/sbin/init` is `hydo-init.sh` byte for byte.
`hydo-build --print-init` shows the generated init. Example: `examples/redis/hydo-live.yaml`.

The program under test can still use the assertion SDK (`hvx/stack/hydoassert`) for
properties that need to see inside it; both kinds of record mix on the console.

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
| `<X>=<value>` | a knob from `knobs:` |
| host side: `disk=ioerr:<p>` / `lying:<p>` | disk I/O errors / a disk that drops acknowledged writes |

Lifting a network fault (`N=`/`O=`, at `to` or at the end of the workload) does not drop
the packets still waiting in netem's delay queue: the init first changes the netem to no
delay/loss/duplication/corruption (queued packets keep their send times, new packets follow
them in order), waits until the queue is empty (at most the largest delay in force plus
500 ms of guest time), then removes the qdisc. The console shows
`network fault lifted: queued=<n> drained_ms=<ms> left=<n>`. Before 2026-10-23 the qdisc was
deleted at once and every queued packet was lost, so TCP retransmitted after a timeout.

A token naming a service the manifest does not have is ignored. `explore.py` draws its
faults from the manifest (`hvx/explore/manifest_space.py`): kills and pauses from all
services, network and one-way faults from services with a `port:`, knob values from
`knobs:`, fault placement from `workload.steps`, fault kinds from `faults:`. It reads
`/etc/hydo/hydo.yaml` out of an `explore-ob-*` image automatically; pass `--manifest` for
any other image. A knob token that collides with a built-in token (K P N O C T Q X) is
rejected.

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

With two or more services that have `addr:`, `explore.py` (manifest_space.py) adds every pair
`a-b`, and with three or more every `a-*`, to the targets its network and one-way faults pick
from. The extension lives in `hydo-init-links.sh`; hydo-build copies it into the image and adds one
line to `/sbin/init` that sources it, only for manifests with an `addr:`, so every other
manifest builds a byte-identical image. Not supported yet: `--arch arm64`.

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

Guest RAM: `explore.py` reads `guest.ram_mib` from the manifest inside the image (or from
`--manifest`) and starts the engine with `--ram-mib N`; `explore.py --ram-mib N` overrides it.
The size is not part of a schedule, so a hand run of a non-default image needs the same flag:
`~/bin/hvx-run sudo ./hvx_explore --ram-mib 2048 one ...` (the report's repro line includes it).
Out-of-range values are rejected by `hydo-build`, `explore.py` and both engines.

Guest CPU: `guest.cpu: v3` makes `explore.py` start `hvx_explore --cpu v3` (hvx flag
`HVX_VM_X86_64_V3`): the guest sees AVX/AVX2, FMA, BMI1/2, F16C, LZCNT, MOVBE and XSAVE (x87|SSE|AVX
only, no AVX-512), which prebuilt x86-64-v3 binaries and images built for Haswell/Broadwell need.
`explore.py --cpu v3|v2` overrides it. Like the RAM size it is not in the schedule, so a hand run
needs `--cpu v3` too (the report's repro line includes it). Without the key nothing changes.

Inside the image: `/etc/hydo/hydo.yaml` (the manifest as written), `/etc/hydo/manifest.sh`
(compiled), `/etc/hydo/prelude.sh` (env + shell helpers), `/sbin/init` (`hydo-init.sh`).

## Limits of v1

- No OCI/Docker import yet (`image:` is reserved); files come from store paths or the host.
- The kernel command line is the explorer's. Guest RAM is per image (`guest.ram_mib`), at most
  3072 MiB on x86_64 (hvx keeps RAM below the 4th-GiB MMIO hole) and 4096 MiB on arm64.

- Images are files on the build machine (no registry pulls); one root per image service, not
  a merged root; no xattrs/capabilities, devices or FIFOs from layers; no zstd layers.
- Guest RAM is fixed at 1024 MiB and the kernel command line is the explorer's.
- The guest console is 64 KiB: a workload that prints every acknowledged id should print
  short ids, or the done marker can be lost.
- One data disk, ext4. The workload's step count is the unit of fault placement.
